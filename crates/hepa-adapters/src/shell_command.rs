use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HepaShellCommandTaskKind {
    Format,
    Codegen,
    Docs,
    Smoke,
}

impl HepaShellCommandTaskKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::Codegen => "codegen",
            Self::Docs => "docs",
            Self::Smoke => "smoke",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaShellCommandRequest {
    pub task_kind: HepaShellCommandTaskKind,
    pub command: String,
    pub worktree: String,
    pub artifact_dir: String,
    pub output_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaShellCommandPlan {
    pub adapter_id: String,
    pub task_kind: HepaShellCommandTaskKind,
    pub command: String,
    pub artifact_dir: String,
    pub output_file: String,
    pub status_mapping: HepaShellCommandStatusMapping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaShellCommandStatusMapping {
    pub success_exit_code: i32,
    pub failure_status: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HepaShellCommandAdapter;

impl HepaShellCommandAdapter {
    pub fn new() -> Self {
        Self
    }

    pub fn plan(
        &self,
        request: HepaShellCommandRequest,
    ) -> Result<HepaShellCommandPlan, HepaShellCommandError> {
        request.validate()?;
        Ok(HepaShellCommandPlan {
            adapter_id: "shell-command".to_string(),
            task_kind: request.task_kind,
            command: request.command,
            artifact_dir: request.artifact_dir,
            output_file: request.output_file,
            status_mapping: HepaShellCommandStatusMapping {
                success_exit_code: 0,
                failure_status: "blocked".to_string(),
            },
        })
    }

    pub fn supported_task_kinds(&self) -> Vec<HepaShellCommandTaskKind> {
        vec![
            HepaShellCommandTaskKind::Format,
            HepaShellCommandTaskKind::Codegen,
            HepaShellCommandTaskKind::Docs,
            HepaShellCommandTaskKind::Smoke,
        ]
    }
}

impl HepaShellCommandRequest {
    fn validate(&self) -> Result<(), HepaShellCommandError> {
        require_single_line("command", &self.command)?;
        require_single_line("worktree", &self.worktree)?;
        require_single_line("artifact_dir", &self.artifact_dir)?;
        require_single_line("output_file", &self.output_file)?;
        reject_git_lifecycle(&self.command)?;
        reject_unrestricted_bypass(&self.command)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaShellCommandError {
    pub field: String,
    pub message: String,
}

impl HepaShellCommandError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaShellCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaShellCommandError {}

fn require_single_line(field: &str, value: &str) -> Result<(), HepaShellCommandError> {
    if value.trim().is_empty() {
        return Err(HepaShellCommandError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaShellCommandError::new(field, "must be a single line"));
    }
    Ok(())
}

fn reject_git_lifecycle(command: &str) -> Result<(), HepaShellCommandError> {
    let lower = command.to_ascii_lowercase();
    let blocked = [
        "git add",
        "git commit",
        "git push",
        "git tag",
        "gh pr create",
        "gh pr merge",
    ];
    if let Some(token) = blocked.iter().find(|token| lower.contains(**token)) {
        return Err(HepaShellCommandError::new(
            "command",
            format!("shell-command adapter must not perform Git lifecycle action: {token}"),
        ));
    }
    Ok(())
}

fn reject_unrestricted_bypass(command: &str) -> Result<(), HepaShellCommandError> {
    let lower = command.to_ascii_lowercase();
    let blocked = [
        "--dangerously-skip-permissions",
        "--allow-all-host",
        "--privileged",
        "--no-sandbox",
    ];
    if let Some(flag) = blocked.iter().find(|flag| lower.contains(**flag)) {
        return Err(HepaShellCommandError::new(
            "command",
            format!("shell-command adapter must not request unrestricted host posture: {flag}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_command_adapter_supports_format_codegen_docs_and_smoke_tasks() {
        let adapter = HepaShellCommandAdapter::new();

        assert_eq!(
            adapter.supported_task_kinds(),
            vec![
                HepaShellCommandTaskKind::Format,
                HepaShellCommandTaskKind::Codegen,
                HepaShellCommandTaskKind::Docs,
                HepaShellCommandTaskKind::Smoke,
            ]
        );
        for kind in adapter.supported_task_kinds() {
            let plan = adapter
                .plan(request(
                    kind,
                    format!("hepa-shell-fixture {}", kind.as_str()),
                ))
                .expect("shell command should plan");

            assert_eq!(plan.adapter_id, "shell-command");
            assert_eq!(plan.task_kind, kind);
            assert_eq!(plan.status_mapping.success_exit_code, 0);
            assert_eq!(plan.status_mapping.failure_status, "blocked");
            assert!(!plan.command.contains("llm"));
        }
    }

    #[test]
    fn shell_command_adapter_rejects_git_lifecycle_actions() {
        let error = HepaShellCommandAdapter::new()
            .plan(request(
                HepaShellCommandTaskKind::Docs,
                "git add README.md && git commit -m docs".to_string(),
            ))
            .expect_err("git lifecycle must fail");

        assert_eq!(error.field, "command");
        assert!(error.message.contains("Git lifecycle"));
    }

    #[test]
    fn shell_command_adapter_rejects_unrestricted_host_bypass_flags() {
        let error = HepaShellCommandAdapter::new()
            .plan(request(
                HepaShellCommandTaskKind::Smoke,
                "tool --dangerously-skip-permissions".to_string(),
            ))
            .expect_err("host bypass must fail");

        assert_eq!(error.field, "command");
        assert!(error.message.contains("unrestricted host"));
    }

    fn request(task_kind: HepaShellCommandTaskKind, command: String) -> HepaShellCommandRequest {
        HepaShellCommandRequest {
            task_kind,
            command,
            worktree: "<WORKTREE>".to_string(),
            artifact_dir: "<ARTIFACT_DIR>".to_string(),
            output_file: "<OUTPUT_FILE>".to_string(),
        }
    }
}
