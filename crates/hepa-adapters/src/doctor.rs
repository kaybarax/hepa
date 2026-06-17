use crate::{
    registry::HepaAdapterRegistry,
    spec::{
        HepaAdapterCostClass, HepaAdapterMode, HepaAdapterRole, HepaAdapterSandbox,
        HepaAdapterSpec, HepaAdapterTemplateContext,
    },
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, env, fs, path::PathBuf, process::Command};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaAdapterDoctorReport {
    pub status: HepaAdapterDoctorStatus,
    pub checks: Vec<HepaAdapterDoctorCheck>,
}

impl HepaAdapterDoctorReport {
    pub fn from_registry(
        registry: &HepaAdapterRegistry,
        probe: &impl HepaAdapterDoctorProbe,
    ) -> Self {
        Self::from_specs(registry.list(), probe)
    }

    pub fn from_specs<'a>(
        specs: impl IntoIterator<Item = &'a HepaAdapterSpec>,
        probe: &impl HepaAdapterDoctorProbe,
    ) -> Self {
        let checks = specs
            .into_iter()
            .map(|spec| check_adapter(spec, probe))
            .collect::<Vec<_>>();
        let status = if checks
            .iter()
            .all(|check| check.status == HepaAdapterCheckStatus::Ok)
        {
            HepaAdapterDoctorStatus::Ok
        } else {
            HepaAdapterDoctorStatus::Degraded
        };
        Self { status, checks }
    }

    pub fn to_redacted_summary(&self) -> String {
        let status = match self.status {
            HepaAdapterDoctorStatus::Ok => "ok",
            HepaAdapterDoctorStatus::Degraded => "degraded",
        };
        let checks = self
            .checks
            .iter()
            .map(|check| {
                format!(
                    "{}={}",
                    redact_detail(&check.adapter_id),
                    check.status.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let actions = self
            .checks
            .iter()
            .filter(|check| check.status != HepaAdapterCheckStatus::Ok)
            .map(|check| {
                format!(
                    "{}: {}",
                    redact_detail(&check.adapter_id),
                    redact_detail(&check.action)
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let diagnostics = self
            .checks
            .iter()
            .filter(|check| check.status != HepaAdapterCheckStatus::Ok)
            .map(|check| {
                format!(
                    "{}: command={} auth={} version={} template={} sandbox={} max_concurrency={}",
                    redact_detail(&check.adapter_id),
                    redact_detail(&check.command_presence),
                    redact_detail(&check.auth_state),
                    redact_detail(&check.version_state),
                    redact_detail(&check.invocation_template),
                    check.sandbox_posture,
                    check.concurrency_cap
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        if actions.is_empty() {
            format!("HEPA adapter doctor: {status} {checks}")
        } else if diagnostics.is_empty() {
            format!("HEPA adapter doctor: {status} {checks}; actions: {actions}")
        } else {
            format!(
                "HEPA adapter doctor: {status} {checks}; diagnostics: {diagnostics}; actions: {actions}"
            )
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaAdapterDoctorStatus {
    Ok,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaAdapterDoctorCheck {
    pub adapter_id: String,
    pub status: HepaAdapterCheckStatus,
    pub command_presence: String,
    pub auth_state: String,
    pub version_state: String,
    pub invocation_template: String,
    pub sandbox_posture: String,
    pub concurrency_cap: u32,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaAdapterCheckStatus {
    Ok,
    Missing,
    Failed,
}

impl HepaAdapterCheckStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::Failed => "failed",
        }
    }
}

pub trait HepaAdapterDoctorProbe {
    fn command_present(&self, command: &str) -> bool;
    fn command_version(&self, command: &str) -> Option<String>;
    fn env_present(&self, name: &str) -> bool;
}

#[derive(Debug, Default)]
pub struct HepaSystemAdapterDoctorProbe;

impl HepaAdapterDoctorProbe for HepaSystemAdapterDoctorProbe {
    fn command_present(&self, command: &str) -> bool {
        command_exists_on_path(command)
    }

    fn command_version(&self, command: &str) -> Option<String> {
        let output = Command::new(command).arg("--version").output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            None
        } else {
            Some(stdout)
        }
    }

    fn env_present(&self, name: &str) -> bool {
        env::var_os(name).is_some()
    }
}

pub fn format_adapter_list(registry: &HepaAdapterRegistry) -> String {
    let rows = registry
        .list()
        .into_iter()
        .map(|spec| {
            format!(
                "{}\t{}\tmode={}\troles={}\tsandbox={}\tcost={}\tmax_concurrency={}\tcommands={}",
                spec.id,
                spec.display_name,
                mode_name(&spec.mode),
                spec.roles
                    .iter()
                    .map(role_name)
                    .collect::<Vec<_>>()
                    .join(","),
                sandbox_name(&spec.sandbox),
                cost_class_name(&spec.cost_class),
                spec.max_concurrency,
                if spec.required_commands.is_empty() {
                    "none".to_string()
                } else {
                    spec.required_commands.join(",")
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("HEPA adapter list:\n{rows}")
}

fn check_adapter(
    spec: &HepaAdapterSpec,
    probe: &impl HepaAdapterDoctorProbe,
) -> HepaAdapterDoctorCheck {
    let mut status = HepaAdapterCheckStatus::Ok;
    let mut actions = Vec::new();

    let missing_commands = spec
        .required_commands
        .iter()
        .filter(|command| !probe.command_present(command))
        .cloned()
        .collect::<Vec<_>>();
    let command_presence = if spec.required_commands.is_empty() {
        "not_required".to_string()
    } else if missing_commands.is_empty() {
        "ok".to_string()
    } else {
        status = HepaAdapterCheckStatus::Missing;
        actions.push(format!(
            "install or configure required command(s): {}",
            missing_commands.join(",")
        ));
        format!("missing:{}", missing_commands.join(","))
    };

    let missing_env = spec
        .required_env
        .iter()
        .filter(|name| !probe.env_present(name))
        .cloned()
        .collect::<Vec<_>>();
    let auth_state = if spec.required_env.is_empty() {
        "not_detectable".to_string()
    } else if missing_env.is_empty() {
        "ok".to_string()
    } else {
        status = most_severe(status, HepaAdapterCheckStatus::Missing);
        actions.push(format!(
            "configure required adapter environment value(s): {}",
            missing_env.join(",")
        ));
        format!("missing_env:{}", missing_env.join(","))
    };

    let version_state = check_versions(spec, probe, &missing_commands, &mut status, &mut actions);
    let invocation_template = check_invocation_template(spec, &mut status, &mut actions);
    let sandbox_posture = sandbox_name(&spec.sandbox).to_string();
    if spec.max_concurrency == 0 {
        status = HepaAdapterCheckStatus::Failed;
        actions.push("set max_concurrency greater than zero".to_string());
    }

    HepaAdapterDoctorCheck {
        adapter_id: spec.id.clone(),
        status,
        command_presence,
        auth_state,
        version_state,
        invocation_template,
        sandbox_posture,
        concurrency_cap: spec.max_concurrency,
        action: if actions.is_empty() {
            "No action required.".to_string()
        } else {
            actions.join("; ")
        },
    }
}

fn check_versions(
    spec: &HepaAdapterSpec,
    probe: &impl HepaAdapterDoctorProbe,
    missing_commands: &[String],
    status: &mut HepaAdapterCheckStatus,
    actions: &mut Vec<String>,
) -> String {
    let missing = missing_commands.iter().collect::<BTreeSet<_>>();
    let mut states = Vec::new();
    for command in &spec.required_commands {
        if missing.contains(command) {
            states.push(format!("{command}=missing"));
            continue;
        }
        let Some(version) = probe.command_version(command) else {
            states.push(format!("{command}=unknown"));
            continue;
        };
        if is_known_good_version(command, &version) {
            states.push(format!("{command}=ok"));
        } else {
            *status = HepaAdapterCheckStatus::Failed;
            states.push(format!("{command}=drift"));
            actions.push(format!(
                "verify {command} version against the known-good 0.1.x adapter template"
            ));
        }
    }
    if states.is_empty() {
        "not_required".to_string()
    } else {
        states.join(",")
    }
}

fn check_invocation_template(
    spec: &HepaAdapterSpec,
    status: &mut HepaAdapterCheckStatus,
    actions: &mut Vec<String>,
) -> String {
    if let Err(error) = spec.validate() {
        *status = HepaAdapterCheckStatus::Failed;
        actions.push(format!(
            "fix adapter spec field {}: {}",
            error.field, error.message
        ));
        return "invalid".to_string();
    }
    let context = HepaAdapterTemplateContext {
        prompt_file: "<PROMPT_FILE>".to_string(),
        worktree: "<WORKTREE>".to_string(),
        review_prompt_file: "<REVIEW_PROMPT_FILE>".to_string(),
        output_file: "<OUTPUT_FILE>".to_string(),
        review_output_file: "<REVIEW_OUTPUT_FILE>".to_string(),
        artifact_dir: "<ARTIFACT_DIR>".to_string(),
    };
    let mut rendered = Vec::new();
    match spec.render_worker_command(&context) {
        Ok(command) => rendered.push(command),
        Err(error) => {
            *status = HepaAdapterCheckStatus::Failed;
            actions.push(format!("fix worker invocation template: {error}"));
            return "invalid".to_string();
        }
    }
    match spec.render_review_command(&context) {
        Ok(Some(command)) => rendered.push(command),
        Ok(None) => {}
        Err(error) => {
            *status = HepaAdapterCheckStatus::Failed;
            actions.push(format!("fix reviewer invocation template: {error}"));
            return "invalid".to_string();
        }
    }

    let unsupported_flags = rendered
        .iter()
        .flat_map(|command| unsupported_hepa_flags(command))
        .collect::<BTreeSet<_>>();
    if unsupported_flags.is_empty() {
        "ok".to_string()
    } else {
        *status = HepaAdapterCheckStatus::Failed;
        actions.push(format!(
            "remove unsupported HEPA-composed flag(s): {}",
            unsupported_flags.into_iter().collect::<Vec<_>>().join(",")
        ));
        "unsupported_flags".to_string()
    }
}

pub fn unsupported_hepa_flags(command: &str) -> Vec<String> {
    let mut tokens = command.split_whitespace();
    let Some(binary) = tokens.next() else {
        return Vec::new();
    };
    let Some(known_flags) = known_hepa_adapter_flags(binary) else {
        return Vec::new();
    };
    tokens
        .filter_map(|token| {
            if !token.starts_with("--") {
                return None;
            }
            let flag = token.split_once('=').map(|(flag, _)| flag).unwrap_or(token);
            if known_flags.contains(flag) {
                None
            } else {
                Some(flag.to_string())
            }
        })
        .collect()
}

fn known_hepa_adapter_flags(binary: &str) -> Option<BTreeSet<&'static str>> {
    let flags = match binary {
        "hepa-fake-adapter" => vec!["--prompt-file", "--json-output"],
        "hepa-shell-adapter"
        | "hepa-user-worker-adapter"
        | "hepa-user-reviewer-adapter"
        | "hepa-local-worker-adapter"
        | "hepa-external-worker-adapter" => {
            vec![
                "--prompt-file",
                "--worktree",
                "--artifact-dir",
                "--json-output",
                "--review",
            ]
        }
        "hepa-custom-adapter" => {
            vec![
                "--prompt-file",
                "--worktree",
                "--artifact-dir",
                "--json-output",
                "--review",
            ]
        }
        "pi" => vec![
            "--no-approve",
            "--no-context-files",
            "--no-extensions",
            "--no-prompt-templates",
            "--no-session",
            "--no-skills",
            "--mode",
            "--model",
            "--provider",
            "--tools",
        ],
        _ => return None,
    };
    Some(flags.into_iter().collect())
}

fn is_known_good_version(command: &str, version: &str) -> bool {
    if !command.starts_with("hepa-") {
        return true;
    }
    let first_line = version.lines().next().unwrap_or_default();
    first_line.contains(command) && first_line.contains("0.1.")
}

fn most_severe(
    current: HepaAdapterCheckStatus,
    candidate: HepaAdapterCheckStatus,
) -> HepaAdapterCheckStatus {
    match (current, candidate) {
        (HepaAdapterCheckStatus::Failed, _) | (_, HepaAdapterCheckStatus::Failed) => {
            HepaAdapterCheckStatus::Failed
        }
        (HepaAdapterCheckStatus::Missing, _) | (_, HepaAdapterCheckStatus::Missing) => {
            HepaAdapterCheckStatus::Missing
        }
        _ => HepaAdapterCheckStatus::Ok,
    }
}

fn command_exists_on_path(command: &str) -> bool {
    let command_path = PathBuf::from(command);
    if command_path.components().count() > 1 {
        return command_path.is_file();
    }
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path)
        .any(|dir| fs::metadata(dir.join(command)).is_ok_and(|meta| meta.is_file()))
}

fn mode_name(mode: &HepaAdapterMode) -> &'static str {
    match mode {
        HepaAdapterMode::Oneshot => "oneshot",
        HepaAdapterMode::Interactive => "interactive",
        HepaAdapterMode::External => "external",
    }
}

fn role_name(role: &HepaAdapterRole) -> &'static str {
    match role {
        HepaAdapterRole::Worker => "worker",
        HepaAdapterRole::Reviewer => "reviewer",
    }
}

fn sandbox_name(sandbox: &HepaAdapterSandbox) -> &'static str {
    match sandbox {
        HepaAdapterSandbox::None => "none",
        HepaAdapterSandbox::AgentNative => "agent-native",
        HepaAdapterSandbox::Container => "container",
    }
}

fn cost_class_name(cost_class: &HepaAdapterCostClass) -> &'static str {
    match cost_class {
        HepaAdapterCostClass::PaidCloud => "paid-cloud",
        HepaAdapterCostClass::FreeTier => "free-tier",
        HepaAdapterCostClass::Local => "local",
    }
}

fn redact_detail(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            if is_private_path_like(word) {
                "<PRIVATE_PATH>"
            } else if is_account_like(word) {
                "<ACCOUNT>"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_private_path_like(value: &str) -> bool {
    [
        ["/", "Users", "/"].concat(),
        ["/", "home", "/"].concat(),
        ["/", "Volumes", "/"].concat(),
        ["/", "private", "/"].concat(),
        ["/", "tmp", "/"].concat(),
    ]
    .iter()
    .any(|prefix| value.contains(prefix))
}

fn is_account_like(value: &str) -> bool {
    value.contains('@') && value.rsplit_once('.').is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        builtin::builtin_adapter_specs,
        spec::{ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterTemplateContext},
    };
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct FakeProbe {
        commands: BTreeSet<String>,
        versions: BTreeMap<String, String>,
        env: BTreeSet<String>,
    }

    impl HepaAdapterDoctorProbe for FakeProbe {
        fn command_present(&self, command: &str) -> bool {
            self.commands.contains(command)
        }

        fn command_version(&self, command: &str) -> Option<String> {
            self.versions.get(command).cloned()
        }

        fn env_present(&self, name: &str) -> bool {
            self.env.contains(name)
        }
    }

    #[test]
    fn doctor_reports_missing_commands_with_actions() {
        let specs = builtin_adapter_specs();
        let report = HepaAdapterDoctorReport::from_specs(specs.values(), &FakeProbe::default());

        assert_eq!(report.status, HepaAdapterDoctorStatus::Degraded);
        let shell = report
            .checks
            .iter()
            .find(|check| check.adapter_id == "shell-command")
            .expect("shell-command check");
        assert_eq!(shell.status, HepaAdapterCheckStatus::Missing);
        assert!(shell.command_presence.contains("hepa-shell-adapter"));
        assert!(shell.action.contains("install or configure"));
        let fake = report
            .checks
            .iter()
            .find(|check| check.adapter_id == "fake")
            .expect("fake check");
        assert_eq!(fake.status, HepaAdapterCheckStatus::Ok);
    }

    #[test]
    fn doctor_detects_version_drift_when_version_is_available() {
        let mut probe = FakeProbe::default();
        probe.commands.insert("hepa-shell-adapter".to_string());
        probe.versions.insert(
            "hepa-shell-adapter".to_string(),
            "hepa-shell-adapter 9.9.0".to_string(),
        );
        let spec = builtin_adapter_specs()
            .remove("shell-command")
            .expect("shell-command spec");
        let report = HepaAdapterDoctorReport::from_specs([&spec], &probe);

        assert_eq!(report.status, HepaAdapterDoctorStatus::Degraded);
        assert_eq!(report.checks[0].status, HepaAdapterCheckStatus::Failed);
        assert!(report.checks[0].version_state.contains("drift"));
        assert!(report.checks[0].action.contains("known-good 0.1.x"));
    }

    #[test]
    fn doctor_summary_includes_actionable_command_auth_and_version_diagnostics() {
        let mut probe = FakeProbe::default();
        probe.commands.insert("hepa-shell-adapter".to_string());
        probe.versions.insert(
            "hepa-shell-adapter".to_string(),
            "hepa-shell-adapter 9.9.0".to_string(),
        );
        let mut missing_auth = builtin_adapter_specs()
            .remove("custom")
            .expect("custom spec");
        missing_auth.required_env = vec!["HEPA_ADAPTER_PROFILE".to_string()];
        let drifted_version = builtin_adapter_specs()
            .remove("shell-command")
            .expect("shell-command spec");

        let report = HepaAdapterDoctorReport::from_specs([&missing_auth, &drifted_version], &probe);
        let summary = report.to_redacted_summary();

        assert!(summary.contains("diagnostics:"));
        assert!(summary.contains("custom: command=missing:hepa-custom-adapter"));
        assert!(summary.contains("auth=missing_env:HEPA_ADAPTER_PROFILE"));
        assert!(summary.contains("shell-command: command=ok"));
        assert!(summary.contains("version=hepa-shell-adapter=drift"));
        assert!(summary.contains("actions:"));
        assert!(summary.contains("install or configure required command(s): hepa-custom-adapter"));
        assert!(
            summary
                .contains("configure required adapter environment value(s): HEPA_ADAPTER_PROFILE")
        );
        assert!(summary.contains("verify hepa-shell-adapter version against the known-good 0.1.x"));
    }

    #[test]
    fn doctor_blocks_unsupported_hepa_composed_flags() {
        let mut spec = builtin_adapter_specs()
            .remove("shell-command")
            .expect("shell-command spec");
        spec.command.push_str(" --unknown-flag");
        let mut probe = FakeProbe::default();
        probe.commands.insert("hepa-shell-adapter".to_string());
        probe.versions.insert(
            "hepa-shell-adapter".to_string(),
            "hepa-shell-adapter 0.1.0".to_string(),
        );
        let report = HepaAdapterDoctorReport::from_specs([&spec], &probe);

        assert_eq!(report.checks[0].status, HepaAdapterCheckStatus::Failed);
        assert_eq!(report.checks[0].invocation_template, "unsupported_flags");
        assert!(report.checks[0].action.contains("--unknown-flag"));
    }

    #[test]
    fn builtin_hepa_adapter_templates_do_not_compose_host_bypass_flags() {
        let context = template_context();
        let dangerous_flags = [
            "--dangerously-skip-permissions",
            "--allow-all-host",
            "--privileged",
            "--no-sandbox",
        ];

        for spec in builtin_adapter_specs().values() {
            let mut rendered = vec![
                spec.render_worker_command(&context)
                    .expect("worker command renders"),
            ];
            if let Some(review_command) = spec
                .render_review_command(&context)
                .expect("review command renders")
            {
                rendered.push(review_command);
            }

            for command in rendered {
                assert_eq!(
                    unsupported_hepa_flags(&command),
                    Vec::<String>::new(),
                    "{} must only compose supported flags",
                    spec.id
                );
                for flag in dangerous_flags {
                    assert!(
                        !command.contains(flag),
                        "{} must not compose unrestricted host bypass flag {flag}",
                        spec.id
                    );
                }
            }
        }
    }

    #[test]
    fn unsupported_hepa_flags_detect_host_bypass_flags_for_known_adapters() {
        for flag in [
            "--dangerously-skip-permissions",
            "--allow-all-host",
            "--privileged",
            "--no-sandbox",
        ] {
            assert_eq!(
                unsupported_hepa_flags(&format!(
                    "hepa-custom-adapter --prompt-file <PROMPT_FILE> {flag}"
                )),
                vec![flag.to_string()]
            );
        }
    }

    #[test]
    fn doctor_reports_auth_env_when_declared_and_detectable() {
        let mut spec = builtin_adapter_specs()
            .remove("custom")
            .expect("custom spec");
        spec.required_env = vec!["HEPA_ADAPTER_PROFILE".to_string()];
        let report = HepaAdapterDoctorReport::from_specs([&spec], &FakeProbe::default());

        assert_eq!(report.checks[0].status, HepaAdapterCheckStatus::Missing);
        assert!(report.checks[0].auth_state.contains("HEPA_ADAPTER_PROFILE"));
    }

    #[test]
    fn doctor_summary_redacts_private_details() {
        let private_path = ["/", "Users", "/person/bin/hepa-shell-adapter"].concat();
        let account = ["owner", "@", "example", ".", "invalid"].concat();
        let report = HepaAdapterDoctorReport {
            status: HepaAdapterDoctorStatus::Degraded,
            checks: vec![HepaAdapterDoctorCheck {
                adapter_id: account.clone(),
                status: HepaAdapterCheckStatus::Failed,
                command_presence: format!("missing:{private_path}"),
                auth_state: format!("missing_env:{account}"),
                version_state: format!("drift:{private_path}"),
                invocation_template: format!("invalid:{account}"),
                sandbox_posture: "agent-native".to_string(),
                concurrency_cap: 1,
                action: format!("inspect {private_path} for {account}"),
            }],
        };
        let summary = report.to_redacted_summary();

        assert!(!summary.contains(&private_path));
        assert!(!summary.contains(&account));
        assert!(summary.contains("<PRIVATE_PATH>"));
        assert!(summary.contains("<ACCOUNT>"));
        assert!(summary.contains("<ACCOUNT>=failed"));
        assert!(summary.contains("command=<PRIVATE_PATH>"));
        assert!(summary.contains("auth=<ACCOUNT>"));
        assert!(summary.contains("version=<PRIVATE_PATH>"));
        assert!(summary.contains("template=<ACCOUNT>"));
    }

    #[test]
    fn adapter_list_includes_sandbox_and_concurrency_caps() {
        let mut registry = crate::registry::HepaAdapterRegistry::load("missing-registry.json")
            .expect("missing registry loads");
        registry
            .upsert(HepaAdapterSpec {
                schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
                id: "small-local".to_string(),
                display_name: "Small Local".to_string(),
                roles: vec![HepaAdapterRole::Worker],
                mode: HepaAdapterMode::Oneshot,
                command: "local --prompt-file {prompt_file}".to_string(),
                review_command: None,
                workdir: "{worktree}".to_string(),
                required_commands: vec!["local".to_string()],
                required_env: Vec::new(),
                sandbox: HepaAdapterSandbox::AgentNative,
                supports_resume: false,
                supports_json_output: true,
                capabilities: vec!["docs".to_string()],
                cost_class: HepaAdapterCostClass::Local,
                resource_weight: 1,
                max_concurrency: 2,
                prompt_transport: crate::spec::HepaAdapterPromptTransport::PromptFile,
                output_capture: crate::spec::HepaAdapterOutputCapture::AdapterFile,
            })
            .expect("spec upsert");
        let output = format_adapter_list(&registry);

        assert!(output.contains("small-local"));
        assert!(output.contains("sandbox=agent-native"));
        assert!(output.contains("max_concurrency=2"));
    }

    fn template_context() -> HepaAdapterTemplateContext {
        HepaAdapterTemplateContext {
            prompt_file: "<PROMPT_FILE>".to_string(),
            worktree: "<WORKTREE>".to_string(),
            review_prompt_file: "<REVIEW_PROMPT_FILE>".to_string(),
            output_file: "<OUTPUT_FILE>".to_string(),
            review_output_file: "<REVIEW_OUTPUT_FILE>".to_string(),
            artifact_dir: "<ARTIFACT_DIR>".to_string(),
        }
    }
}
