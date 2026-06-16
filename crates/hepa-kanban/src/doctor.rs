use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaKanbanDoctorReport {
    pub status: HepaKanbanDoctorStatus,
    pub checks: Vec<HepaKanbanDoctorCheck>,
}

impl HepaKanbanDoctorReport {
    pub fn from_checks(checks: impl IntoIterator<Item = HepaKanbanDoctorCheck>) -> Self {
        let checks = checks.into_iter().collect::<Vec<_>>();
        let status = if checks
            .iter()
            .all(|check| check.status == HepaKanbanCheckStatus::Ok)
        {
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
    Missing,
    Failed,
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
