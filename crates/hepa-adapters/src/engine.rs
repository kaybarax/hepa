use crate::spec::{
    HepaAdapterMode, HepaAdapterOutputCapture, HepaAdapterPromptTransport, HepaAdapterRole,
    HepaAdapterSpec, HepaAdapterTemplateContext,
};
use hepa_core::monitor::{HepaMonitorPolicy, HepaMonitorStop, HepaMonitorStopKind};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaOneshotAdapterInvocation {
    pub spec: HepaAdapterSpec,
    pub role: HepaAdapterRole,
    pub context: HepaAdapterTemplateContext,
    pub prompt: String,
    pub environment: BTreeMap<String, String>,
    pub monitor_policy: HepaMonitorPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaOneshotAdapterResult {
    pub adapter_id: String,
    pub role: HepaAdapterRole,
    pub command: String,
    pub workdir: PathBuf,
    pub allowed_env_keys: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HepaOneshotAdapterExecutor;

impl HepaOneshotAdapterExecutor {
    pub fn new() -> Self {
        Self
    }

    pub fn run(
        &self,
        invocation: &HepaOneshotAdapterInvocation,
    ) -> Result<HepaOneshotAdapterResult, HepaAdapterExecutionError> {
        invocation
            .spec
            .validate()
            .map_err(|error| HepaAdapterExecutionError::new("spec", error.to_string()))?;
        if invocation.spec.mode != HepaAdapterMode::Oneshot {
            return Err(HepaAdapterExecutionError::new(
                "mode",
                "executor only supports oneshot adapters",
            ));
        }
        if !invocation.spec.roles.contains(&invocation.role) {
            return Err(HepaAdapterExecutionError::new(
                "role",
                "adapter does not support requested role",
            ));
        }

        let command = rendered_command(invocation)?;
        reject_unsupported_hepa_flags(&command)?;
        invocation
            .monitor_policy
            .check_command(&command)
            .map_err(monitor_error)?;
        let argv = split_command_line(&command)?;
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| HepaAdapterExecutionError::new("command", "must not be empty"))?;
        let mut args = args.to_vec();
        if invocation.spec.prompt_transport == HepaAdapterPromptTransport::PromptArg {
            args.push(invocation.prompt.clone());
        }
        let workdir = rendered_workdir(invocation)?;
        if !workdir.is_dir() {
            return Err(HepaAdapterExecutionError::new(
                "workdir",
                "lane worktree must exist before adapter launch",
            ));
        }
        if invocation.spec.prompt_transport == HepaAdapterPromptTransport::PromptFile {
            write_prompt_file(invocation)?;
        }

        let filtered_env = filtered_environment(invocation);
        let mut child = Command::new(program);
        child.args(&args).current_dir(&workdir).env_clear().envs(
            filtered_env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
        child.stdout(Stdio::piped()).stderr(Stdio::piped());
        if invocation.spec.prompt_transport == HepaAdapterPromptTransport::Stdin {
            child.stdin(Stdio::piped());
        }
        let mut child = child.spawn().map_err(|error| {
            HepaAdapterExecutionError::new("command", format!("failed to spawn adapter: {error}"))
        })?;
        if invocation.spec.prompt_transport == HepaAdapterPromptTransport::Stdin {
            use std::io::Write;
            let mut stdin = child.stdin.take().ok_or_else(|| {
                HepaAdapterExecutionError::new("stdin", "failed to open adapter stdin")
            })?;
            stdin
                .write_all(invocation.prompt.as_bytes())
                .map_err(|error| {
                    HepaAdapterExecutionError::new(
                        "stdin",
                        format!("failed to write prompt to adapter stdin: {error}"),
                    )
                })?;
            stdin.flush().map_err(|error| {
                HepaAdapterExecutionError::new(
                    "stdin",
                    format!("failed to flush prompt to adapter stdin: {error}"),
                )
            })?;
            drop(stdin);
        }
        let output = wait_with_monitor(child, &invocation.monitor_policy)?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        invocation
            .monitor_policy
            .check_output(&stdout)
            .and_then(|_| invocation.monitor_policy.check_output(&stderr))
            .map_err(monitor_error)?;
        if invocation.spec.output_capture == HepaAdapterOutputCapture::Stdout {
            write_output_artifact(invocation, &stdout)?;
        }

        Ok(HepaOneshotAdapterResult {
            adapter_id: invocation.spec.id.clone(),
            role: invocation.role.clone(),
            command,
            workdir,
            allowed_env_keys: filtered_env.keys().cloned().collect(),
            exit_code: output.status.code(),
            stdout,
            stderr,
        })
    }
}

fn write_output_artifact(
    invocation: &HepaOneshotAdapterInvocation,
    output: &str,
) -> Result<(), HepaAdapterExecutionError> {
    let output_path = match invocation.role {
        HepaAdapterRole::Worker => PathBuf::from(&invocation.context.output_file),
        HepaAdapterRole::Reviewer => PathBuf::from(&invocation.context.review_output_file),
    };
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            HepaAdapterExecutionError::new(
                "output_file",
                format!("failed to create parent: {error}"),
            )
        })?;
    }
    fs::write(&output_path, output).map_err(|error| {
        HepaAdapterExecutionError::new("output_file", format!("failed to write output: {error}"))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaAdapterExecutionError {
    pub field: String,
    pub message: String,
    pub status: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

impl HepaAdapterExecutionError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            status: None,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn blocked(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            status: Some("blocked".to_string()),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn with_output(mut self, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        self.stdout = String::from_utf8_lossy(&stdout).to_string();
        self.stderr = String::from_utf8_lossy(&stderr).to_string();
        self
    }

    pub fn has_output(&self) -> bool {
        !self.stdout.is_empty() || !self.stderr.is_empty()
    }
}

impl HepaAdapterExecutionError {
    pub fn sanitized_summary(&self) -> String {
        if self.has_output() {
            format!(
                "{}: {}; captured stdout={} bytes stderr={} bytes",
                self.field,
                self.message,
                self.stdout.len(),
                self.stderr.len()
            )
        } else {
            self.to_string()
        }
    }
}

impl fmt::Display for HepaAdapterExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaAdapterExecutionError {}

fn wait_with_monitor(
    mut child: std::process::Child,
    policy: &HepaMonitorPolicy,
) -> Result<std::process::Output, HepaAdapterExecutionError> {
    let stdout = child.stdout.take().ok_or_else(|| {
        HepaAdapterExecutionError::new("stdout", "failed to capture adapter stdout")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        HepaAdapterExecutionError::new("stderr", "failed to capture adapter stderr")
    })?;
    let stdout_reader = spawn_output_reader(stdout);
    let stderr_reader = spawn_output_reader(stderr);
    let started_at = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| HepaAdapterExecutionError::new("command", error.to_string()))?
        {
            let stdout = receive_output_reader("stdout", &stdout_reader)?;
            let stderr = receive_output_reader("stderr", &stderr_reader)?;
            return Ok(std::process::Output {
                status,
                stdout,
                stderr,
            });
        }
        let elapsed_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        if let Err(stop) = policy.check_elapsed(elapsed_ms) {
            let _ = child.kill();
            let _ = child.wait();
            let stdout = receive_output_reader_snapshot(&stdout_reader);
            let stderr = receive_output_reader_snapshot(&stderr_reader);
            return Err(monitor_error(stop).with_output(stdout, stderr));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct OutputReader {
    bytes: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Receiver<Result<(), std::io::Error>>,
}

fn spawn_output_reader<R>(mut stream: R) -> OutputReader
where
    R: Read + Send + 'static,
{
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let thread_bytes = Arc::clone(&bytes);
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = [0_u8; 8192];
        let result = loop {
            match stream.read(&mut chunk) {
                Ok(0) => break Ok(()),
                Ok(count) => {
                    thread_bytes
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .extend_from_slice(&chunk[..count]);
                }
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send(result);
    });
    OutputReader {
        bytes,
        done: receiver,
    }
}

fn receive_output_reader(
    stream: &'static str,
    reader: &OutputReader,
) -> Result<Vec<u8>, HepaAdapterExecutionError> {
    reader
        .done
        .recv()
        .map_err(|_| HepaAdapterExecutionError::new(stream, "reader thread disconnected"))?
        .map_err(|error| {
            HepaAdapterExecutionError::new(
                stream,
                format!("failed to read adapter {stream}: {error}"),
            )
        })?;
    Ok(reader
        .bytes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone())
}

fn receive_output_reader_snapshot(reader: &OutputReader) -> Vec<u8> {
    let _ = reader.done.recv_timeout(Duration::from_millis(100));
    reader
        .bytes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn write_prompt_file(
    invocation: &HepaOneshotAdapterInvocation,
) -> Result<(), HepaAdapterExecutionError> {
    let prompt_path = match invocation.role {
        HepaAdapterRole::Worker => PathBuf::from(&invocation.context.prompt_file),
        HepaAdapterRole::Reviewer => PathBuf::from(&invocation.context.review_prompt_file),
    };
    if let Some(parent) = prompt_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            HepaAdapterExecutionError::new(
                "prompt_file",
                format!("failed to create parent: {error}"),
            )
        })?;
    }
    fs::write(&prompt_path, &invocation.prompt).map_err(|error| {
        HepaAdapterExecutionError::new("prompt_file", format!("failed to write prompt: {error}"))
    })
}

fn monitor_error(stop: HepaMonitorStop) -> HepaAdapterExecutionError {
    let reason = match stop.kind {
        HepaMonitorStopKind::CommandPolicy => "command_policy",
        HepaMonitorStopKind::SecretDetected => "secret_detected",
        HepaMonitorStopKind::ScopeViolation => "scope_violation",
        HepaMonitorStopKind::SuspiciousPath => "suspicious_path",
        HepaMonitorStopKind::Timeout => "timeout",
        HepaMonitorStopKind::Stall => "stall",
    };
    HepaAdapterExecutionError::blocked("monitor", format!("{reason}: {}", stop.evidence))
}

fn rendered_command(
    invocation: &HepaOneshotAdapterInvocation,
) -> Result<String, HepaAdapterExecutionError> {
    match invocation.role {
        HepaAdapterRole::Worker => invocation.spec.render_worker_command(&invocation.context),
        HepaAdapterRole::Reviewer => invocation
            .spec
            .render_review_command(&invocation.context)
            .and_then(|command| {
                command.ok_or_else(|| crate::spec::HepaAdapterTemplateError {
                    placeholder: "review_command".to_string(),
                    message: "review command is required for reviewer role".to_string(),
                })
            }),
    }
    .map_err(|error| HepaAdapterExecutionError::new("command", error.to_string()))
}

fn reject_unsupported_hepa_flags(command: &str) -> Result<(), HepaAdapterExecutionError> {
    let unsupported_flags = crate::doctor::unsupported_hepa_flags(command);
    if unsupported_flags.is_empty() {
        Ok(())
    } else {
        Err(HepaAdapterExecutionError::blocked(
            "invocation_template",
            format!(
                "unsupported HEPA-composed flag(s): {}",
                unsupported_flags.join(",")
            ),
        ))
    }
}

fn rendered_workdir(
    invocation: &HepaOneshotAdapterInvocation,
) -> Result<PathBuf, HepaAdapterExecutionError> {
    crate::spec::render_command_template(&invocation.spec.workdir, &invocation.context)
        .map(PathBuf::from)
        .map_err(|error| HepaAdapterExecutionError::new("workdir", error.to_string()))
}

fn filtered_environment(invocation: &HepaOneshotAdapterInvocation) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }
    env.insert("HEPA_ADAPTER_ID".to_string(), invocation.spec.id.clone());
    env.insert(
        "HEPA_ADAPTER_ROLE".to_string(),
        role_name(&invocation.role).to_string(),
    );
    for key in &invocation.spec.required_env {
        if is_manager_only_env_key(key) {
            continue;
        }
        if let Some(value) = invocation.environment.get(key) {
            env.insert(key.clone(), value.clone());
        }
    }
    for (key, value) in &invocation.environment {
        if key.starts_with("PI_") {
            env.insert(key.clone(), value.clone());
        }
    }
    env
}

fn is_manager_only_env_key(key: &str) -> bool {
    key == ["GITHUB", "TOKEN"].join("_")
        || key.starts_with("HEPA_MANAGER_")
        || key.starts_with("MANAGER_")
}

fn role_name(role: &HepaAdapterRole) -> &'static str {
    match role {
        HepaAdapterRole::Worker => "worker",
        HepaAdapterRole::Reviewer => "reviewer",
    }
}

fn split_command_line(command: &str) -> Result<Vec<String>, HepaAdapterExecutionError> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for character in command.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else {
                current.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            value if value.is_whitespace() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            value => current.push(value),
        }
    }
    if escaped {
        current.push('\\');
    }
    if quote.is_some() {
        return Err(HepaAdapterExecutionError::new(
            "command",
            "unterminated quoted argument",
        ));
    }
    if !current.is_empty() {
        parts.push(current);
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{
        ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterOutputCapture,
        HepaAdapterPromptTransport, HepaAdapterSandbox,
    };
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn oneshot_executor_spawns_cli_in_lane_worktree_with_filtered_env() {
        let root = unique_test_dir("oneshot-success");
        let worktree = root.join("lane-worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let script = root.join("fake-adapter");
        write_executable(
            &script,
            r#"#!/bin/sh
pwd > "$2"
env | sort >> "$2"
printf 'worker stdout'
printf 'worker stderr' >&2
"#,
        );
        let output_file = artifact_dir.join("worker-env.txt");
        let invocation = HepaOneshotAdapterInvocation {
            spec: adapter_spec(
                "worker-primary",
                vec![HepaAdapterRole::Worker],
                format!("{} --output {{output_file}}", script.display()),
                None,
                vec!["ALLOWED_CONTEXT".to_string()],
            ),
            role: HepaAdapterRole::Worker,
            context: template_context(&worktree, &artifact_dir, &output_file),
            prompt: "Worker prompt from task spec".to_string(),
            environment: BTreeMap::from([
                ("ALLOWED_CONTEXT".to_string(), "visible".to_string()),
                ("UNLISTED_CONTEXT".to_string(), "hidden".to_string()),
            ]),
            monitor_policy: HepaMonitorPolicy::default(),
        };

        let result = HepaOneshotAdapterExecutor::new()
            .run(&invocation)
            .expect("adapter should run");
        let env_capture = fs::read_to_string(output_file).expect("env capture");

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout, "worker stdout");
        assert_eq!(result.stderr, "worker stderr");
        assert_eq!(result.workdir, worktree);
        assert!(env_capture.contains("ALLOWED_CONTEXT=visible"));
        assert!(env_capture.contains("HEPA_ADAPTER_ROLE=worker"));
        assert!(!env_capture.contains("UNLISTED_CONTEXT"));
        assert_eq!(
            result.allowed_env_keys,
            vec![
                "ALLOWED_CONTEXT".to_string(),
                "HEPA_ADAPTER_ID".to_string(),
                "HEPA_ADAPTER_ROLE".to_string(),
                "PATH".to_string()
            ]
        );

        remove_test_dir(root);
    }

    #[test]
    fn oneshot_executor_rejects_missing_lane_worktree() {
        let root = unique_test_dir("oneshot-missing-worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let invocation = HepaOneshotAdapterInvocation {
            spec: adapter_spec(
                "worker-primary",
                vec![HepaAdapterRole::Worker],
                "agent --prompt-file {prompt_file}".to_string(),
                None,
                Vec::new(),
            ),
            role: HepaAdapterRole::Worker,
            context: template_context(
                &root.join("missing-worktree"),
                &artifact_dir,
                &artifact_dir.join("out.json"),
            ),
            prompt: "Worker prompt from task spec".to_string(),
            environment: BTreeMap::new(),
            monitor_policy: HepaMonitorPolicy::default(),
        };

        let error = HepaOneshotAdapterExecutor::new()
            .run(&invocation)
            .expect_err("missing worktree must fail");

        assert_eq!(error.field, "workdir");
        assert!(error.message.contains("lane worktree"));

        remove_test_dir(root);
    }

    #[test]
    fn oneshot_executor_reports_fake_binary_failure() {
        let root = unique_test_dir("oneshot-failure");
        let worktree = root.join("lane-worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let script = root.join("fake-adapter");
        write_executable(
            &script,
            r#"#!/bin/sh
printf 'failure stdout'
printf 'failure stderr' >&2
exit 7
"#,
        );
        let output_file = artifact_dir.join("failure.json");
        let invocation = HepaOneshotAdapterInvocation {
            spec: adapter_spec(
                "worker-primary",
                vec![HepaAdapterRole::Worker],
                format!("{} --output {{output_file}}", script.display()),
                None,
                Vec::new(),
            ),
            role: HepaAdapterRole::Worker,
            context: template_context(&worktree, &artifact_dir, &output_file),
            prompt: "Worker prompt from task spec".to_string(),
            environment: BTreeMap::new(),
            monitor_policy: HepaMonitorPolicy::default(),
        };

        let result = HepaOneshotAdapterExecutor::new()
            .run(&invocation)
            .expect("adapter process should complete with failure status");

        assert_eq!(result.exit_code, Some(7));
        assert_eq!(result.stdout, "failure stdout");
        assert_eq!(result.stderr, "failure stderr");

        remove_test_dir(root);
    }

    #[test]
    fn oneshot_executor_blocks_unsupported_hepa_composed_flags_before_spawn() {
        let root = unique_test_dir("unsupported-flags");
        let worktree = root.join("lane-worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let output_file = artifact_dir.join("unsupported.json");
        let invocation = HepaOneshotAdapterInvocation {
            spec: adapter_spec(
                "shell-command",
                vec![HepaAdapterRole::Worker],
                "hepa-shell-adapter --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file} --mystery-flag"
                    .to_string(),
                None,
                Vec::new(),
            ),
            role: HepaAdapterRole::Worker,
            context: template_context(&worktree, &artifact_dir, &output_file),
            prompt: "Worker prompt from task spec".to_string(),
            environment: BTreeMap::new(),
            monitor_policy: HepaMonitorPolicy::default(),
        };

        let error = HepaOneshotAdapterExecutor::new()
            .run(&invocation)
            .expect_err("unsupported HEPA-composed flags must fail before spawn");

        assert_eq!(error.field, "invocation_template");
        assert_eq!(error.status.as_deref(), Some("blocked"));
        assert!(error.message.contains("--mystery-flag"));
        assert!(!PathBuf::from(&invocation.context.prompt_file).exists());

        remove_test_dir(root);
    }

    #[test]
    fn oneshot_executor_blocks_manager_credentials_for_worker_and_reviewer() {
        for (role, label) in [
            (HepaAdapterRole::Worker, "worker"),
            (HepaAdapterRole::Reviewer, "reviewer"),
        ] {
            let root = unique_test_dir(label);
            let worktree = root.join("lane-worktree");
            let artifact_dir = root.join("artifacts");
            fs::create_dir_all(&worktree).expect("worktree dir");
            fs::create_dir_all(&artifact_dir).expect("artifact dir");
            let script = root.join("fake-adapter");
            write_executable(
                &script,
                r#"#!/bin/sh
env | sort > "$2"
"#,
            );
            let output_file = artifact_dir.join(format!("{label}-env.txt"));
            let github_token_key = ["GITHUB", "TOKEN"].join("_");
            let command = format!("{} --output {{output_file}}", script.display());
            let invocation = HepaOneshotAdapterInvocation {
                spec: adapter_spec(
                    &format!("{label}-adapter"),
                    vec![role.clone()],
                    command.clone(),
                    Some(command),
                    vec!["ALLOWED_CONTEXT".to_string()],
                ),
                role: role.clone(),
                context: template_context(&worktree, &artifact_dir, &output_file),
                prompt: "Role prompt from task spec".to_string(),
                environment: BTreeMap::from([
                    ("ALLOWED_CONTEXT".to_string(), "visible".to_string()),
                    ("MANAGER_ONLY_CONTEXT".to_string(), "blocked".to_string()),
                    ("HEPA_MANAGER_SESSION".to_string(), "blocked".to_string()),
                    (github_token_key.clone(), "blocked".to_string()),
                ]),
                monitor_policy: HepaMonitorPolicy::default(),
            };

            let result = HepaOneshotAdapterExecutor::new()
                .run(&invocation)
                .expect("adapter should run");
            let env_capture = fs::read_to_string(output_file).expect("env capture");

            assert_eq!(result.exit_code, Some(0));
            assert!(env_capture.contains("ALLOWED_CONTEXT=visible"));
            assert!(env_capture.contains(&format!("HEPA_ADAPTER_ROLE={label}")));
            assert!(!env_capture.contains("MANAGER_ONLY_CONTEXT"));
            assert!(!env_capture.contains("HEPA_MANAGER_SESSION"));
            assert!(!env_capture.contains(github_token_key.as_str()));
            assert!(
                !result
                    .allowed_env_keys
                    .contains(&"MANAGER_ONLY_CONTEXT".to_string())
            );
            assert!(
                !result
                    .allowed_env_keys
                    .contains(&"HEPA_MANAGER_SESSION".to_string())
            );
            assert!(!result.allowed_env_keys.contains(&github_token_key));

            remove_test_dir(root);
        }
    }

    #[test]
    fn oneshot_executor_writes_prompt_files_and_passes_paths() {
        let root = unique_test_dir("prompt-file");
        let worktree = root.join("lane-worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let script = root.join("fake-adapter");
        write_executable(
            &script,
            r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prompt-file) prompt_file="$2"; shift 2 ;;
    --output) output_file="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s\n' "$prompt_file" > "$output_file"
cat "$prompt_file" >> "$output_file"
"#,
        );
        let output_file = artifact_dir.join("prompt-capture.txt");
        let invocation = HepaOneshotAdapterInvocation {
            spec: adapter_spec(
                "worker-primary",
                vec![HepaAdapterRole::Worker],
                format!(
                    "{} --prompt-file {{prompt_file}} --output {{output_file}}",
                    script.display()
                ),
                None,
                Vec::new(),
            ),
            role: HepaAdapterRole::Worker,
            context: template_context(&worktree, &artifact_dir, &output_file),
            prompt: "Implement the fixture task without inline task text.".to_string(),
            environment: BTreeMap::new(),
            monitor_policy: HepaMonitorPolicy::default(),
        };

        let result = HepaOneshotAdapterExecutor::new()
            .run(&invocation)
            .expect("adapter should run");
        let capture = fs::read_to_string(&output_file).expect("prompt capture");

        assert_eq!(result.exit_code, Some(0));
        assert!(PathBuf::from(&invocation.context.prompt_file).exists());
        assert!(
            capture
                .lines()
                .next()
                .unwrap_or_default()
                .ends_with("prompt.md")
        );
        assert!(capture.contains("Implement the fixture task"));
        assert!(!result.command.contains("Implement the fixture task"));

        remove_test_dir(root);
    }

    #[test]
    fn oneshot_executor_can_feed_prompt_to_stdin_and_capture_stdout_artifact() {
        let root = unique_test_dir("stdin-stdout");
        let worktree = root.join("lane-worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let script = root.join("pi");
        write_executable(
            &script,
            r#"#!/bin/sh
prompt="$(cat)"
printf '{"type":"session","cwd":"%s"}\n' "$(pwd)"
printf '{"type":"agent_end","message":{"content":"%s"}}\n' "$prompt"
printf 'fake pi stderr' >&2
"#,
        );
        let output_file = artifact_dir.join("pi-events.jsonl");
        let mut spec = adapter_spec(
            "pi",
            vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            format!("{} -p --mode json", script.display()),
            Some(format!("{} -p --mode json", script.display())),
            Vec::new(),
        );
        spec.prompt_transport = HepaAdapterPromptTransport::Stdin;
        spec.output_capture = HepaAdapterOutputCapture::Stdout;
        let invocation = HepaOneshotAdapterInvocation {
            spec,
            role: HepaAdapterRole::Worker,
            context: template_context(&worktree, &artifact_dir, &output_file),
            prompt: "Worker prompt through stdin".to_string(),
            environment: BTreeMap::new(),
            monitor_policy: HepaMonitorPolicy::default(),
        };

        let result = HepaOneshotAdapterExecutor::new()
            .run(&invocation)
            .expect("stdin/stdout adapter should run");
        let artifact = fs::read_to_string(&output_file).expect("stdout artifact exists");

        assert_eq!(result.exit_code, Some(0));
        assert!(result.stderr.contains("fake pi stderr"));
        assert!(artifact.contains("Worker prompt through stdin"));
        assert!(!PathBuf::from(&invocation.context.prompt_file).exists());

        remove_test_dir(root);
    }

    #[test]
    fn oneshot_executor_can_pass_prompt_as_single_command_argument() {
        let root = unique_test_dir("prompt-arg");
        let worktree = root.join("lane-worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let script = root.join("prompt-arg-adapter");
        write_executable(
            &script,
            r#"#!/bin/sh
printf '%s\n' "$#" > "$1"
printf '%s\n' "$2" >> "$1"
"#,
        );
        let output_file = artifact_dir.join("prompt-arg.txt");
        let mut spec = adapter_spec(
            "prompt-arg",
            vec![HepaAdapterRole::Worker],
            format!("{} {}", script.display(), output_file.display()),
            None,
            Vec::new(),
        );
        spec.prompt_transport = HepaAdapterPromptTransport::PromptArg;
        let invocation = HepaOneshotAdapterInvocation {
            spec,
            role: HepaAdapterRole::Worker,
            context: template_context(&worktree, &artifact_dir, &output_file),
            prompt: "Prompt with spaces and {literal braces}".to_string(),
            environment: BTreeMap::new(),
            monitor_policy: HepaMonitorPolicy::default(),
        };

        let result = HepaOneshotAdapterExecutor::new()
            .run(&invocation)
            .expect("prompt-arg adapter should run");
        let capture = fs::read_to_string(&output_file).expect("prompt arg capture");

        assert_eq!(result.exit_code, Some(0));
        assert!(capture.starts_with("2\n"));
        assert!(capture.contains("Prompt with spaces and {literal braces}"));
        assert!(!result.command.contains("Prompt with spaces"));

        remove_test_dir(root);
    }

    #[test]
    fn oneshot_executor_drains_large_stdout_while_adapter_is_running() {
        let root = unique_test_dir("large-stdout");
        let worktree = root.join("lane-worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let script = root.join("chatty-adapter");
        write_executable(
            &script,
            r#"#!/bin/sh
i=0
while [ "$i" -lt 12000 ]; do
  printf '{"type":"message_update","chunk":"%064d"}\n' "$i"
  i=$((i + 1))
done
printf '{"type":"agent_end","messages":[]}\n'
"#,
        );
        let output_file = artifact_dir.join("chatty.jsonl");
        let mut spec = adapter_spec(
            "chatty",
            vec![HepaAdapterRole::Worker],
            format!("{} ignored", script.display()),
            None,
            Vec::new(),
        );
        spec.output_capture = HepaAdapterOutputCapture::Stdout;
        let invocation = HepaOneshotAdapterInvocation {
            spec,
            role: HepaAdapterRole::Worker,
            context: template_context(&worktree, &artifact_dir, &output_file),
            prompt: "large stdout should not deadlock".to_string(),
            environment: BTreeMap::new(),
            monitor_policy: HepaMonitorPolicy {
                timeout_ms: Some(5_000),
                ..HepaMonitorPolicy::default()
            },
        };

        let result = HepaOneshotAdapterExecutor::new()
            .run(&invocation)
            .expect("chatty adapter should not deadlock");
        let artifact = fs::read_to_string(output_file).expect("chatty stdout artifact");

        assert_eq!(result.exit_code, Some(0));
        assert!(artifact.contains("\"agent_end\""));
        assert!(artifact.len() > 1_000_000);

        remove_test_dir(root);
    }

    #[test]
    fn oneshot_executor_monitor_blocks_command_output_scope_timeout_and_stall() {
        let root = unique_test_dir("monitor");
        let worktree = root.join("lane-worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let script = root.join("fake-adapter");
        write_executable(
            &script,
            r#"#!/bin/sh
case "$1" in
  leak) printf 'api_key=blocked' ;;
  scope) printf '%s' "$2" ;;
  slow) sleep 1 ;;
  ok) printf 'ok' ;;
esac
"#,
        );

        let command_error = run_monitor_case(
            &worktree,
            &artifact_dir,
            format!("{} ok && git push", script.display()),
            HepaMonitorPolicy::default(),
        )
        .expect_err("command policy should stop");
        assert_eq!(command_error.status.as_deref(), Some("blocked"));
        assert!(command_error.message.contains("command_policy"));

        let secret_error = run_monitor_case(
            &worktree,
            &artifact_dir,
            format!("{} leak", script.display()),
            HepaMonitorPolicy::default(),
        )
        .expect_err("secret output should stop");
        assert_eq!(secret_error.status.as_deref(), Some("blocked"));
        assert!(secret_error.message.contains("secret_detected"));

        let scope_error = run_monitor_case(
            &worktree,
            &artifact_dir,
            format!("{} scope <OUT_OF_SCOPE>", script.display()),
            HepaMonitorPolicy {
                blocked_scope_refs: vec!["<OUT_OF_SCOPE>".to_string()],
                ..HepaMonitorPolicy::default()
            },
        )
        .expect_err("scope output should stop");
        assert_eq!(scope_error.status.as_deref(), Some("blocked"));
        assert!(scope_error.message.contains("scope_violation"));

        let timeout_error = run_monitor_case(
            &worktree,
            &artifact_dir,
            format!("{} slow", script.display()),
            HepaMonitorPolicy {
                timeout_ms: Some(50),
                ..HepaMonitorPolicy::default()
            },
        )
        .expect_err("timeout should stop");
        assert_eq!(timeout_error.status.as_deref(), Some("blocked"));
        assert!(timeout_error.message.contains("timeout"));

        let stall_error = run_monitor_case(
            &worktree,
            &artifact_dir,
            format!("{} slow", script.display()),
            HepaMonitorPolicy {
                stall_ms: Some(50),
                ..HepaMonitorPolicy::default()
            },
        )
        .expect_err("stall should stop");
        assert_eq!(stall_error.status.as_deref(), Some("blocked"));
        assert!(stall_error.message.contains("stall"));

        remove_test_dir(root);
    }

    #[test]
    fn oneshot_executor_retains_partial_streams_on_monitor_stop() {
        let root = unique_test_dir("monitor-output");
        let worktree = root.join("lane-worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let script = root.join("fake-adapter");
        write_executable(
            &script,
            r#"#!/bin/sh
printf 'partial stdout before timeout'
printf 'partial stderr before timeout' >&2
sleep 10
"#,
        );

        let error = run_monitor_case(
            &worktree,
            &artifact_dir,
            format!("{} ignored", script.display()),
            HepaMonitorPolicy {
                timeout_ms: Some(5_000),
                ..HepaMonitorPolicy::default()
            },
        )
        .expect_err("timeout should retain diagnostics");

        assert_eq!(error.status.as_deref(), Some("blocked"));
        assert!(error.message.contains("timeout"));
        assert!(error.stdout.contains("partial stdout before timeout"));
        assert!(error.stderr.contains("partial stderr before timeout"));
        assert!(error.sanitized_summary().contains("captured stdout="));

        remove_test_dir(root);
    }

    #[test]
    fn oneshot_executor_timeout_does_not_wait_on_descendant_held_pipes() {
        let root = unique_test_dir("monitor-descendant");
        let worktree = root.join("lane-worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let script = root.join("fake-adapter");
        write_executable(
            &script,
            r#"#!/bin/sh
(sleep 5) &
printf 'parent output before timeout'
sleep 3
"#,
        );
        let started = Instant::now();

        let error = run_monitor_case(
            &worktree,
            &artifact_dir,
            format!("{} ignored", script.display()),
            HepaMonitorPolicy {
                timeout_ms: Some(250),
                ..HepaMonitorPolicy::default()
            },
        )
        .expect_err("timeout should return before descendant pipe EOF");

        assert_eq!(error.status.as_deref(), Some("blocked"));
        assert!(error.message.contains("timeout"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout path waited on descendant-held pipe"
        );

        remove_test_dir(root);
    }

    fn adapter_spec(
        id: &str,
        roles: Vec<HepaAdapterRole>,
        command: String,
        review_command: Option<String>,
        required_env: Vec<String>,
    ) -> HepaAdapterSpec {
        HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: id.to_string(),
            display_name: "Test Adapter".to_string(),
            roles,
            mode: HepaAdapterMode::Oneshot,
            command,
            review_command,
            workdir: "{worktree}".to_string(),
            required_commands: Vec::new(),
            required_env,
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: false,
            supports_json_output: true,
            capabilities: vec!["docs".to_string()],
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 1,
            prompt_transport: crate::spec::HepaAdapterPromptTransport::PromptFile,
            output_capture: crate::spec::HepaAdapterOutputCapture::AdapterFile,
        }
    }

    fn run_monitor_case(
        worktree: &Path,
        artifact_dir: &Path,
        command: String,
        monitor_policy: HepaMonitorPolicy,
    ) -> Result<HepaOneshotAdapterResult, HepaAdapterExecutionError> {
        let output_file = artifact_dir.join("monitor-output.json");
        let invocation = HepaOneshotAdapterInvocation {
            spec: adapter_spec(
                "monitor-adapter",
                vec![HepaAdapterRole::Worker],
                command,
                None,
                Vec::new(),
            ),
            role: HepaAdapterRole::Worker,
            context: template_context(worktree, artifact_dir, &output_file),
            prompt: "Monitor fixture prompt".to_string(),
            environment: BTreeMap::new(),
            monitor_policy,
        };
        HepaOneshotAdapterExecutor::new().run(&invocation)
    }

    fn template_context(
        worktree: &Path,
        artifact_dir: &Path,
        output_file: &Path,
    ) -> HepaAdapterTemplateContext {
        HepaAdapterTemplateContext {
            prompt_file: artifact_dir.join("prompt.md").to_string_lossy().to_string(),
            worktree: worktree.to_string_lossy().to_string(),
            review_prompt_file: artifact_dir.join("review.md").to_string_lossy().to_string(),
            output_file: output_file.to_string_lossy().to_string(),
            review_output_file: artifact_dir
                .join("review.json")
                .to_string_lossy()
                .to_string(),
            artifact_dir: artifact_dir.to_string_lossy().to_string(),
        }
    }

    fn write_executable(path: &Path, contents: &str) {
        fs::write(path, contents).expect("script write");
        let mut permissions = fs::metadata(path).expect("script metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).expect("script permissions");
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-adapter-engine-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
