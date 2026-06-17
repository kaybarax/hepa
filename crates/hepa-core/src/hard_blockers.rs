use crate::monitor::{HepaMonitorStop, HepaMonitorStopKind};
use crate::redaction::redact_secrets;

/// A blocked status derived from a deterministic monitor stop.
///
/// The evidence is always sanitized, and the card status is the Hermes-blocked
/// vocabulary so the block reconciles onto the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaBlockedStatus {
    pub reason: String,
    pub evidence: String,
    pub card_status: String,
    pub human_attention_required: bool,
}

/// Map a monitor stop to a blocked status with sanitized evidence.
pub fn block_from_monitor_stop(stop: &HepaMonitorStop) -> HepaBlockedStatus {
    HepaBlockedStatus {
        reason: stop_kind_name(&stop.kind).to_string(),
        evidence: redact_secrets(&stop.evidence),
        card_status: "blocked".to_string(),
        human_attention_required: true,
    }
}

fn stop_kind_name(kind: &HepaMonitorStopKind) -> &'static str {
    match kind {
        HepaMonitorStopKind::CommandPolicy => "command_policy",
        HepaMonitorStopKind::SecretDetected => "secret_detected",
        HepaMonitorStopKind::ScopeViolation => "scope_violation",
        HepaMonitorStopKind::SuspiciousPath => "suspicious_path",
        HepaMonitorStopKind::Timeout => "timeout",
        HepaMonitorStopKind::Stall => "stall",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::HepaMonitorPolicy;
    use crate::notifications::HepaNotificationStatus;

    #[test]
    fn unsafe_git_and_secret_path_scenarios_block_consistently() {
        let policy = HepaMonitorPolicy::default();

        let git_stop = policy
            .check_command("worker && git push origin main")
            .expect_err("unsafe git must stop");
        let git_block = block_from_monitor_stop(&git_stop);
        assert_eq!(git_block.reason, "command_policy");
        assert_eq!(git_block.card_status, "blocked");
        assert!(git_block.human_attention_required);

        let path_stop = policy
            .check_command("cat ~/.ssh/id_rsa")
            .expect_err("secret path must stop");
        let path_block = block_from_monitor_stop(&path_stop);
        assert_eq!(path_block.reason, "suspicious_path");
        assert_eq!(path_block.card_status, "blocked");
    }

    #[test]
    fn blocked_evidence_is_sanitized() {
        let stop = HepaMonitorStop::new(
            HepaMonitorStopKind::SecretDetected,
            "leaked GITHUB_TOKEN=ghp_supersecretvalue in output",
        );
        let block = block_from_monitor_stop(&stop);
        assert!(!block.evidence.contains("ghp_supersecretvalue"));
        assert!(block.evidence.contains("<redacted>"));
    }

    #[test]
    fn blocked_card_status_matches_hermes_blocked_vocabulary() {
        let stop = HepaMonitorStop::new(HepaMonitorStopKind::ScopeViolation, "<SCOPE_REF>");
        let block = block_from_monitor_stop(&stop);
        // The block reconciles onto the board as a blocked card.
        assert!(HepaNotificationStatus::Blocked.agrees_with_card_status(&block.card_status));
    }
}
