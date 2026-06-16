use hepa_core::contracts::{
    CONTRACT_SCHEMA_VERSION, HepaAgentRole, HepaAttemptReport, HepaAttemptStatus,
    HepaFindingSeverity, HepaReviewFinding, HepaReviewSignal, HepaReviewStatus, HepaTaskSpec,
    HepaValidate,
};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaFakeAdapter {
    adapter_id: String,
}

impl Default for HepaFakeAdapter {
    fn default() -> Self {
        Self {
            adapter_id: "fake".to_string(),
        }
    }
}

impl HepaFakeAdapter {
    pub fn new(adapter_id: impl Into<String>) -> Result<Self, HepaFakeAdapterError> {
        let adapter_id = adapter_id.into();
        require_single_line("adapter_id", &adapter_id)?;
        Ok(Self { adapter_id })
    }

    pub fn run_worker_attempt(
        &self,
        input: &HepaFakeWorkerInput,
    ) -> Result<HepaAttemptReport, HepaFakeAdapterError> {
        input.validate()?;
        let report = HepaAttemptReport {
            schema_version: CONTRACT_SCHEMA_VERSION,
            attempt_id: input.attempt_id.clone(),
            lane_id: input.lane_id.clone(),
            task_id: input.task_spec.task_id.clone(),
            round: input.round,
            role: HepaAgentRole::Worker,
            adapter_id: self.adapter_id.clone(),
            status: HepaAttemptStatus::Completed,
            commands_run: input.task_spec.validation_commands.clone(),
            changed_files: input.task_spec.expected_areas.clone(),
            summary: vec![format!(
                "Fake worker completed task {} deterministically.",
                input.task_spec.task_id
            )],
            blocked_reason: None,
            started_at: input.started_at.clone(),
            completed_at: Some(input.completed_at.clone()),
        };
        report.validate()?;
        Ok(report)
    }

    pub fn run_reviewer(
        &self,
        input: &HepaFakeReviewerInput,
    ) -> Result<HepaReviewSignal, HepaFakeAdapterError> {
        input.validate()?;
        let signal = HepaReviewSignal {
            schema_version: CONTRACT_SCHEMA_VERSION,
            review_id: input.review_id.clone(),
            lane_id: input.lane_id.clone(),
            adapter_id: self.adapter_id.clone(),
            status: HepaReviewStatus::Approved,
            findings: vec![HepaReviewFinding {
                finding_id: "fake-review-1".to_string(),
                severity: HepaFindingSeverity::Low,
                file_ref: None,
                line: None,
                message: "Deterministic fake review found no blocking issues.".to_string(),
                accepted: true,
            }],
            summary: vec![format!("Fake reviewer approved lane {}.", input.lane_id)],
            completed_at: input.completed_at.clone(),
        };
        signal.validate()?;
        Ok(signal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaFakeWorkerInput {
    pub task_spec: HepaTaskSpec,
    pub lane_id: String,
    pub attempt_id: String,
    pub round: u32,
    pub started_at: String,
    pub completed_at: String,
}

impl HepaFakeWorkerInput {
    fn validate(&self) -> Result<(), HepaFakeAdapterError> {
        self.task_spec
            .validate()
            .map_err(HepaFakeAdapterError::from)?;
        require_single_line("lane_id", &self.lane_id)?;
        require_single_line("attempt_id", &self.attempt_id)?;
        if self.round == 0 {
            return Err(HepaFakeAdapterError::new(
                "round",
                "must be greater than zero",
            ));
        }
        require_single_line("started_at", &self.started_at)?;
        require_single_line("completed_at", &self.completed_at)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaFakeReviewerInput {
    pub lane_id: String,
    pub review_id: String,
    pub completed_at: String,
}

impl HepaFakeReviewerInput {
    fn validate(&self) -> Result<(), HepaFakeAdapterError> {
        require_single_line("lane_id", &self.lane_id)?;
        require_single_line("review_id", &self.review_id)?;
        require_single_line("completed_at", &self.completed_at)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaFakeAdapterError {
    pub field: String,
    pub message: String,
}

impl HepaFakeAdapterError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaFakeAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaFakeAdapterError {}

impl From<hepa_core::contracts::HepaContractError> for HepaFakeAdapterError {
    fn from(error: hepa_core::contracts::HepaContractError) -> Self {
        Self::new(error.field, error.message)
    }
}

fn require_single_line(field: &str, value: &str) -> Result<(), HepaFakeAdapterError> {
    if value.trim().is_empty() {
        return Err(HepaFakeAdapterError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaFakeAdapterError::new(field, "must be a single line"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hepa_core::contracts::HepaRiskLevel;

    #[test]
    fn fake_worker_produces_deterministic_attempt_report() {
        let report = HepaFakeAdapter::default()
            .run_worker_attempt(&worker_input())
            .expect("fake worker should complete");

        assert_eq!(report.status, HepaAttemptStatus::Completed);
        assert_eq!(report.role, HepaAgentRole::Worker);
        assert_eq!(report.adapter_id, "fake");
        assert_eq!(report.commands_run, vec!["cargo test".to_string()]);
        assert_eq!(report.changed_files, vec!["README.md".to_string()]);
        assert_eq!(report.completed_at.as_deref(), Some("2026-06-16T00:00:01Z"));
    }

    #[test]
    fn fake_reviewer_produces_approved_review_signal() {
        let signal = HepaFakeAdapter::default()
            .run_reviewer(&HepaFakeReviewerInput {
                lane_id: "lane-1".to_string(),
                review_id: "review-1".to_string(),
                completed_at: "2026-06-16T00:00:02Z".to_string(),
            })
            .expect("fake reviewer should approve");

        assert_eq!(signal.status, HepaReviewStatus::Approved);
        assert_eq!(signal.adapter_id, "fake");
        assert_eq!(signal.findings.len(), 1);
        assert!(signal.summary[0].contains("approved"));
    }

    #[test]
    fn fake_adapter_rejects_invalid_inputs() {
        let error = HepaFakeAdapter::default()
            .run_reviewer(&HepaFakeReviewerInput {
                lane_id: "lane\n1".to_string(),
                review_id: "review-1".to_string(),
                completed_at: "2026-06-16T00:00:02Z".to_string(),
            })
            .expect_err("invalid lane should fail");

        assert_eq!(error.field, "lane_id");
    }

    fn worker_input() -> HepaFakeWorkerInput {
        HepaFakeWorkerInput {
            task_spec: HepaTaskSpec {
                schema_version: CONTRACT_SCHEMA_VERSION,
                task_id: "task-1".to_string(),
                project_id: "project-1".to_string(),
                goal: "Update docs".to_string(),
                non_goals: Vec::new(),
                expected_areas: vec!["README.md".to_string()],
                acceptance_criteria: vec!["Docs updated".to_string()],
                validation_commands: vec!["cargo test".to_string()],
                dependencies: Vec::new(),
                target_branch: Some("main".to_string()),
                risk_level: HepaRiskLevel::Low,
                max_total_rounds: 1,
                created_at: "2026-06-16T00:00:00Z".to_string(),
            },
            lane_id: "lane-1".to_string(),
            attempt_id: "attempt-1".to_string(),
            round: 1,
            started_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: "2026-06-16T00:00:01Z".to_string(),
        }
    }
}
