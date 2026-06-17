use crate::spec::{
    ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterMode, HepaAdapterRole,
    HepaAdapterSandbox, HepaAdapterSpec, HepaAdapterSpecError, HepaAdapterTemplateContext,
};
use hepa_core::monitor::{HepaMonitorPolicy, HepaMonitorStop, HepaMonitorStopKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    path::PathBuf,
    process::{Command, Stdio},
};

pub const EXTERNAL_STATUS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaExternalWorkerAdapterTemplate {
    pub id: String,
    pub display_name: String,
    pub status_command: String,
    pub required_commands: Vec<String>,
    pub env_allowlist: Vec<String>,
    pub max_concurrency: u32,
}

impl HepaExternalWorkerAdapterTemplate {
    pub fn into_spec(self) -> Result<HepaAdapterSpec, HepaExternalWorkerAdapterError> {
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: self.id,
            display_name: self.display_name,
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::External,
            command: self.status_command,
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: self.required_commands,
            required_env: self.env_allowlist,
            sandbox: HepaAdapterSandbox::None,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["external".to_string(), "status".to_string()],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: self.max_concurrency,
            prompt_transport: crate::spec::HepaAdapterPromptTransport::PromptFile,
            output_capture: crate::spec::HepaAdapterOutputCapture::AdapterFile,
        };
        spec.validate()
            .map_err(HepaExternalWorkerAdapterError::from)?;
        Ok(spec)
    }
}

impl Default for HepaExternalWorkerAdapterTemplate {
    fn default() -> Self {
        Self {
            id: "external-worker".to_string(),
            display_name: "External Worker Adapter".to_string(),
            status_command: "external-worker --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file}".to_string(),
            required_commands: vec!["external-worker".to_string()],
            env_allowlist: Vec::new(),
            max_concurrency: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaExternalWorkerAdapterError {
    pub field: String,
    pub message: String,
    pub status: Option<String>,
}

impl fmt::Display for HepaExternalWorkerAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaExternalWorkerAdapterError {}

impl From<HepaAdapterSpecError> for HepaExternalWorkerAdapterError {
    fn from(error: HepaAdapterSpecError) -> Self {
        Self {
            field: error.field,
            message: error.message,
            status: None,
        }
    }
}

impl HepaExternalWorkerAdapterError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            status: None,
        }
    }

    fn blocked(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            status: Some("blocked".to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaExternalStatusReport {
    pub schema_version: u32,
    pub adapter_id: String,
    pub external_ref: String,
    pub lane_id: String,
    pub status: HepaExternalWorkStatus,
    pub summary: Vec<String>,
    pub updated_at: String,
}

impl HepaExternalStatusReport {
    pub fn validate(&self) -> Result<(), HepaExternalWorkerAdapterError> {
        if self.schema_version != EXTERNAL_STATUS_SCHEMA_VERSION {
            return Err(HepaExternalWorkerAdapterError::new(
                "schema_version",
                format!("must be {EXTERNAL_STATUS_SCHEMA_VERSION}"),
            ));
        }
        require_single_line("adapter_id", &self.adapter_id)?;
        require_single_line("external_ref", &self.external_ref)?;
        reject_secret_like_ref("external_ref", &self.external_ref)?;
        require_single_line("lane_id", &self.lane_id)?;
        require_single_line("updated_at", &self.updated_at)?;
        if self.summary.is_empty() {
            return Err(HepaExternalWorkerAdapterError::new(
                "summary",
                "must include at least one status line",
            ));
        }
        for (index, line) in self.summary.iter().enumerate() {
            require_single_line(format!("summary[{index}]"), line)?;
            reject_secret_like_ref(format!("summary[{index}]"), line)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HepaExternalWorkStatus {
    Queued,
    Running,
    Completed,
    Blocked,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaExternalStatusPollRequest {
    pub spec: HepaAdapterSpec,
    pub context: HepaAdapterTemplateContext,
    pub prompt: String,
    pub environment: BTreeMap<String, String>,
    pub monitor_policy: HepaMonitorPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaExternalStatusPollResult {
    pub adapter_id: String,
    pub command: String,
    pub workdir: PathBuf,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub report: HepaExternalStatusReport,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HepaExternalStatusPoller;

impl HepaExternalStatusPoller {
    pub fn new() -> Self {
        Self
    }

    pub fn poll(
        &self,
        request: &HepaExternalStatusPollRequest,
    ) -> Result<HepaExternalStatusPollResult, HepaExternalWorkerAdapterError> {
        request
            .spec
            .validate()
            .map_err(HepaExternalWorkerAdapterError::from)?;
        if request.spec.mode != HepaAdapterMode::External {
            return Err(HepaExternalWorkerAdapterError::new(
                "mode",
                "status poller only supports external adapters",
            ));
        }
        if !request.spec.roles.contains(&HepaAdapterRole::Worker) {
            return Err(HepaExternalWorkerAdapterError::new(
                "role",
                "external status adapters must support worker role",
            ));
        }
        let command = request
            .spec
            .render_worker_command(&request.context)
            .map_err(|error| HepaExternalWorkerAdapterError::new("command", error.to_string()))?;
        let unsupported_flags = crate::doctor::unsupported_hepa_flags(&command);
        if !unsupported_flags.is_empty() {
            return Err(HepaExternalWorkerAdapterError::blocked(
                "invocation_template",
                format!(
                    "unsupported HEPA-composed flag(s): {}",
                    unsupported_flags.join(",")
                ),
            ));
        }
        request
            .monitor_policy
            .check_command(&command)
            .map_err(monitor_error)?;
        let workdir = crate::spec::render_command_template(&request.spec.workdir, &request.context)
            .map(PathBuf::from)
            .map_err(|error| HepaExternalWorkerAdapterError::new("workdir", error.to_string()))?;
        if !workdir.is_dir() {
            return Err(HepaExternalWorkerAdapterError::new(
                "workdir",
                "lane worktree must exist before external status poll",
            ));
        }
        write_status_prompt(request)?;
        let output = run_status_command(&command, &workdir, request)?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        request
            .monitor_policy
            .check_output(&stdout)
            .and_then(|_| request.monitor_policy.check_output(&stderr))
            .map_err(monitor_error)?;
        if !output.status.success() {
            return Err(HepaExternalWorkerAdapterError::new(
                "status_command",
                format!(
                    "external status command exited with {:?}",
                    output.status.code()
                ),
            ));
        }
        let report = parse_external_status_report(&request.context.output_file)?;

        Ok(HepaExternalStatusPollResult {
            adapter_id: request.spec.id.clone(),
            command,
            workdir,
            exit_code: output.status.code(),
            stdout,
            stderr,
            report,
        })
    }
}

pub fn parse_external_status_report(
    report_path: &str,
) -> Result<HepaExternalStatusReport, HepaExternalWorkerAdapterError> {
    let raw = fs::read_to_string(report_path).map_err(|error| {
        HepaExternalWorkerAdapterError::new(
            "output_file",
            format!("failed to read external status report: {error}"),
        )
    })?;
    let report = serde_json::from_str::<HepaExternalStatusReport>(&raw).map_err(|error| {
        HepaExternalWorkerAdapterError::new(
            "output_file",
            format!("failed to parse external status report: {error}"),
        )
    })?;
    report.validate()?;
    Ok(report)
}

fn write_status_prompt(
    request: &HepaExternalStatusPollRequest,
) -> Result<(), HepaExternalWorkerAdapterError> {
    let prompt_path = PathBuf::from(&request.context.prompt_file);
    if let Some(parent) = prompt_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            HepaExternalWorkerAdapterError::new(
                "prompt_file",
                format!("failed to create parent: {error}"),
            )
        })?;
    }
    fs::write(prompt_path, &request.prompt).map_err(|error| {
        HepaExternalWorkerAdapterError::new(
            "prompt_file",
            format!("failed to write external status prompt: {error}"),
        )
    })
}

fn run_status_command(
    command: &str,
    workdir: &PathBuf,
    request: &HepaExternalStatusPollRequest,
) -> Result<std::process::Output, HepaExternalWorkerAdapterError> {
    let argv = command.split_whitespace().collect::<Vec<_>>();
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| HepaExternalWorkerAdapterError::new("command", "must not be empty"))?;
    let mut child = Command::new(program);
    child
        .args(args)
        .current_dir(workdir)
        .env_clear()
        .envs(filtered_environment(request))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    child.output().map_err(|error| {
        HepaExternalWorkerAdapterError::new(
            "status_command",
            format!("failed to run external status command: {error}"),
        )
    })
}

fn filtered_environment(request: &HepaExternalStatusPollRequest) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }
    for key in &request.spec.required_env {
        if let Some(value) = request.environment.get(key) {
            env.insert(key.clone(), value.clone());
        }
    }
    env
}

fn monitor_error(stop: HepaMonitorStop) -> HepaExternalWorkerAdapterError {
    let reason = match stop.kind {
        HepaMonitorStopKind::CommandPolicy => "command_policy",
        HepaMonitorStopKind::SecretDetected => "secret_detected",
        HepaMonitorStopKind::ScopeViolation => "scope_violation",
        HepaMonitorStopKind::SuspiciousPath => "suspicious_path",
        HepaMonitorStopKind::Timeout => "timeout",
        HepaMonitorStopKind::Stall => "stall",
    };
    HepaExternalWorkerAdapterError::blocked("monitor", format!("{reason}: {}", stop.evidence))
}

fn require_single_line(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaExternalWorkerAdapterError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaExternalWorkerAdapterError::new(
            field,
            "must not be empty",
        ));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaExternalWorkerAdapterError::new(
            field,
            "must be a single line",
        ));
    }
    Ok(())
}

fn reject_secret_like_ref(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaExternalWorkerAdapterError> {
    let field = field.into();
    let lowered = value.to_ascii_lowercase();
    if lowered.contains("/users/")
        || lowered.contains("/home/")
        || lowered.contains(".env")
        || lowered.contains("api_key")
        || lowered.contains("apikey")
        || lowered.contains("credential")
        || lowered.contains("password")
        || lowered.contains("private_key")
        || lowered.contains("secret")
        || lowered.contains("token")
    {
        return Err(HepaExternalWorkerAdapterError::new(
            field,
            "must not contain sensitive references",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn external_worker_template_builds_external_status_spec() {
        let spec = HepaExternalWorkerAdapterTemplate {
            status_command: "external-status --prompt-file {prompt_file} --artifact-dir {artifact_dir} --json-output {output_file}".to_string(),
            required_commands: vec!["external-status".to_string()],
            env_allowlist: vec!["EXTERNAL_QUEUE".to_string()],
            ..HepaExternalWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect("external worker spec should validate");

        assert_eq!(spec.id, "external-worker");
        assert_eq!(spec.roles, vec![HepaAdapterRole::Worker]);
        assert_eq!(spec.mode, HepaAdapterMode::External);
        assert_eq!(spec.sandbox, HepaAdapterSandbox::None);
        assert_eq!(spec.capabilities, vec!["external", "status"]);
        assert_eq!(spec.required_env, vec!["EXTERNAL_QUEUE"]);
        assert!(spec.supports_resume);
    }

    #[test]
    fn external_status_report_validates_and_parses_status_artifact() {
        let root = unique_test_dir("external-status-report");
        fs::create_dir_all(&root).expect("root dir");
        let report_path = root.join("status.json");
        fs::write(
            &report_path,
            serde_json::to_string(&HepaExternalStatusReport {
                schema_version: EXTERNAL_STATUS_SCHEMA_VERSION,
                adapter_id: "external-worker".to_string(),
                external_ref: "queue-item-42".to_string(),
                lane_id: "lane-1".to_string(),
                status: HepaExternalWorkStatus::Running,
                summary: vec!["External worker is still running.".to_string()],
                updated_at: "2026-06-18T00:00:00Z".to_string(),
            })
            .expect("serialize report"),
        )
        .expect("write report");

        let report =
            parse_external_status_report(&report_path.to_string_lossy()).expect("parse report");

        assert_eq!(report.status, HepaExternalWorkStatus::Running);
        assert_eq!(report.external_ref, "queue-item-42");
        remove_test_dir(root);
    }

    #[test]
    fn external_status_report_rejects_sensitive_refs() {
        let report = HepaExternalStatusReport {
            schema_version: EXTERNAL_STATUS_SCHEMA_VERSION,
            adapter_id: "external-worker".to_string(),
            external_ref: "secret-ticket".to_string(),
            lane_id: "lane-1".to_string(),
            status: HepaExternalWorkStatus::Blocked,
            summary: vec!["Blocked waiting on credential".to_string()],
            updated_at: "2026-06-18T00:00:00Z".to_string(),
        };

        let error = report
            .validate()
            .expect_err("sensitive external refs must be rejected");

        assert_eq!(error.field, "external_ref");
    }

    #[test]
    fn external_status_poller_runs_external_mode_and_collects_report() {
        let root = unique_test_dir("external-status-poller");
        let worktree = root.join("worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let fake_bin = root.join("external-status");
        write_fake_external_status(&fake_bin);
        let output_file = artifact_dir.join("status.json");
        let spec = HepaExternalWorkerAdapterTemplate {
            status_command: format!(
                "{} --prompt-file {{prompt_file}} --json-output {{output_file}}",
                fake_bin.display()
            ),
            required_commands: vec![fake_bin.to_string_lossy().to_string()],
            ..HepaExternalWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect("external worker spec");
        let request = HepaExternalStatusPollRequest {
            spec,
            context: template_context(&worktree, &artifact_dir, &output_file),
            prompt: "Report external status for lane-1.".to_string(),
            environment: BTreeMap::new(),
            monitor_policy: HepaMonitorPolicy::default(),
        };

        let result = HepaExternalStatusPoller::new()
            .poll(&request)
            .expect("external status should poll");

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.report.status, HepaExternalWorkStatus::Running);
        assert_eq!(result.report.lane_id, "lane-1");
        assert!(result.stdout.contains("external status stdout"));
        assert!(PathBuf::from(&request.context.prompt_file).exists());
        remove_test_dir(root);
    }

    #[test]
    fn external_status_poller_rejects_oneshot_adapters() {
        let root = unique_test_dir("external-status-rejects-oneshot");
        let worktree = root.join("worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let mut spec = HepaExternalWorkerAdapterTemplate::default()
            .into_spec()
            .expect("external worker spec");
        spec.mode = HepaAdapterMode::Oneshot;
        let request = HepaExternalStatusPollRequest {
            spec,
            context: template_context(&worktree, &artifact_dir, &artifact_dir.join("status.json")),
            prompt: "Report external status.".to_string(),
            environment: BTreeMap::new(),
            monitor_policy: HepaMonitorPolicy::default(),
        };

        let error = HepaExternalStatusPoller::new()
            .poll(&request)
            .expect_err("only external adapters may poll external status");

        assert_eq!(error.field, "mode");
        remove_test_dir(root);
    }

    #[test]
    fn external_status_poller_blocks_git_lifecycle_before_spawn() {
        let root = unique_test_dir("external-status-blocks-git");
        let worktree = root.join("worktree");
        let artifact_dir = root.join("artifacts");
        fs::create_dir_all(&worktree).expect("worktree dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        let spec = HepaExternalWorkerAdapterTemplate {
            status_command: "external-status --prompt-file {prompt_file} && git push".to_string(),
            required_commands: vec!["external-status".to_string()],
            ..HepaExternalWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect("external worker spec");
        let request = HepaExternalStatusPollRequest {
            spec,
            context: template_context(&worktree, &artifact_dir, &artifact_dir.join("status.json")),
            prompt: "Report external status.".to_string(),
            environment: BTreeMap::new(),
            monitor_policy: HepaMonitorPolicy::default(),
        };

        let error = HepaExternalStatusPoller::new()
            .poll(&request)
            .expect_err("git lifecycle must be blocked before spawn");

        assert_eq!(error.field, "monitor");
        assert_eq!(error.status.as_deref(), Some("blocked"));
        assert!(!PathBuf::from(&request.context.prompt_file).exists());
        remove_test_dir(root);
    }

    #[test]
    fn external_worker_template_rejects_manager_only_env_allowlist() {
        let error = HepaExternalWorkerAdapterTemplate {
            env_allowlist: vec![["GITHUB", "TOKEN"].join("_")],
            ..HepaExternalWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect_err("manager-only env must fail");

        assert_eq!(error.field, "required_env[0]");
        assert!(error.message.contains("manager-only"));
    }

    #[test]
    fn external_worker_template_rejects_invalid_status_placeholders() {
        let error = HepaExternalWorkerAdapterTemplate {
            status_command: "external-status --raw {task_text}".to_string(),
            ..HepaExternalWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect_err("raw task placeholder must fail");

        assert_eq!(error.field, "command");
        assert!(error.message.contains("task_text"));
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
            review_output_file: output_file.to_string_lossy().to_string(),
            artifact_dir: artifact_dir.to_string_lossy().to_string(),
        }
    }

    fn write_fake_external_status(path: &Path) {
        fs::write(
        path,
        r#"#!/bin/sh
output_file=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --json-output) output_file="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf 'external status stdout'
printf '{"schema_version":1,"adapter_id":"external-worker","external_ref":"queue-item-42","lane_id":"lane-1","status":"running","summary":["External worker is still running."],"updated_at":"2026-06-18T00:00:00Z"}\n' > "$output_file"
"#,
    )
    .expect("fake external status write");
        let mut permissions = fs::metadata(path)
            .expect("fake external status metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).expect("fake external status permissions");
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-external-worker-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
