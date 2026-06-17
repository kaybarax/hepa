use hepa_core::contracts::{HepaFindingSeverity, HepaReviewFinding, HepaValidate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaArbitrationDisposition {
    Downgraded,
    ManagerRequired,
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
