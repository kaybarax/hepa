use serde::{Deserialize, Serialize};

pub const CONTRACT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaProject {
    pub schema_version: u32,
    pub project_id: String,
    pub display_name: String,
    pub repo_ref: String,
    pub default_branch: String,
    pub routing_policy_ref: Option<String>,
    pub is_active: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaTaskSpec {
    pub schema_version: u32,
    pub task_id: String,
    pub project_id: String,
    pub goal: String,
    pub non_goals: Vec<String>,
    pub expected_areas: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub validation_commands: Vec<String>,
    pub dependencies: Vec<String>,
    pub target_branch: Option<String>,
    pub risk_level: HepaRiskLevel,
    pub max_total_rounds: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaRiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaFleetTask {
    pub schema_version: u32,
    pub task_id: String,
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub status: HepaTaskStatus,
    pub readiness: HepaReadinessState,
    pub dependencies: Vec<String>,
    pub lane_ids: Vec<String>,
    pub priority: u32,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaTaskStatus {
    Draft,
    Queued,
    Ready,
    Running,
    Blocked,
    Cancelled,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaReadinessState {
    NotReady,
    Ready,
    DraftReady,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaLane {
    pub schema_version: u32,
    pub lane_id: String,
    pub project_id: String,
    pub task_id: String,
    pub adapter_id: String,
    pub state: HepaLaneState,
    pub worktree_ref: String,
    pub branch: String,
    pub run_dir_ref: String,
    pub attempt_count: u32,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaLaneState {
    Allocated,
    Starting,
    Running,
    Validating,
    Reviewing,
    Repairing,
    NeedsHumanStaging,
    PrReady,
    ReadyForHuman,
    Blocked,
    Failed,
    Cleaned,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaAttemptReport {
    pub schema_version: u32,
    pub attempt_id: String,
    pub lane_id: String,
    pub task_id: String,
    pub round: u32,
    pub role: HepaAgentRole,
    pub adapter_id: String,
    pub status: HepaAttemptStatus,
    pub commands_run: Vec<String>,
    pub changed_files: Vec<String>,
    pub summary: Vec<String>,
    pub blocked_reason: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaAgentRole {
    Manager,
    Worker,
    Reviewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaAttemptStatus {
    Completed,
    Blocked,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaValidationSummary {
    pub schema_version: u32,
    pub status: HepaValidationStatus,
    pub commands: Vec<HepaValidationCommandResult>,
    pub no_tests_detected: bool,
    pub failure_type: Option<String>,
    pub summary: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaValidationStatus {
    Passed,
    Failed,
    Skipped,
    NoTestsDetected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaValidationCommandResult {
    pub command: String,
    pub exit_code: i32,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaReviewSignal {
    pub schema_version: u32,
    pub review_id: String,
    pub lane_id: String,
    pub adapter_id: String,
    pub status: HepaReviewStatus,
    pub findings: Vec<HepaReviewFinding>,
    pub summary: Vec<String>,
    pub completed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaReviewStatus {
    Approved,
    ChangesRequested,
    Blocked,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaReviewFinding {
    pub finding_id: String,
    pub severity: HepaFindingSeverity,
    pub file_ref: Option<String>,
    pub line: Option<u32>,
    pub message: String,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaFindingSeverity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaReadinessResult {
    pub schema_version: u32,
    pub task_id: String,
    pub status: HepaReadinessStatus,
    pub blockers: Vec<String>,
    pub questions: Vec<String>,
    pub checked_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaReadinessStatus {
    Ready,
    NeedsClarification,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HepaTimingRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub phases: Vec<HepaTimingPhase>,
    pub counters: HepaTimingCounters,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HepaTimingPhase {
    pub name: String,
    pub status: HepaPhaseStatus,
    pub duration_seconds: f64,
    pub round: Option<u32>,
    pub role: Option<HepaAgentRole>,
    pub adapter_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaPhaseStatus {
    Completed,
    Failed,
    Blocked,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaTimingCounters {
    pub agent_loops: u32,
    pub manager_passes: u32,
    pub reviewer_passes: u32,
    pub container_starts: u32,
    pub dependency_installs: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HepaTerminalTaskReport {
    pub schema_version: u32,
    pub task_id: String,
    pub lane_id: String,
    pub status: HepaTerminalStatus,
    pub pr_url: Option<String>,
    pub validation: Option<HepaValidationSummary>,
    pub review_signals: Vec<HepaReviewSignal>,
    pub timing: Option<HepaTimingRecord>,
    pub summary: Vec<String>,
    pub human_attention_required: bool,
    pub completed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaTerminalStatus {
    Completed,
    Blocked,
    Failed,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_spec_uses_snake_case_enum_values() {
        let spec = HepaTaskSpec {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            goal: "Update docs".to_string(),
            non_goals: vec!["No code changes".to_string()],
            expected_areas: vec!["README.md".to_string()],
            acceptance_criteria: vec!["README.md updated".to_string()],
            validation_commands: vec!["git diff --check".to_string()],
            dependencies: Vec::new(),
            target_branch: Some("main".to_string()),
            risk_level: HepaRiskLevel::Low,
            max_total_rounds: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
        };

        let json = serde_json::to_string(&spec).expect("task spec should serialize");

        assert!(json.contains("\"risk_level\":\"low\""));
        assert!(json.contains("\"validation_commands\""));
    }

    #[test]
    fn terminal_report_can_carry_nested_gate_outputs() {
        let report = HepaTerminalTaskReport {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            status: HepaTerminalStatus::Blocked,
            pr_url: None,
            validation: Some(HepaValidationSummary {
                schema_version: CONTRACT_SCHEMA_VERSION,
                status: HepaValidationStatus::NoTestsDetected,
                commands: Vec::new(),
                no_tests_detected: true,
                failure_type: None,
                summary: vec!["No tests detected.".to_string()],
            }),
            review_signals: Vec::new(),
            timing: Some(HepaTimingRecord {
                schema_version: CONTRACT_SCHEMA_VERSION,
                run_id: "run-1".to_string(),
                phases: vec![HepaTimingPhase {
                    name: "worker_attempt".to_string(),
                    status: HepaPhaseStatus::Blocked,
                    duration_seconds: 12.5,
                    round: Some(1),
                    role: Some(HepaAgentRole::Worker),
                    adapter_id: Some("fake".to_string()),
                }],
                counters: HepaTimingCounters {
                    agent_loops: 1,
                    manager_passes: 0,
                    reviewer_passes: 0,
                    container_starts: 0,
                    dependency_installs: 0,
                },
            }),
            summary: vec!["Blocked by fake adapter.".to_string()],
            human_attention_required: true,
            completed_at: "2026-06-16T00:01:00Z".to_string(),
        };

        let round_trip: HepaTerminalTaskReport =
            serde_json::from_str(&serde_json::to_string(&report).expect("serialize report"))
                .expect("deserialize report");

        assert_eq!(round_trip, report);
    }
}
