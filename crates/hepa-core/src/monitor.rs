use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaMonitorPolicy {
    pub command_denylist: Vec<String>,
    pub secret_markers: Vec<String>,
    pub blocked_scope_refs: Vec<String>,
    pub suspicious_path_markers: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub stall_ms: Option<u64>,
}

impl Default for HepaMonitorPolicy {
    fn default() -> Self {
        Self {
            command_denylist: default_git_lifecycle_denylist(),
            secret_markers: vec![
                "api_key=".to_string(),
                "api_key:".to_string(),
                "private_key=".to_string(),
                "private_key:".to_string(),
                "secret=".to_string(),
            ],
            blocked_scope_refs: Vec::new(),
            suspicious_path_markers: default_suspicious_path_markers(),
            timeout_ms: None,
            stall_ms: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaMonitorStopKind {
    CommandPolicy,
    SecretDetected,
    ScopeViolation,
    SuspiciousPath,
    Timeout,
    Stall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaMonitorStop {
    pub kind: HepaMonitorStopKind,
    pub evidence: String,
}

impl HepaMonitorStop {
    pub fn new(kind: HepaMonitorStopKind, evidence: impl Into<String>) -> Self {
        Self {
            kind,
            evidence: evidence.into(),
        }
    }
}

impl fmt::Display for HepaMonitorStop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.evidence)
    }
}

impl Error for HepaMonitorStop {}

impl HepaMonitorPolicy {
    pub fn check_command(&self, command: &str) -> Result<(), HepaMonitorStop> {
        let command = command.to_ascii_lowercase();
        if let Some(evidence) = detect_git_lifecycle_action(&command) {
            return Err(HepaMonitorStop::new(
                HepaMonitorStopKind::CommandPolicy,
                evidence,
            ));
        }
        for denied in &self.command_denylist {
            if command.contains(&denied.to_ascii_lowercase()) {
                return Err(HepaMonitorStop::new(
                    HepaMonitorStopKind::CommandPolicy,
                    denied.clone(),
                ));
            }
        }
        for marker in &self.suspicious_path_markers {
            if command.contains(&marker.to_ascii_lowercase()) {
                return Err(HepaMonitorStop::new(
                    HepaMonitorStopKind::SuspiciousPath,
                    marker.clone(),
                ));
            }
        }
        Ok(())
    }

    /// Check a referenced file path for suspicious/secret-like access.
    pub fn check_path(&self, path: &str) -> Result<(), HepaMonitorStop> {
        let lowered = path.to_ascii_lowercase();
        for marker in &self.suspicious_path_markers {
            if lowered.contains(&marker.to_ascii_lowercase()) {
                return Err(HepaMonitorStop::new(
                    HepaMonitorStopKind::SuspiciousPath,
                    marker.clone(),
                ));
            }
        }
        Ok(())
    }

    pub fn check_output(&self, output: &str) -> Result<(), HepaMonitorStop> {
        let lowered = output.to_ascii_lowercase();
        for marker in &self.secret_markers {
            if output_contains_secret_marker(&lowered, &marker.to_ascii_lowercase()) {
                return Err(HepaMonitorStop::new(
                    HepaMonitorStopKind::SecretDetected,
                    marker.clone(),
                ));
            }
        }
        for scope_ref in &self.blocked_scope_refs {
            if scope_ref.trim().is_empty() {
                continue;
            }
            if output.contains(scope_ref) {
                return Err(HepaMonitorStop::new(
                    HepaMonitorStopKind::ScopeViolation,
                    "<SCOPE_REF>",
                ));
            }
        }
        Ok(())
    }

    pub fn check_elapsed(&self, elapsed_ms: u64) -> Result<(), HepaMonitorStop> {
        if self
            .timeout_ms
            .is_some_and(|timeout_ms| elapsed_ms > timeout_ms)
        {
            return Err(HepaMonitorStop::new(
                HepaMonitorStopKind::Timeout,
                "timeout budget exceeded",
            ));
        }
        if self.stall_ms.is_some_and(|stall_ms| elapsed_ms > stall_ms) {
            return Err(HepaMonitorStop::new(
                HepaMonitorStopKind::Stall,
                "stall budget exceeded",
            ));
        }
        Ok(())
    }
}

fn output_contains_secret_marker(output: &str, marker: &str) -> bool {
    if marker.contains('=') || marker.contains(':') {
        return output
            .match_indices(marker)
            .any(|(index, _)| assignment_marker_has_value(output, index + marker.len()));
    }

    output
        .lines()
        .any(|line| contains_secret_assignment(line, marker))
}

fn assignment_marker_has_value(output: &str, value_start: usize) -> bool {
    let value = output[value_start..]
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | '`' | ',' | ';' | ')' | ']' | '}' | '<' | '>'
            )
        });
    if value.is_empty() {
        return false;
    }
    !is_redacted_secret_placeholder(value)
}

fn is_redacted_secret_placeholder(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    if normalized.starts_with("your-") || normalized.starts_with("<your-") {
        return true;
    }
    matches!(
        normalized.as_str(),
        "redacted"
            | "<redacted>"
            | "[redacted]"
            | "***"
            | "****"
            | "xxxxx"
            | "xxxx"
            | "example"
            | "example-value"
            | "placeholder"
            | "<placeholder>"
    )
}

fn contains_secret_assignment(line: &str, marker: &str) -> bool {
    for separator in ['=', ':'] {
        let Some(index) = line.find(separator) else {
            continue;
        };
        let key = line[..index]
            .trim()
            .trim_start_matches(|character: char| !character.is_ascii_alphanumeric());
        if key.contains(marker) {
            return true;
        }
    }
    false
}

fn default_suspicious_path_markers() -> Vec<String> {
    [
        ".env",
        "/.ssh/",
        "id_rsa",
        "id_ed25519",
        ".aws/credentials",
        ".gnupg",
        ".npmrc",
        ".netrc",
        "/etc/shadow",
        "/etc/passwd",
        "private_key",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_git_lifecycle_denylist() -> Vec<String> {
    [
        "git add",
        "git branch",
        "git checkout",
        "git commit",
        "git merge",
        "git push",
        "git rebase",
        "git reset",
        "git restore",
        "git switch",
        "git tag",
        "git worktree add",
        "git worktree remove",
        "gh pr close",
        "gh pr create",
        "gh pr edit",
        "gh pr merge",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn detect_git_lifecycle_action(command: &str) -> Option<String> {
    let tokens = shell_like_tokens(command);
    for (index, token) in tokens.iter().enumerate() {
        if token == "git" {
            if let Some(action) = git_action_after(&tokens[index + 1..]) {
                return Some(action);
            }
        }
        if token == "gh"
            && tokens
                .get(index + 1)
                .is_some_and(|next| next.as_str() == "pr")
            && tokens.get(index + 2).is_some_and(|action| {
                matches!(action.as_str(), "close" | "create" | "edit" | "merge")
            })
        {
            return Some(format!("gh pr {}", tokens[index + 2]));
        }
    }
    None
}

fn git_action_after(tokens: &[String]) -> Option<String> {
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "-c" | "--git-dir" | "--work-tree" | "--namespace" => index += 2,
            token if token.starts_with("-c") && token.len() > 2 => index += 1,
            token
                if token.starts_with("--git-dir=")
                    || token.starts_with("--work-tree=")
                    || token.starts_with("--namespace=") =>
            {
                index += 1
            }
            token if token.starts_with('-') => index += 1,
            "worktree" => {
                return tokens.get(index + 1).and_then(|action| {
                    if matches!(action.as_str(), "add" | "remove") {
                        Some(format!("git worktree {action}"))
                    } else {
                        None
                    }
                });
            }
            action
                if matches!(
                    action,
                    "add"
                        | "branch"
                        | "checkout"
                        | "commit"
                        | "merge"
                        | "push"
                        | "rebase"
                        | "reset"
                        | "restore"
                        | "switch"
                        | "tag"
                ) =>
            {
                return Some(format!("git {action}"));
            }
            _ => return None,
        }
    }
    None
}

fn shell_like_tokens(command: &str) -> Vec<String> {
    command
        .split(|character: char| {
            character.is_ascii_whitespace() || matches!(character, ';' | '&' | '|' | '(' | ')')
        })
        .filter_map(|token| {
            let token = token
                .trim_matches(|character: char| {
                    matches!(character, '\'' | '"' | ',' | ':' | '[' | ']')
                })
                .to_ascii_lowercase();
            if token.is_empty() { None } else { Some(token) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_blocks_denied_commands_and_secret_output() {
        let policy = HepaMonitorPolicy::default();

        assert!(matches!(
            policy.check_command("agent && git -C <WORKTREE> push"),
            Err(HepaMonitorStop {
                kind: HepaMonitorStopKind::CommandPolicy,
                ..
            })
        ));
        assert!(matches!(
            policy.check_output("api_key=real-token-value"),
            Err(HepaMonitorStop {
                kind: HepaMonitorStopKind::SecretDetected,
                ..
            })
        ));
        assert!(
            policy
                .check_output("Updated ResetPasswordForm and reset-password tests")
                .is_ok()
        );
        assert!(
            policy
                .check_output("Document API key management without embedding credentials")
                .is_ok()
        );
        assert!(
            policy
                .check_output("## Secrets:\nManaged through the deployment platform")
                .is_ok()
        );
        assert!(
            policy
                .check_output("monitor emitted an empty sentinel: secret=")
                .is_ok()
        );
        assert!(
            policy
                .check_output("monitor emitted a redacted sentinel: secret=redacted")
                .is_ok()
        );
        assert!(
            policy
                .check_output("documentation placeholder: NEXTAUTH_SECRET=your-nextauth-secret")
                .is_ok()
        );
        assert!(matches!(
            policy.check_output("monitor emitted a real value: secret=real-token-value"),
            Err(HepaMonitorStop {
                kind: HepaMonitorStopKind::SecretDetected,
                ..
            })
        ));
    }

    #[test]
    fn monitor_blocks_manager_owned_git_and_pr_lifecycle_actions() {
        let policy = HepaMonitorPolicy::default();
        let blocked = [
            "worker && git add src/lib.rs",
            "worker && git commit -m docs",
            "worker && git -C <WORKTREE> push",
            "worker && git worktree remove lane",
            "reviewer && gh pr create --fill",
            "reviewer && gh pr merge 42",
        ];

        for command in blocked {
            let error = policy
                .check_command(command)
                .expect_err("manager-owned lifecycle must be blocked");

            assert_eq!(error.kind, HepaMonitorStopKind::CommandPolicy);
        }

        assert!(policy.check_command("reviewer && git diff --stat").is_ok());
        assert!(policy.check_command("worker && git status --short").is_ok());
    }

    #[test]
    fn monitor_blocks_suspicious_file_paths() {
        let policy = HepaMonitorPolicy::default();

        for command in [
            "cat ~/.ssh/id_rsa",
            "cp .env /tmp/leak",
            "less /etc/shadow",
            "read .aws/credentials",
        ] {
            let stop = policy
                .check_command(command)
                .expect_err("suspicious path must be blocked");
            assert_eq!(stop.kind, HepaMonitorStopKind::SuspiciousPath);
        }

        assert_eq!(
            policy
                .check_path("config/.env.local")
                .expect_err("secret-like path")
                .kind,
            HepaMonitorStopKind::SuspiciousPath
        );
        assert!(policy.check_path("src/main.rs").is_ok());
    }
}
