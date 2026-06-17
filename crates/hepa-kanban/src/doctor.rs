use hepa_core::config::HepaHermesBridgeConfig;
use serde::{Deserialize, Serialize};
use std::{env, path::PathBuf, process::Command};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaKanbanDoctorReport {
    pub status: HepaKanbanDoctorStatus,
    pub checks: Vec<HepaKanbanDoctorCheck>,
}

impl HepaKanbanDoctorReport {
    pub fn from_checks(checks: impl IntoIterator<Item = HepaKanbanDoctorCheck>) -> Self {
        let checks = checks.into_iter().collect::<Vec<_>>();
        let status = if checks.iter().all(|check| {
            matches!(
                check.status,
                HepaKanbanCheckStatus::Ok | HepaKanbanCheckStatus::Skipped
            )
        }) {
            HepaKanbanDoctorStatus::Ok
        } else {
            HepaKanbanDoctorStatus::Degraded
        };
        Self { status, checks }
    }

    pub fn to_redacted_summary(&self) -> String {
        let status = match self.status {
            HepaKanbanDoctorStatus::Ok => "ok",
            HepaKanbanDoctorStatus::Degraded => "degraded",
        };
        let checks = self
            .checks
            .iter()
            .map(|check| {
                let check_status = match check.status {
                    HepaKanbanCheckStatus::Ok => "ok",
                    HepaKanbanCheckStatus::Skipped => "skipped",
                    HepaKanbanCheckStatus::Missing => "missing",
                    HepaKanbanCheckStatus::Failed => "failed",
                };
                format!("{}={}", check.name, check_status)
            })
            .collect::<Vec<_>>()
            .join(" ");
        let actions = self
            .checks
            .iter()
            .filter(|check| check.status != HepaKanbanCheckStatus::Ok)
            .map(|check| format!("{}: {}", check.name, redact_detail(&check.action)))
            .collect::<Vec<_>>()
            .join("; ");
        if actions.is_empty() {
            format!("HEPA kanban doctor: {status} {checks}")
        } else {
            format!("HEPA kanban doctor: {status} {checks}; actions: {actions}")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaKanbanDoctorStatus {
    Ok,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaKanbanDoctorCheck {
    pub name: String,
    pub status: HepaKanbanCheckStatus,
    pub detail: String,
    pub action: String,
}

impl HepaKanbanDoctorCheck {
    pub fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: HepaKanbanCheckStatus::Ok,
            detail: redact_detail(&detail.into()),
            action: "No action required.".to_string(),
        }
    }

    pub fn missing(name: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: HepaKanbanCheckStatus::Missing,
            detail: "Not detected.".to_string(),
            action: redact_detail(&action.into()),
        }
    }

    pub fn skipped(
        name: impl Into<String>,
        detail: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: HepaKanbanCheckStatus::Skipped,
            detail: redact_detail(&detail.into()),
            action: redact_detail(&action.into()),
        }
    }

    pub fn failed(
        name: impl Into<String>,
        detail: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: HepaKanbanCheckStatus::Failed,
            detail: redact_detail(&detail.into()),
            action: redact_detail(&action.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaKanbanCheckStatus {
    Ok,
    Skipped,
    Missing,
    Failed,
}

pub trait HepaKanbanDoctorProbe {
    fn command_present(&self, command: &str) -> bool;
    fn command_version(&self, command: &str) -> Option<String>;
    fn env_present(&self, name: &str) -> bool;
}

#[derive(Debug, Default)]
pub struct HepaSystemKanbanDoctorProbe;

impl HepaKanbanDoctorProbe for HepaSystemKanbanDoctorProbe {
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

pub fn system_kanban_doctor_report(config: &HepaHermesBridgeConfig) -> HepaKanbanDoctorReport {
    kanban_doctor_report(config, &HepaSystemKanbanDoctorProbe)
}

pub fn kanban_doctor_report(
    config: &HepaHermesBridgeConfig,
    probe: &impl HepaKanbanDoctorProbe,
) -> HepaKanbanDoctorReport {
    if !config.enabled {
        return HepaKanbanDoctorReport::from_checks([
            HepaKanbanDoctorCheck::skipped(
                "cli",
                "Hermes bridge disabled by configuration.",
                "Documented non-blocking skip: enable HEPA_HERMES_ENABLED for live board sync.",
            ),
            HepaKanbanDoctorCheck::skipped(
                "api",
                "Hermes bridge disabled by configuration.",
                "Documented non-blocking skip: configure Hermes endpoint and board when live sync is required.",
            ),
            HepaKanbanDoctorCheck::skipped(
                "auth",
                "Hermes bridge disabled by configuration.",
                "Documented non-blocking skip: authenticate Hermes when live sync is required.",
            ),
            HepaKanbanDoctorCheck::skipped(
                "workspace",
                "Hermes bridge disabled by configuration.",
                "Documented non-blocking skip: select a workspace when live sync is required.",
            ),
            HepaKanbanDoctorCheck::skipped(
                "board",
                "Hermes bridge disabled by configuration.",
                "Documented non-blocking skip: select a board when live sync is required.",
            ),
        ]);
    }

    let cli = if probe.command_present("hermes") {
        HepaKanbanDoctorCheck::ok(
            "cli",
            probe
                .command_version("hermes")
                .unwrap_or_else(|| "Hermes CLI detected.".to_string()),
        )
    } else {
        HepaKanbanDoctorCheck::missing("cli", "Install or configure the Hermes CLI/API.")
    };
    let api = match config.endpoint.as_deref() {
        Some(endpoint) if !endpoint.trim().is_empty() => {
            HepaKanbanDoctorCheck::ok("api", "Hermes endpoint configured.")
        }
        _ => HepaKanbanDoctorCheck::skipped(
            "api",
            "No Hermes endpoint configured; headless/degraded sync remains available.",
            "Documented non-blocking skip: set HEPA_HERMES_ENDPOINT for live board sync.",
        ),
    };
    let auth = if probe.env_present("HERMES_API_KEY")
        || probe.env_present("HERMES_TOKEN")
        || probe.env_present("HERMES_AUTH_TOKEN")
    {
        HepaKanbanDoctorCheck::ok("auth", "Hermes auth environment detected.")
    } else {
        HepaKanbanDoctorCheck::skipped(
            "auth",
            "No Hermes auth environment detected; headless/degraded sync remains available.",
            "Documented non-blocking skip: authenticate Hermes for live board sync.",
        )
    };
    let workspace = if config
        .endpoint
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        HepaKanbanDoctorCheck::ok("workspace", "Hermes workspace endpoint configured.")
    } else {
        HepaKanbanDoctorCheck::skipped(
            "workspace",
            "No Hermes workspace configured; headless/degraded sync remains available.",
            "Documented non-blocking skip: configure a Hermes workspace for live board sync.",
        )
    };
    let board = match config.board_id.as_deref() {
        Some(board_id) if !board_id.trim().is_empty() => {
            HepaKanbanDoctorCheck::ok("board", "Hermes board configured.")
        }
        _ => HepaKanbanDoctorCheck::skipped(
            "board",
            "No Hermes board configured; card projection evidence remains local.",
            "Documented non-blocking skip: set HEPA_HERMES_BOARD_ID for live board sync.",
        ),
    };

    HepaKanbanDoctorReport::from_checks([cli, api, auth, workspace, board])
}

fn command_exists_on_path(command: &str) -> bool {
    let command_path = PathBuf::from(command);
    if command_path.components().count() > 1 {
        return command_path.is_file();
    }
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|dir| {
        fs_metadata_is_file(dir.join(command))
            || (cfg!(windows) && fs_metadata_is_file(dir.join(format!("{command}.exe"))))
    })
}

fn fs_metadata_is_file(path: PathBuf) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
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
    .any(|prefix| value.starts_with(prefix))
}

fn is_account_like(value: &str) -> bool {
    value.contains('@') && value.rsplit_once('.').is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Default)]
    struct FakeProbe {
        commands: BTreeSet<String>,
        versions: BTreeMap<String, String>,
        env: BTreeSet<String>,
    }

    impl HepaKanbanDoctorProbe for FakeProbe {
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
    fn doctor_reports_cli_api_auth_workspace_and_board_checks() {
        let report = HepaKanbanDoctorReport::from_checks([
            HepaKanbanDoctorCheck::ok("cli", "Hermes CLI detected."),
            HepaKanbanDoctorCheck::ok("api", "Hermes API detected."),
            HepaKanbanDoctorCheck::missing("auth", "Authenticate the Hermes integration."),
            HepaKanbanDoctorCheck::missing("workspace", "Select a Hermes workspace."),
            HepaKanbanDoctorCheck::missing("board", "Select a reachable Hermes board."),
        ]);

        assert_eq!(report.status, HepaKanbanDoctorStatus::Degraded);
        assert_eq!(report.checks.len(), 5);
        let summary = report.to_redacted_summary();
        assert!(summary.contains("cli=ok"));
        assert!(summary.contains("api=ok"));
        assert!(summary.contains("auth=missing"));
        assert!(summary.contains("workspace=missing"));
        assert!(summary.contains("board=missing"));
    }

    #[test]
    fn doctor_reports_skipped_external_board_config_as_non_blocking() {
        let mut probe = FakeProbe::default();
        probe.commands.insert("hermes".to_string());
        probe
            .versions
            .insert("hermes".to_string(), "Hermes Agent v0.16.0".to_string());
        let report = kanban_doctor_report(&HepaHermesBridgeConfig::default(), &probe);

        assert_eq!(report.status, HepaKanbanDoctorStatus::Ok);
        let summary = report.to_redacted_summary();
        assert!(summary.contains("HEPA kanban doctor: ok"));
        assert!(summary.contains("cli=ok"));
        assert!(summary.contains("api=skipped"));
        assert!(summary.contains("auth=skipped"));
        assert!(summary.contains("workspace=skipped"));
        assert!(summary.contains("board=skipped"));
        assert!(summary.contains("Documented non-blocking skip"));
    }

    #[test]
    fn doctor_degrades_when_hermes_cli_is_missing() {
        let report =
            kanban_doctor_report(&HepaHermesBridgeConfig::default(), &FakeProbe::default());

        assert_eq!(report.status, HepaKanbanDoctorStatus::Degraded);
        assert!(report.to_redacted_summary().contains("cli=missing"));
    }

    #[test]
    fn doctor_summary_redacts_private_details() {
        let private_path = ["/", "Users", "/person/hermes"].concat();
        let account = ["owner", "@", "example", ".", "invalid"].concat();
        let report = HepaKanbanDoctorReport::from_checks([HepaKanbanDoctorCheck::failed(
            "workspace",
            format!("Workspace at {private_path} for {account} failed."),
            format!("Reconfigure {private_path} for {account}."),
        )]);
        let summary = report.to_redacted_summary();

        assert!(!summary.contains(&private_path));
        assert!(!summary.contains(&account));
        assert!(summary.contains("<PRIVATE_PATH>"));
        assert!(summary.contains("<ACCOUNT>"));
    }
}
