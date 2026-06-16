use crate::spec::{HepaAdapterMode, HepaAdapterRole, HepaAdapterSpec, HepaAdapterTemplateContext};
use std::{collections::BTreeMap, error::Error, fmt, path::PathBuf, process::Command};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaOneshotAdapterInvocation {
    pub spec: HepaAdapterSpec,
    pub role: HepaAdapterRole,
    pub context: HepaAdapterTemplateContext,
    pub environment: BTreeMap<String, String>,
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
        let argv = split_command_line(&command)?;
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| HepaAdapterExecutionError::new("command", "must not be empty"))?;
        let workdir = rendered_workdir(invocation)?;
        if !workdir.is_dir() {
            return Err(HepaAdapterExecutionError::new(
                "workdir",
                "lane worktree must exist before adapter launch",
            ));
        }

        let filtered_env = filtered_environment(invocation);
        let mut child = Command::new(program);
        child.args(args).current_dir(&workdir).env_clear().envs(
            filtered_env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
        let output = child.output().map_err(|error| {
            HepaAdapterExecutionError::new("command", format!("failed to spawn adapter: {error}"))
        })?;

        Ok(HepaOneshotAdapterResult {
            adapter_id: invocation.spec.id.clone(),
            role: invocation.role.clone(),
            command,
            workdir,
            allowed_env_keys: filtered_env.keys().cloned().collect(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaAdapterExecutionError {
    pub field: String,
    pub message: String,
}

impl HepaAdapterExecutionError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaAdapterExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaAdapterExecutionError {}

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

fn rendered_workdir(
    invocation: &HepaOneshotAdapterInvocation,
) -> Result<PathBuf, HepaAdapterExecutionError> {
    crate::spec::render_command_template(&invocation.spec.workdir, &invocation.context)
        .map(PathBuf::from)
        .map_err(|error| HepaAdapterExecutionError::new("workdir", error.to_string()))
}

fn filtered_environment(invocation: &HepaOneshotAdapterInvocation) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
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
    use crate::spec::{ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterSandbox};
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
            environment: BTreeMap::from([
                ("ALLOWED_CONTEXT".to_string(), "visible".to_string()),
                ("UNLISTED_CONTEXT".to_string(), "hidden".to_string()),
            ]),
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
                "HEPA_ADAPTER_ROLE".to_string()
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
            environment: BTreeMap::new(),
        };

        let error = HepaOneshotAdapterExecutor::new()
            .run(&invocation)
            .expect_err("missing worktree must fail");

        assert_eq!(error.field, "workdir");
        assert!(error.message.contains("lane worktree"));

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
                    vec![
                        "ALLOWED_CONTEXT".to_string(),
                        "MANAGER_ONLY_CONTEXT".to_string(),
                        "HEPA_MANAGER_SESSION".to_string(),
                    ],
                ),
                role: role.clone(),
                context: template_context(&worktree, &artifact_dir, &output_file),
                environment: BTreeMap::from([
                    ("ALLOWED_CONTEXT".to_string(), "visible".to_string()),
                    ("MANAGER_ONLY_CONTEXT".to_string(), "blocked".to_string()),
                    ("HEPA_MANAGER_SESSION".to_string(), "blocked".to_string()),
                    (github_token_key.clone(), "blocked".to_string()),
                ]),
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
        }
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
