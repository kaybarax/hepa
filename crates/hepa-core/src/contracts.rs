use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

pub const CONTRACT_SCHEMA_VERSION: u32 = 1;

pub type HepaContractResult = Result<(), HepaContractError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaContractError {
    pub field: String,
    pub message: String,
}

impl HepaContractError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaContractError {}

pub trait HepaValidate {
    fn validate(&self) -> HepaContractResult;
}

pub fn validate_contract<T>(value: &T) -> HepaContractResult
where
    T: HepaValidate,
{
    value.validate()
}

pub fn to_stable_json<T>(value: &T) -> Result<String, serde_json::Error>
where
    T: Serialize,
{
    serde_json::to_string_pretty(value).map(ensure_trailing_newline)
}

pub fn to_stable_yaml<T>(value: &T) -> Result<String, serde_yaml::Error>
where
    T: Serialize,
{
    serde_yaml::to_string(value).map(ensure_trailing_newline)
}

fn ensure_trailing_newline(mut value: String) -> String {
    if !value.ends_with('\n') {
        value.push('\n');
    }
    value
}

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

impl HepaValidate for HepaProject {
    fn validate(&self) -> HepaContractResult {
        require_schema(self.schema_version)?;
        require_single_line("project_id", &self.project_id)?;
        require_non_empty("display_name", &self.display_name)?;
        require_single_line("repo_ref", &self.repo_ref)?;
        reject_secret_like_ref("repo_ref", &self.repo_ref)?;
        require_single_line("default_branch", &self.default_branch)?;
        if let Some(routing_policy_ref) = &self.routing_policy_ref {
            require_single_line("routing_policy_ref", routing_policy_ref)?;
            reject_secret_like_ref("routing_policy_ref", routing_policy_ref)?;
        }
        Ok(())
    }
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

impl HepaValidate for HepaTaskSpec {
    fn validate(&self) -> HepaContractResult {
        require_schema(self.schema_version)?;
        require_single_line("task_id", &self.task_id)?;
        require_single_line("project_id", &self.project_id)?;
        require_non_empty("goal", &self.goal)?;
        require_string_list("non_goals", &self.non_goals)?;
        require_string_list("expected_areas", &self.expected_areas)?;
        require_string_list("acceptance_criteria", &self.acceptance_criteria)?;
        require_string_list("validation_commands", &self.validation_commands)?;
        require_dependency_links("dependencies", &self.task_id, &self.dependencies)?;
        if let Some(target_branch) = &self.target_branch {
            require_single_line("target_branch", target_branch)?;
        }
        if self.max_total_rounds == 0 {
            return Err(HepaContractError::new(
                "max_total_rounds",
                "must be greater than zero",
            ));
        }
        Ok(())
    }
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
    pub external_card_id: Option<String>,
    pub priority: u32,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

impl HepaValidate for HepaFleetTask {
    fn validate(&self) -> HepaContractResult {
        require_schema(self.schema_version)?;
        require_single_line("task_id", &self.task_id)?;
        require_single_line("project_id", &self.project_id)?;
        require_single_line("title", &self.title)?;
        require_dependency_links("dependencies", &self.task_id, &self.dependencies)?;
        require_string_list("lane_ids", &self.lane_ids)?;
        if let Some(external_card_id) = &self.external_card_id {
            require_single_line("external_card_id", external_card_id)?;
            reject_secret_like_ref("external_card_id", external_card_id)?;
        }
        if matches!(self.status, HepaTaskStatus::Completed) && self.completed_at.is_none() {
            return Err(HepaContractError::new(
                "completed_at",
                "completed tasks must record completion time",
            ));
        }
        Ok(())
    }
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

impl HepaTaskStatus {
    pub fn can_transition_to(&self, next: &Self) -> bool {
        use HepaTaskStatus::*;
        matches!(
            (self, next),
            (Draft, Queued | Blocked | Cancelled)
                | (Queued, Ready | Blocked | Cancelled)
                | (Ready, Running | Blocked | Cancelled)
                | (Running, Blocked | Completed | Cancelled)
                | (Blocked, Queued | Ready | Cancelled)
        )
    }
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

impl HepaValidate for HepaLane {
    fn validate(&self) -> HepaContractResult {
        require_schema(self.schema_version)?;
        require_single_line("lane_id", &self.lane_id)?;
        require_single_line("project_id", &self.project_id)?;
        require_single_line("task_id", &self.task_id)?;
        require_single_line("adapter_id", &self.adapter_id)?;
        require_single_line("worktree_ref", &self.worktree_ref)?;
        reject_secret_like_ref("worktree_ref", &self.worktree_ref)?;
        require_single_line("branch", &self.branch)?;
        require_single_line("run_dir_ref", &self.run_dir_ref)?;
        reject_secret_like_ref("run_dir_ref", &self.run_dir_ref)?;
        if matches!(
            self.state,
            HepaLaneState::Completed | HepaLaneState::Cleaned
        ) && self.completed_at.is_none()
        {
            return Err(HepaContractError::new(
                "completed_at",
                "terminal lanes must record completion time",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaLaneSteeringRecord {
    pub schema_version: u32,
    pub lane_id: String,
    pub session_id: String,
    pub message: String,
    pub manager_approved: bool,
    pub dry_run: bool,
    pub lane_state: HepaLaneState,
}

impl HepaValidate for HepaLaneSteeringRecord {
    fn validate(&self) -> HepaContractResult {
        require_schema(self.schema_version)?;
        require_single_line("lane_id", &self.lane_id)?;
        require_single_line("session_id", &self.session_id)?;
        require_non_empty("message", &self.message)?;
        reject_secret_like_ref("message", &self.message)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaLaneState {
    DraftSpec,
    Ready,
    Allocated,
    Starting,
    Running,
    Validating,
    Reviewing,
    Repairing,
    Staging,
    PrCreated,
    ReadyForHuman,
    Blocked,
    Failed,
    Cancelled,
    Cleaned,
    Completed,
}

impl HepaLaneState {
    pub fn can_transition_to(&self, next: &Self) -> bool {
        use HepaLaneState::*;
        matches!(
            (self, next),
            (DraftSpec, Ready | Blocked | Cancelled | Cleaned)
                | (Ready, Allocated | Blocked | Cancelled | Cleaned)
                | (Allocated, Starting | Blocked | Failed | Cancelled | Cleaned)
                | (Starting, Running | Blocked | Failed | Cleaned)
                | (Running, Validating | Blocked | Failed | Cleaned)
                | (
                    Validating,
                    Reviewing | Repairing | Blocked | Failed | Cleaned
                )
                | (Reviewing, Repairing | Staging | Blocked | Failed | Cleaned)
                | (Repairing, Running | Validating | Blocked | Failed | Cleaned)
                | (Staging, PrCreated | Blocked | Failed | Cleaned)
                | (
                    PrCreated,
                    ReadyForHuman | Completed | Blocked | Failed | Cleaned
                )
                | (ReadyForHuman, Completed | Blocked | Failed | Cleaned)
                | (Blocked, Running | Repairing | Cleaned)
                | (Failed, Repairing | Cleaned)
                | (Cancelled, Cleaned)
                | (Completed, Cleaned)
        )
    }
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

impl HepaValidate for HepaAttemptReport {
    fn validate(&self) -> HepaContractResult {
        require_schema(self.schema_version)?;
        require_single_line("attempt_id", &self.attempt_id)?;
        require_single_line("lane_id", &self.lane_id)?;
        require_single_line("task_id", &self.task_id)?;
        require_single_line("adapter_id", &self.adapter_id)?;
        require_string_list("commands_run", &self.commands_run)?;
        require_string_list("changed_files", &self.changed_files)?;
        for changed_file in &self.changed_files {
            reject_secret_like_ref("changed_files", changed_file)?;
        }
        if matches!(
            self.status,
            HepaAttemptStatus::Blocked | HepaAttemptStatus::Failed
        ) && self.blocked_reason.is_none()
        {
            return Err(HepaContractError::new(
                "blocked_reason",
                "blocked or failed attempts must record a reason",
            ));
        }
        Ok(())
    }
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

impl HepaValidate for HepaValidationSummary {
    fn validate(&self) -> HepaContractResult {
        require_schema(self.schema_version)?;
        for (index, command) in self.commands.iter().enumerate() {
            require_single_line(format!("commands[{index}].command"), &command.command)?;
        }
        if matches!(self.status, HepaValidationStatus::NoTestsDetected) && !self.no_tests_detected {
            return Err(HepaContractError::new(
                "no_tests_detected",
                "must be true when status is no_tests_detected",
            ));
        }
        Ok(())
    }
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

impl HepaValidate for HepaReviewSignal {
    fn validate(&self) -> HepaContractResult {
        require_schema(self.schema_version)?;
        require_single_line("review_id", &self.review_id)?;
        require_single_line("lane_id", &self.lane_id)?;
        require_single_line("adapter_id", &self.adapter_id)?;
        for finding in &self.findings {
            finding.validate()?;
        }
        Ok(())
    }
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

impl HepaValidate for HepaReviewFinding {
    fn validate(&self) -> HepaContractResult {
        require_single_line("finding_id", &self.finding_id)?;
        if let Some(file_ref) = &self.file_ref {
            require_single_line("file_ref", file_ref)?;
            reject_secret_like_ref("file_ref", file_ref)?;
        }
        require_non_empty("message", &self.message)?;
        Ok(())
    }
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

impl HepaValidate for HepaReadinessResult {
    fn validate(&self) -> HepaContractResult {
        require_schema(self.schema_version)?;
        require_single_line("task_id", &self.task_id)?;
        require_string_list("blockers", &self.blockers)?;
        require_string_list("questions", &self.questions)?;
        if matches!(
            self.status,
            HepaReadinessStatus::NeedsClarification | HepaReadinessStatus::Blocked
        ) && self.blockers.is_empty()
            && self.questions.is_empty()
        {
            return Err(HepaContractError::new(
                "status",
                "not-ready readiness results must include a blocker or question",
            ));
        }
        Ok(())
    }
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

impl HepaValidate for HepaTimingRecord {
    fn validate(&self) -> HepaContractResult {
        require_schema(self.schema_version)?;
        require_single_line("run_id", &self.run_id)?;
        for phase in &self.phases {
            phase.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HepaTimingPhase {
    pub name: String,
    pub status: HepaPhaseStatus,
    pub duration_seconds: f64,
    pub round: Option<u32>,
    pub role: Option<HepaAgentRole>,
    pub adapter_id: Option<String>,
    pub routing_reason: Option<String>,
    pub sandbox_posture: Option<String>,
}

impl HepaValidate for HepaTimingPhase {
    fn validate(&self) -> HepaContractResult {
        require_single_line("name", &self.name)?;
        if self.duration_seconds < 0.0 {
            return Err(HepaContractError::new(
                "duration_seconds",
                "must not be negative",
            ));
        }
        if let Some(adapter_id) = &self.adapter_id {
            require_single_line("adapter_id", adapter_id)?;
        }
        if let Some(routing_reason) = &self.routing_reason {
            require_single_line("routing_reason", routing_reason)?;
        }
        if let Some(sandbox_posture) = &self.sandbox_posture {
            require_single_line("sandbox_posture", sandbox_posture)?;
        }
        Ok(())
    }
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
    pub install_events: u32,
    pub container_count: u32,
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

impl HepaValidate for HepaTerminalTaskReport {
    fn validate(&self) -> HepaContractResult {
        require_schema(self.schema_version)?;
        require_single_line("task_id", &self.task_id)?;
        require_single_line("lane_id", &self.lane_id)?;
        if let Some(validation) = &self.validation {
            validation.validate()?;
        }
        for review_signal in &self.review_signals {
            review_signal.validate()?;
        }
        if let Some(timing) = &self.timing {
            timing.validate()?;
        }
        if matches!(
            self.status,
            HepaTerminalStatus::Blocked | HepaTerminalStatus::Failed
        ) && !self.human_attention_required
        {
            return Err(HepaContractError::new(
                "human_attention_required",
                "blocked or failed terminal reports must request attention",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaTerminalStatus {
    Completed,
    Blocked,
    Failed,
    Cancelled,
}

fn require_schema(schema_version: u32) -> HepaContractResult {
    if schema_version == CONTRACT_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(HepaContractError::new(
            "schema_version",
            format!("must be {CONTRACT_SCHEMA_VERSION}"),
        ))
    }
}

fn require_non_empty(field: impl Into<String>, value: &str) -> HepaContractResult {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaContractError::new(field, "must not be empty"));
    }
    Ok(())
}

fn require_single_line(field: impl Into<String>, value: &str) -> HepaContractResult {
    let field = field.into();
    require_non_empty(field.clone(), value)?;
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaContractError::new(field, "must be a single line"));
    }
    Ok(())
}

fn require_string_list(field: &str, values: &[String]) -> HepaContractResult {
    for (index, value) in values.iter().enumerate() {
        require_single_line(format!("{field}[{index}]"), value)?;
    }
    Ok(())
}

fn require_dependency_links(
    field: &str,
    owner_id: &str,
    dependencies: &[String],
) -> HepaContractResult {
    require_string_list(field, dependencies)?;
    for dependency in dependencies {
        if dependency == owner_id {
            return Err(HepaContractError::new(field, "must not reference itself"));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for dependency in dependencies {
        if !seen.insert(dependency) {
            return Err(HepaContractError::new(field, "must not contain duplicates"));
        }
    }
    Ok(())
}

fn reject_secret_like_ref(field: impl Into<String>, value: &str) -> HepaContractResult {
    let lowered = value.to_ascii_lowercase();
    let secret_like = [
        ".env",
        "credential",
        "id_rsa",
        "password",
        "private_key",
        "secret",
        "token",
    ]
    .iter()
    .any(|needle| lowered.contains(needle));
    if secret_like {
        Err(HepaContractError::new(
            field,
            "must not contain secret-like path or value fragments",
        ))
    } else {
        Ok(())
    }
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
    fn task_spec_has_stable_json_and_yaml_field_names() {
        let spec = HepaTaskSpec {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            goal: "Update docs".to_string(),
            non_goals: vec!["No code changes".to_string()],
            expected_areas: vec!["README.md".to_string()],
            acceptance_criteria: vec!["README.md updated".to_string()],
            validation_commands: vec!["git diff --check".to_string()],
            dependencies: vec!["task-0".to_string()],
            target_branch: Some("main".to_string()),
            risk_level: HepaRiskLevel::Low,
            max_total_rounds: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
        };

        let json = to_stable_json(&spec).expect("stable JSON should serialize");
        let yaml = to_stable_yaml(&spec).expect("stable YAML should serialize");

        assert_eq!(
            json,
            concat!(
                "{\n",
                "  \"schema_version\": 1,\n",
                "  \"task_id\": \"task-1\",\n",
                "  \"project_id\": \"project-1\",\n",
                "  \"goal\": \"Update docs\",\n",
                "  \"non_goals\": [\n",
                "    \"No code changes\"\n",
                "  ],\n",
                "  \"expected_areas\": [\n",
                "    \"README.md\"\n",
                "  ],\n",
                "  \"acceptance_criteria\": [\n",
                "    \"README.md updated\"\n",
                "  ],\n",
                "  \"validation_commands\": [\n",
                "    \"git diff --check\"\n",
                "  ],\n",
                "  \"dependencies\": [\n",
                "    \"task-0\"\n",
                "  ],\n",
                "  \"target_branch\": \"main\",\n",
                "  \"risk_level\": \"low\",\n",
                "  \"max_total_rounds\": 1,\n",
                "  \"created_at\": \"2026-06-16T00:00:00Z\"\n",
                "}\n"
            )
        );
        assert_eq!(
            yaml,
            concat!(
                "schema_version: 1\n",
                "task_id: task-1\n",
                "project_id: project-1\n",
                "goal: Update docs\n",
                "non_goals:\n",
                "- No code changes\n",
                "expected_areas:\n",
                "- README.md\n",
                "acceptance_criteria:\n",
                "- README.md updated\n",
                "validation_commands:\n",
                "- git diff --check\n",
                "dependencies:\n",
                "- task-0\n",
                "target_branch: main\n",
                "risk_level: low\n",
                "max_total_rounds: 1\n",
                "created_at: 2026-06-16T00:00:00Z\n"
            )
        );
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
                    routing_reason: Some("default fake adapter".to_string()),
                    sandbox_posture: Some("host-worktree".to_string()),
                }],
                counters: HepaTimingCounters {
                    agent_loops: 1,
                    manager_passes: 0,
                    reviewer_passes: 0,
                    install_events: 0,
                    container_count: 0,
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

    #[test]
    fn invalid_contracts_fail_with_clear_field_errors() {
        let spec = HepaTaskSpec {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            goal: "Update docs".to_string(),
            non_goals: Vec::new(),
            expected_areas: Vec::new(),
            acceptance_criteria: Vec::new(),
            validation_commands: Vec::new(),
            dependencies: vec!["task-1".to_string()],
            target_branch: None,
            risk_level: HepaRiskLevel::Low,
            max_total_rounds: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
        };

        let error = spec.validate().expect_err("self-dependency must fail");

        assert_eq!(error.field, "dependencies");
        assert!(error.message.contains("itself"));
    }

    #[test]
    fn fleet_task_records_stable_external_card_id() {
        let task = HepaFleetTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            title: "Update docs".to_string(),
            description: "Documentation task".to_string(),
            status: HepaTaskStatus::Queued,
            readiness: HepaReadinessState::NotReady,
            dependencies: Vec::new(),
            lane_ids: Vec::new(),
            external_card_id: Some("hermes-card-1".to_string()),
            priority: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: None,
        };

        task.validate().expect("stable card IDs should validate");

        let json = serde_json::to_string(&task).expect("task should serialize");
        assert!(json.contains("\"external_card_id\":\"hermes-card-1\""));
    }

    #[test]
    fn fleet_task_rejects_secret_like_external_card_id() {
        let task = HepaFleetTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            title: "Update docs".to_string(),
            description: "Documentation task".to_string(),
            status: HepaTaskStatus::Queued,
            readiness: HepaReadinessState::NotReady,
            dependencies: Vec::new(),
            lane_ids: Vec::new(),
            external_card_id: Some("card-secret-ref".to_string()),
            priority: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: None,
        };

        let error = task.validate().expect_err("secret-like card IDs must fail");

        assert_eq!(error.field, "external_card_id");
        assert!(error.message.contains("secret-like"));
    }

    #[test]
    fn secret_like_refs_are_rejected() {
        let lane = HepaLane {
            schema_version: CONTRACT_SCHEMA_VERSION,
            lane_id: "lane-1".to_string(),
            project_id: "project-1".to_string(),
            task_id: "task-1".to_string(),
            adapter_id: "fake".to_string(),
            state: HepaLaneState::Running,
            worktree_ref: "safe-worktree".to_string(),
            branch: "task-branch".to_string(),
            run_dir_ref: "run-token-cache".to_string(),
            attempt_count: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:01Z".to_string(),
            completed_at: None,
        };

        let error = lane
            .validate()
            .expect_err("secret-like run dir refs must fail");

        assert_eq!(error.field, "run_dir_ref");
        assert!(error.message.contains("secret-like"));
    }

    #[test]
    fn status_transitions_reject_terminal_regression() {
        assert!(HepaLaneState::Allocated.can_transition_to(&HepaLaneState::Starting));
        assert!(!HepaLaneState::Completed.can_transition_to(&HepaLaneState::Running));
        assert!(HepaTaskStatus::Running.can_transition_to(&HepaTaskStatus::Completed));
        assert!(!HepaTaskStatus::Completed.can_transition_to(&HepaTaskStatus::Running));
    }

    #[test]
    fn lane_states_use_phase_three_stable_names() {
        let states = [
            (HepaLaneState::DraftSpec, "\"draft_spec\""),
            (HepaLaneState::Ready, "\"ready\""),
            (HepaLaneState::Allocated, "\"allocated\""),
            (HepaLaneState::Starting, "\"starting\""),
            (HepaLaneState::Running, "\"running\""),
            (HepaLaneState::Validating, "\"validating\""),
            (HepaLaneState::Reviewing, "\"reviewing\""),
            (HepaLaneState::Repairing, "\"repairing\""),
            (HepaLaneState::Staging, "\"staging\""),
            (HepaLaneState::PrCreated, "\"pr_created\""),
            (HepaLaneState::ReadyForHuman, "\"ready_for_human\""),
            (HepaLaneState::Blocked, "\"blocked\""),
            (HepaLaneState::Failed, "\"failed\""),
            (HepaLaneState::Cancelled, "\"cancelled\""),
            (HepaLaneState::Cleaned, "\"cleaned\""),
            (HepaLaneState::Completed, "\"completed\""),
        ];

        for (state, expected_json) in states {
            let json = serde_json::to_string(&state).expect("state should serialize");
            assert_eq!(json, expected_json);
        }
    }

    #[test]
    fn timing_record_carries_structural_phase_and_counter_telemetry() {
        let timing = HepaTimingRecord {
            schema_version: CONTRACT_SCHEMA_VERSION,
            run_id: "run-1".to_string(),
            phases: vec![HepaTimingPhase {
                name: "worker_attempt".to_string(),
                status: HepaPhaseStatus::Completed,
                duration_seconds: 3.25,
                round: Some(1),
                role: Some(HepaAgentRole::Worker),
                adapter_id: Some("fake".to_string()),
                routing_reason: Some("matched docs task capability".to_string()),
                sandbox_posture: Some("host-worktree".to_string()),
            }],
            counters: HepaTimingCounters {
                agent_loops: 1,
                manager_passes: 1,
                reviewer_passes: 2,
                install_events: 1,
                container_count: 0,
            },
        };

        timing.validate().expect("timing telemetry should validate");
        let json = to_stable_json(&timing).expect("timing should serialize");

        assert!(json.contains("\"adapter_id\": \"fake\""));
        assert!(json.contains("\"routing_reason\": \"matched docs task capability\""));
        assert!(json.contains("\"sandbox_posture\": \"host-worktree\""));
        assert!(json.contains("\"agent_loops\": 1"));
        assert!(json.contains("\"manager_passes\": 1"));
        assert!(json.contains("\"install_events\": 1"));
        assert!(json.contains("\"container_count\": 0"));
    }
}
