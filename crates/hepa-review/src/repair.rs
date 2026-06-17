use hepa_core::contracts::{HepaReviewFinding, HepaValidate, HepaValidationCommandResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaRepairBriefInput {
    pub lane_id: String,
    pub repair_round: u32,
    pub prior_prompt: String,
    pub failing_commands: Vec<HepaValidationCommandResult>,
    pub review_findings: Vec<HepaReviewFinding>,
    pub diff_state: String,
    pub files_touched: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaRepairBrief {
    pub lane_id: String,
    pub repair_round: u32,
    pub prompt: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaRepairRoundPolicy {
    pub max_repair_rounds: u32,
    pub max_total_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaRepairRoundState {
    pub next_repair_round: u32,
    pub total_attempts_after_next: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaRepairRoundDecision {
    pub allowed: bool,
    pub block_classification: Option<HepaRepairBlockClassification>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaRepairBlockClassification {
    RoundExhausted,
    AttemptsExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaRepairBlock {
    pub classification: HepaRepairBlockClassification,
    pub reason: String,
    pub human_attention_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaRepairError {
    pub field: String,
    pub message: String,
}

pub fn rewrite_repair_prompt_from_evidence(
    input: HepaRepairBriefInput,
) -> Result<HepaRepairBrief, HepaRepairError> {
    require_single_line("lane_id", &input.lane_id)?;
    if input.repair_round == 0 {
        return Err(HepaRepairError {
            field: "repair_round".to_string(),
            message: "must be greater than zero".to_string(),
        });
    }
    require_non_empty("prior_prompt", &input.prior_prompt)?;
    require_non_empty("diff_state", &input.diff_state)?;
    require_string_list("files_touched", &input.files_touched)?;
    for finding in &input.review_findings {
        finding.validate().map_err(|error| HepaRepairError {
            field: error.field,
            message: error.message,
        })?;
    }

    let evidence = evidence_lines(&input);
    if evidence.is_empty() {
        return Err(HepaRepairError {
            field: "evidence".to_string(),
            message: "repair brief requires failing commands or reviewer findings".to_string(),
        });
    }

    let prompt = format!(
        "Ralph-V2 repair brief for lane {}\nRound: {}\n\nPrior prompt summary:\n{}\n\nEvidence to address:\n{}\n\nDiff state:\n{}\n\nFiles touched:\n{}\n\nRepair instructions:\nFix only the evidenced failures above, preserve unrelated work, rerun the failing commands, and explain any remaining blocker with the evidence that still fails.",
        input.lane_id,
        input.repair_round,
        input.prior_prompt.trim(),
        evidence.join("\n"),
        input.diff_state.trim(),
        input.files_touched.join(", ")
    );

    Ok(HepaRepairBrief {
        lane_id: input.lane_id,
        repair_round: input.repair_round,
        prompt,
        evidence,
    })
}

pub fn enforce_repair_round_budget(
    policy: HepaRepairRoundPolicy,
    state: HepaRepairRoundState,
) -> Result<HepaRepairRoundDecision, HepaRepairError> {
    if policy.max_repair_rounds == 0 {
        return Err(HepaRepairError {
            field: "max_repair_rounds".to_string(),
            message: "must be greater than zero".to_string(),
        });
    }
    if policy.max_total_attempts == 0 {
        return Err(HepaRepairError {
            field: "max_total_attempts".to_string(),
            message: "must be greater than zero".to_string(),
        });
    }
    if state.next_repair_round == 0 {
        return Err(HepaRepairError {
            field: "next_repair_round".to_string(),
            message: "must be greater than zero".to_string(),
        });
    }
    if state.next_repair_round > policy.max_repair_rounds {
        return Ok(HepaRepairRoundDecision {
            allowed: false,
            block_classification: Some(HepaRepairBlockClassification::RoundExhausted),
            reason: format!(
                "repair round {} exceeds max repair rounds {}",
                state.next_repair_round, policy.max_repair_rounds
            ),
        });
    }
    if state.total_attempts_after_next > policy.max_total_attempts {
        return Ok(HepaRepairRoundDecision {
            allowed: false,
            block_classification: Some(HepaRepairBlockClassification::AttemptsExhausted),
            reason: format!(
                "total attempts {} exceeds max total attempts {}",
                state.total_attempts_after_next, policy.max_total_attempts
            ),
        });
    }
    Ok(HepaRepairRoundDecision {
        allowed: true,
        block_classification: None,
        reason: format!(
            "repair round {} within max rounds {} and total attempts {} within max {}",
            state.next_repair_round,
            policy.max_repair_rounds,
            state.total_attempts_after_next,
            policy.max_total_attempts
        ),
    })
}

pub fn block_when_repair_cannot_converge(
    decision: HepaRepairRoundDecision,
) -> Option<HepaRepairBlock> {
    decision
        .block_classification
        .map(|classification| HepaRepairBlock {
            classification,
            reason: decision.reason,
            human_attention_required: true,
        })
}

fn evidence_lines(input: &HepaRepairBriefInput) -> Vec<String> {
    let mut evidence = Vec::new();
    for command in &input.failing_commands {
        if command.exit_code != 0 {
            evidence.push(format!(
                "- command `{}` failed with exit code {} after {} ms",
                command.command, command.exit_code, command.duration_ms
            ));
        }
    }
    for finding in &input.review_findings {
        if finding.accepted {
            evidence.push(format!(
                "- review finding {} [{}]: {} Evidence: {} Recommended action: {}",
                finding.finding_id,
                finding.category,
                finding.message,
                finding.evidence,
                finding.recommended_action
            ));
        }
    }
    evidence
}

fn require_single_line(field: impl Into<String>, value: &str) -> Result<(), HepaRepairError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaRepairError {
            field,
            message: "must not be empty".to_string(),
        });
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaRepairError {
            field,
            message: "must be a single line".to_string(),
        });
    }
    Ok(())
}

fn require_non_empty(field: impl Into<String>, value: &str) -> Result<(), HepaRepairError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaRepairError {
            field,
            message: "must not be empty".to_string(),
        });
    }
    Ok(())
}

fn require_string_list(field: &str, values: &[String]) -> Result<(), HepaRepairError> {
    if values.is_empty() {
        return Err(HepaRepairError {
            field: field.to_string(),
            message: "must not be empty".to_string(),
        });
    }
    for (index, value) in values.iter().enumerate() {
        if value.trim().is_empty() || value.contains('\n') || value.contains('\r') {
            return Err(HepaRepairError {
                field: format!("{field}[{index}]"),
                message: "must be a non-empty single line".to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hepa_core::contracts::{HepaFindingSeverity, HepaValidationCommandResult};

    #[test]
    fn rewrites_repair_prompt_from_failure_and_review_evidence() {
        let prior_prompt = "Implement the task and run tests.";
        let brief = rewrite_repair_prompt_from_evidence(HepaRepairBriefInput {
            lane_id: "lane-1".to_string(),
            repair_round: 1,
            prior_prompt: prior_prompt.to_string(),
            failing_commands: vec![HepaValidationCommandResult {
                command: "cargo test -p app".to_string(),
                exit_code: 101,
                duration_ms: 1240,
            }],
            review_findings: vec![finding()],
            diff_state: "modified src/lib.rs with failing test output".to_string(),
            files_touched: vec!["src/lib.rs".to_string(), "tests/lib.rs".to_string()],
        })
        .expect("repair brief builds");

        assert_ne!(brief.prompt, prior_prompt);
        assert!(brief.prompt.contains("cargo test -p app"));
        assert!(brief.prompt.contains("finding-1"));
        assert!(brief.prompt.contains("modified src/lib.rs"));
        assert!(brief.prompt.contains("src/lib.rs, tests/lib.rs"));
        assert!(brief.prompt.contains("Prior prompt summary"));
        assert_eq!(brief.evidence.len(), 2);
    }

    #[test]
    fn enforces_max_rounds_and_total_attempts() {
        let policy = HepaRepairRoundPolicy {
            max_repair_rounds: 2,
            max_total_attempts: 3,
        };

        let allowed = enforce_repair_round_budget(
            policy.clone(),
            HepaRepairRoundState {
                next_repair_round: 2,
                total_attempts_after_next: 3,
            },
        )
        .expect("budget evaluates");
        assert!(allowed.allowed);
        assert_eq!(allowed.block_classification, None);

        let round_exhausted = enforce_repair_round_budget(
            policy.clone(),
            HepaRepairRoundState {
                next_repair_round: 3,
                total_attempts_after_next: 3,
            },
        )
        .expect("budget evaluates");
        assert!(!round_exhausted.allowed);
        assert_eq!(
            round_exhausted.block_classification,
            Some(HepaRepairBlockClassification::RoundExhausted)
        );
        assert!(round_exhausted.reason.contains("exceeds max repair rounds"));

        let attempts_exhausted = enforce_repair_round_budget(
            policy,
            HepaRepairRoundState {
                next_repair_round: 2,
                total_attempts_after_next: 4,
            },
        )
        .expect("budget evaluates");
        assert!(!attempts_exhausted.allowed);
        assert_eq!(
            attempts_exhausted.block_classification,
            Some(HepaRepairBlockClassification::AttemptsExhausted)
        );
        assert!(
            attempts_exhausted
                .reason
                .contains("exceeds max total attempts")
        );
    }

    #[test]
    fn rejects_zero_repair_budget_limits() {
        let error = enforce_repair_round_budget(
            HepaRepairRoundPolicy {
                max_repair_rounds: 0,
                max_total_attempts: 3,
            },
            HepaRepairRoundState {
                next_repair_round: 1,
                total_attempts_after_next: 2,
            },
        )
        .expect_err("zero max repair rounds fails");

        assert_eq!(error.field, "max_repair_rounds");
    }

    #[test]
    fn blocks_with_classification_when_repair_cannot_converge() {
        let decision = enforce_repair_round_budget(
            HepaRepairRoundPolicy {
                max_repair_rounds: 2,
                max_total_attempts: 3,
            },
            HepaRepairRoundState {
                next_repair_round: 3,
                total_attempts_after_next: 3,
            },
        )
        .expect("budget evaluates");

        let block = block_when_repair_cannot_converge(decision).expect("block is classified");

        assert_eq!(
            block.classification,
            HepaRepairBlockClassification::RoundExhausted
        );
        assert!(block.reason.contains("exceeds max repair rounds"));
        assert!(block.human_attention_required);
    }

    #[test]
    fn continuing_repair_does_not_create_block() {
        let decision = enforce_repair_round_budget(
            HepaRepairRoundPolicy {
                max_repair_rounds: 2,
                max_total_attempts: 3,
            },
            HepaRepairRoundState {
                next_repair_round: 1,
                total_attempts_after_next: 2,
            },
        )
        .expect("budget evaluates");

        assert_eq!(block_when_repair_cannot_converge(decision), None);
    }

    fn finding() -> HepaReviewFinding {
        HepaReviewFinding {
            finding_id: "finding-1".to_string(),
            severity: HepaFindingSeverity::High,
            category: "correctness".to_string(),
            evidence: "The assertion fails in the new test.".to_string(),
            in_scope: true,
            release_risk: true,
            recommended_action: "Fix the implementation and rerun tests.".to_string(),
            file_ref: Some("src/lib.rs".to_string()),
            line: Some(22),
            message: "Implementation returns the wrong value.".to_string(),
            accepted: true,
        }
    }
}
