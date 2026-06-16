use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaMonitorPolicy {
    pub command_denylist: Vec<String>,
    pub secret_markers: Vec<String>,
    pub blocked_scope_refs: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub stall_ms: Option<u64>,
}

impl Default for HepaMonitorPolicy {
    fn default() -> Self {
        Self {
            command_denylist: vec!["git push".to_string(), "git commit".to_string()],
            secret_markers: vec![
                "api_key".to_string(),
                "password".to_string(),
                "private_key".to_string(),
                "secret=".to_string(),
            ],
            blocked_scope_refs: Vec::new(),
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
        for denied in &self.command_denylist {
            if command.contains(&denied.to_ascii_lowercase()) {
                return Err(HepaMonitorStop::new(
                    HepaMonitorStopKind::CommandPolicy,
                    denied.clone(),
                ));
            }
        }
        Ok(())
    }

    pub fn check_output(&self, output: &str) -> Result<(), HepaMonitorStop> {
        let lowered = output.to_ascii_lowercase();
        for marker in &self.secret_markers {
            if lowered.contains(&marker.to_ascii_lowercase()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_blocks_denied_commands_and_secret_output() {
        let policy = HepaMonitorPolicy::default();

        assert!(matches!(
            policy.check_command("agent && git push"),
            Err(HepaMonitorStop {
                kind: HepaMonitorStopKind::CommandPolicy,
                ..
            })
        ));
        assert!(matches!(
            policy.check_output("password=redacted"),
            Err(HepaMonitorStop {
                kind: HepaMonitorStopKind::SecretDetected,
                ..
            })
        ));
    }
}
