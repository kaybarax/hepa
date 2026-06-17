use hepa_core::contracts::{
    CONTRACT_SCHEMA_VERSION, HepaArbitrationFindingRecord, HepaArbitrationSummary,
    HepaFindingSeverity, HepaReviewFinding, HepaValidate,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaArbitrationDisposition {
    Downgraded,
    ManagerRequired,
    ManagerAccepted,
    ManagerRejected,
    ManagerDowngraded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaArbitratedFinding {
    pub original: HepaReviewFinding,
    pub finding: HepaReviewFinding,
    pub disposition: HepaArbitrationDisposition,
    pub rule_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaArbitrationError {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaManagerArbitrationAction {
    Accept,
    Reject,
    Downgrade { severity: HepaFindingSeverity },
}

pub fn apply_deterministic_downgrade_rules(
    finding: HepaReviewFinding,
) -> Result<HepaArbitratedFinding, HepaArbitrationError> {
    finding.validate().map_err(|error| HepaArbitrationError {
        field: error.field,
        message: error.message,
    })?;

    if !finding.in_scope && !finding.release_risk && finding.severity != HepaFindingSeverity::Low {
        let mut downgraded = finding.clone();
        downgraded.severity = HepaFindingSeverity::Low;
        downgraded.accepted = false;
        return Ok(HepaArbitratedFinding {
            original: finding,
            finding: downgraded,
            disposition: HepaArbitrationDisposition::Downgraded,
            rule_id: Some("out-of-scope-non-release-risk".to_string()),
            reason: "Out-of-scope non-release-risk findings are downgraded to low severity and excluded from repair.".to_string(),
        });
    }

    Ok(HepaArbitratedFinding {
        original: finding.clone(),
        finding,
        disposition: HepaArbitrationDisposition::ManagerRequired,
        rule_id: None,
        reason: "No deterministic downgrade rule applies; manager arbitration is required."
            .to_string(),
    })
}

pub fn apply_manager_arbitration(
    arbitrated: HepaArbitratedFinding,
    action: HepaManagerArbitrationAction,
    reason: impl Into<String>,
) -> Result<HepaArbitratedFinding, HepaArbitrationError> {
    if arbitrated.disposition != HepaArbitrationDisposition::ManagerRequired {
        return Err(HepaArbitrationError {
            field: "disposition".to_string(),
            message: "manager arbitration applies only to manager-required findings".to_string(),
        });
    }
    let reason = reason.into();
    require_reason(&reason)?;
    let mut finding = arbitrated.finding;

    let disposition = match action {
        HepaManagerArbitrationAction::Accept => {
            finding.accepted = true;
            HepaArbitrationDisposition::ManagerAccepted
        }
        HepaManagerArbitrationAction::Reject => {
            finding.accepted = false;
            HepaArbitrationDisposition::ManagerRejected
        }
        HepaManagerArbitrationAction::Downgrade { severity } => {
            require_downgrade(&finding.severity, &severity)?;
            finding.severity = severity;
            finding.accepted = true;
            HepaArbitrationDisposition::ManagerDowngraded
        }
    };

    Ok(HepaArbitratedFinding {
        original: arbitrated.original,
        finding,
        disposition,
        rule_id: Some("manager-judgment".to_string()),
        reason,
    })
}

pub fn summarize_arbitration_results(
    decisions: &[HepaArbitratedFinding],
) -> Result<HepaArbitrationSummary, HepaArbitrationError> {
    if decisions.is_empty() {
        return Err(HepaArbitrationError {
            field: "decisions".to_string(),
            message: "at least one arbitration decision is required".to_string(),
        });
    }

    let mut records = decisions.iter().map(arbitration_record).collect::<Vec<_>>();
    records.sort_by(|left, right| left.finding_id.cmp(&right.finding_id));
    let accepted = records.iter().filter(|record| record.accepted).count();
    let status = arbitration_status(&records);
    let pr_body_lines = records
        .iter()
        .map(|record| {
            format!(
                "- {}: {}, {:?} -> {:?}, accepted={}, reason={}",
                record.finding_id,
                record.disposition,
                record.severity_before,
                record.severity_after,
                record.accepted,
                record.reason
            )
        })
        .collect::<Vec<_>>();
    Ok(HepaArbitrationSummary {
        schema_version: CONTRACT_SCHEMA_VERSION,
        card_status: format!(
            "arbitration={status} records={} accepted={accepted}",
            records.len()
        ),
        status,
        records,
        pr_body_lines,
    })
}

fn arbitration_record(decision: &HepaArbitratedFinding) -> HepaArbitrationFindingRecord {
    HepaArbitrationFindingRecord {
        schema_version: CONTRACT_SCHEMA_VERSION,
        finding_id: decision.original.finding_id.clone(),
        disposition: disposition_name(&decision.disposition).to_string(),
        rule_id: decision.rule_id.clone(),
        reason: decision.reason.clone(),
        severity_before: decision.original.severity.clone(),
        severity_after: decision.finding.severity.clone(),
        accepted: decision.finding.accepted,
    }
}

fn arbitration_status(records: &[HepaArbitrationFindingRecord]) -> String {
    if records
        .iter()
        .any(|record| record.disposition == "manager_required")
    {
        "manager_required".to_string()
    } else if records.iter().any(|record| record.accepted) {
        "residual_accepted".to_string()
    } else {
        "settled".to_string()
    }
}

fn disposition_name(disposition: &HepaArbitrationDisposition) -> &'static str {
    match disposition {
        HepaArbitrationDisposition::Downgraded => "downgraded",
        HepaArbitrationDisposition::ManagerRequired => "manager_required",
        HepaArbitrationDisposition::ManagerAccepted => "manager_accepted",
        HepaArbitrationDisposition::ManagerRejected => "manager_rejected",
        HepaArbitrationDisposition::ManagerDowngraded => "manager_downgraded",
    }
}

fn require_reason(reason: &str) -> Result<(), HepaArbitrationError> {
    if reason.trim().is_empty() {
        return Err(HepaArbitrationError {
            field: "reason".to_string(),
            message: "manager arbitration reason must not be empty".to_string(),
        });
    }
    if reason.contains('\n') || reason.contains('\r') {
        return Err(HepaArbitrationError {
            field: "reason".to_string(),
            message: "manager arbitration reason must be a single line".to_string(),
        });
    }
    Ok(())
}

fn require_downgrade(
    from: &HepaFindingSeverity,
    to: &HepaFindingSeverity,
) -> Result<(), HepaArbitrationError> {
    if severity_rank(to) >= severity_rank(from) {
        return Err(HepaArbitrationError {
            field: "severity".to_string(),
            message: "manager downgrade must lower severity".to_string(),
        });
    }
    Ok(())
}

fn severity_rank(severity: &HepaFindingSeverity) -> u8 {
    match severity {
        HepaFindingSeverity::Low => 0,
        HepaFindingSeverity::Medium => 1,
        HepaFindingSeverity::High => 2,
        HepaFindingSeverity::Critical => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downgrades_out_of_scope_non_release_risk_findings_deterministically() {
        let decision = apply_deterministic_downgrade_rules(finding(
            HepaFindingSeverity::High,
            false,
            false,
            true,
        ))
        .expect("finding arbitrates");

        assert_eq!(decision.disposition, HepaArbitrationDisposition::Downgraded);
        assert_eq!(
            decision.rule_id,
            Some("out-of-scope-non-release-risk".to_string())
        );
        assert_eq!(decision.original.severity, HepaFindingSeverity::High);
        assert_eq!(decision.finding.severity, HepaFindingSeverity::Low);
        assert!(!decision.finding.accepted);
        assert!(decision.reason.contains("Out-of-scope non-release-risk"));
    }

    #[test]
    fn keeps_in_scope_or_release_risk_findings_for_manager_judgment() {
        for finding in [
            finding(HepaFindingSeverity::High, true, false, true),
            finding(HepaFindingSeverity::Medium, false, true, true),
        ] {
            let decision =
                apply_deterministic_downgrade_rules(finding).expect("finding arbitrates");

            assert_eq!(
                decision.disposition,
                HepaArbitrationDisposition::ManagerRequired
            );
            assert_eq!(decision.rule_id, None);
            assert_eq!(decision.original, decision.finding);
            assert!(decision.reason.contains("manager arbitration is required"));
        }
    }

    #[test]
    fn manager_can_accept_reject_or_downgrade_required_findings_with_reason() {
        let accepted = manager_decision(
            HepaManagerArbitrationAction::Accept,
            "Manager accepts the residual risk for this lane.",
        );
        assert_eq!(
            accepted.disposition,
            HepaArbitrationDisposition::ManagerAccepted
        );
        assert!(accepted.finding.accepted);
        assert_eq!(accepted.rule_id, Some("manager-judgment".to_string()));

        let rejected = manager_decision(
            HepaManagerArbitrationAction::Reject,
            "Manager rejects the finding because validation evidence contradicts it.",
        );
        assert_eq!(
            rejected.disposition,
            HepaArbitrationDisposition::ManagerRejected
        );
        assert!(!rejected.finding.accepted);

        let downgraded = manager_decision(
            HepaManagerArbitrationAction::Downgrade {
                severity: HepaFindingSeverity::Medium,
            },
            "Manager downgrades because the issue is non-blocking after inspection.",
        );
        assert_eq!(
            downgraded.disposition,
            HepaArbitrationDisposition::ManagerDowngraded
        );
        assert_eq!(downgraded.finding.severity, HepaFindingSeverity::Medium);
        assert!(downgraded.finding.accepted);
    }

    #[test]
    fn manager_arbitration_requires_reason_and_real_downgrade() {
        let manager_required = apply_deterministic_downgrade_rules(finding(
            HepaFindingSeverity::High,
            true,
            true,
            true,
        ))
        .expect("manager required");

        let empty_reason = apply_manager_arbitration(
            manager_required.clone(),
            HepaManagerArbitrationAction::Reject,
            " ",
        )
        .expect_err("empty reason fails");
        assert_eq!(empty_reason.field, "reason");

        let non_downgrade = apply_manager_arbitration(
            manager_required,
            HepaManagerArbitrationAction::Downgrade {
                severity: HepaFindingSeverity::High,
            },
            "Manager attempted to keep the same severity.",
        )
        .expect_err("same severity is not a downgrade");
        assert_eq!(non_downgrade.field, "severity");
    }

    #[test]
    fn arbitration_summary_records_artifact_pr_body_and_card_status() {
        let accepted = manager_decision(
            HepaManagerArbitrationAction::Accept,
            "Manager accepts the residual risk for this lane.",
        );
        let rejected = manager_decision(
            HepaManagerArbitrationAction::Reject,
            "Manager rejects the finding because validation evidence contradicts it.",
        );

        let summary =
            summarize_arbitration_results(&[rejected, accepted]).expect("summary renders");

        assert_eq!(summary.status, "residual_accepted");
        assert_eq!(
            summary.card_status,
            "arbitration=residual_accepted records=2 accepted=1"
        );
        assert_eq!(summary.records.len(), 2);
        assert_eq!(summary.records[0].finding_id, "finding-1");
        assert!(
            summary
                .pr_body_lines
                .iter()
                .any(|line| line.contains("manager_accepted")
                    && line.contains("Manager accepts the residual risk"))
        );
    }

    fn manager_decision(
        action: HepaManagerArbitrationAction,
        reason: &str,
    ) -> HepaArbitratedFinding {
        let manager_required = apply_deterministic_downgrade_rules(finding(
            HepaFindingSeverity::High,
            true,
            true,
            false,
        ))
        .expect("manager required");
        apply_manager_arbitration(manager_required, action, reason)
            .expect("manager arbitration applies")
    }

    fn finding(
        severity: HepaFindingSeverity,
        in_scope: bool,
        release_risk: bool,
        accepted: bool,
    ) -> HepaReviewFinding {
        HepaReviewFinding {
            finding_id: "finding-1".to_string(),
            severity,
            category: "correctness".to_string(),
            evidence: "Reviewer evidence describes the issue.".to_string(),
            in_scope,
            release_risk,
            recommended_action: "Apply the recommended fix.".to_string(),
            file_ref: Some("src/lib.rs".to_string()),
            line: Some(10),
            message: "Finding message.".to_string(),
            accepted,
        }
    }
}
