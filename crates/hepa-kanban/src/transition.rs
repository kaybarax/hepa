use hepa_core::contracts::{
    HepaFleetTask, HepaLane, HepaLaneState, HepaReadinessState, HepaTaskStatus,
};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaBoardTransitionRequest {
    pub request_id: String,
    pub task_id: String,
    pub card_id: Option<String>,
    pub requested_by: String,
    pub action: HepaBoardTransitionAction,
    pub requested_at: String,
}

impl HepaBoardTransitionRequest {
    pub fn validate(&self) -> Result<(), HepaBoardTransitionError> {
        require_single_line("request_id", &self.request_id)?;
        require_single_line("task_id", &self.task_id)?;
        if let Some(card_id) = &self.card_id {
            require_single_line("card_id", card_id)?;
        }
        require_single_line("requested_by", &self.requested_by)?;
        self.action.validate()?;
        require_single_line("requested_at", &self.requested_at)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum HepaBoardTransitionAction {
    SetPriority { priority: u32 },
    MarkReady,
    Block { reason: String },
    Cancel { reason: String },
    Resume,
    SetDependencies { dependencies: Vec<String> },
}

impl HepaBoardTransitionAction {
    fn validate(&self) -> Result<(), HepaBoardTransitionError> {
        match self {
            Self::SetPriority { .. } | Self::MarkReady | Self::Resume => Ok(()),
            Self::Block { reason } => require_single_line("action.reason", reason),
            Self::Cancel { reason } => require_single_line("action.reason", reason),
            Self::SetDependencies { dependencies } => {
                let mut seen = std::collections::BTreeSet::new();
                for (index, dependency) in dependencies.iter().enumerate() {
                    require_single_line(format!("action.dependencies[{index}]"), dependency)?;
                    if !seen.insert(dependency) {
                        return Err(HepaBoardTransitionError::new(
                            "action.dependencies",
                            "must not contain duplicates",
                        ));
                    }
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaBoardTransitionDecision {
    pub request_id: String,
    pub task_id: String,
    pub status: HepaBoardTransitionDecisionStatus,
    pub reason: String,
    pub updated_task: Option<HepaFleetTask>,
}

impl HepaBoardTransitionDecision {
    pub fn to_card_response(&self) -> HepaBoardTransitionCardResponse {
        let status = match self.status {
            HepaBoardTransitionDecisionStatus::Accepted => "accepted",
            HepaBoardTransitionDecisionStatus::Rejected => "rejected",
        };
        HepaBoardTransitionCardResponse {
            request_id: self.request_id.clone(),
            task_id: self.task_id.clone(),
            visible_reason: format!("Board transition {status}: {}", self.reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaBoardTransitionCardResponse {
    pub request_id: String,
    pub task_id: String,
    pub visible_reason: String,
}

pub const BOARD_TRANSITION_ARTIFACT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaBoardTransitionArtifact {
    pub schema_version: u32,
    pub request: HepaBoardTransitionRequest,
    pub decision: HepaBoardTransitionDecision,
}

pub fn board_transition_artifact(
    request: HepaBoardTransitionRequest,
    decision: HepaBoardTransitionDecision,
) -> HepaBoardTransitionArtifact {
    HepaBoardTransitionArtifact {
        schema_version: BOARD_TRANSITION_ARTIFACT_SCHEMA_VERSION,
        request,
        decision,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaBoardTransitionDecisionStatus {
    Accepted,
    Rejected,
}

pub fn evaluate_board_transition(
    task: &HepaFleetTask,
    request: &HepaBoardTransitionRequest,
) -> Result<HepaBoardTransitionDecision, HepaBoardTransitionError> {
    evaluate_board_transition_with_lanes(task, &[], request)
}

pub fn evaluate_board_transition_with_lanes(
    task: &HepaFleetTask,
    lanes: &[HepaLane],
    request: &HepaBoardTransitionRequest,
) -> Result<HepaBoardTransitionDecision, HepaBoardTransitionError> {
    request.validate()?;
    if task.task_id != request.task_id {
        return Ok(rejected(
            request,
            "request task_id does not match HEPA task record",
        ));
    }
    if board_request_would_bypass_lane_gates(&request.action, lanes) {
        return Ok(rejected(
            request,
            "cards cannot bypass validation, review, safety, or done gates",
        ));
    }

    let mut updated_task = task.clone();
    match &request.action {
        HepaBoardTransitionAction::SetPriority { priority } => {
            updated_task.priority = *priority;
            Ok(accepted(request, "priority updated", updated_task))
        }
        HepaBoardTransitionAction::MarkReady => {
            updated_task.readiness = HepaReadinessState::Ready;
            transition_status(
                request,
                updated_task,
                HepaTaskStatus::Ready,
                "task marked ready",
            )
        }
        HepaBoardTransitionAction::Block { .. } => {
            updated_task.readiness = HepaReadinessState::Blocked;
            transition_status(
                request,
                updated_task,
                HepaTaskStatus::Blocked,
                "task blocked",
            )
        }
        HepaBoardTransitionAction::Cancel { .. } => transition_status(
            request,
            updated_task,
            HepaTaskStatus::Cancelled,
            "task cancelled",
        ),
        HepaBoardTransitionAction::Resume => {
            let next_status = if matches!(updated_task.readiness, HepaReadinessState::Ready) {
                HepaTaskStatus::Ready
            } else {
                HepaTaskStatus::Queued
            };
            transition_status(request, updated_task, next_status, "task resumed")
        }
        HepaBoardTransitionAction::SetDependencies { dependencies } => {
            if dependencies
                .iter()
                .any(|dependency| dependency == &task.task_id)
            {
                return Ok(rejected(
                    request,
                    "dependencies must not reference the task itself",
                ));
            }
            updated_task.dependencies = dependencies.clone();
            Ok(accepted(request, "dependencies updated", updated_task))
        }
    }
}

fn board_request_would_bypass_lane_gates(
    action: &HepaBoardTransitionAction,
    lanes: &[HepaLane],
) -> bool {
    matches!(
        action,
        HepaBoardTransitionAction::MarkReady | HepaBoardTransitionAction::Resume
    ) && lanes.iter().any(|lane| {
        matches!(
            lane.state,
            HepaLaneState::Validating
                | HepaLaneState::Reviewing
                | HepaLaneState::NeedsHumanStaging
                | HepaLaneState::PrReady
                | HepaLaneState::ReadyForHuman
                | HepaLaneState::Completed
        )
    })
}

fn transition_status(
    request: &HepaBoardTransitionRequest,
    mut task: HepaFleetTask,
    next_status: HepaTaskStatus,
    reason: &str,
) -> Result<HepaBoardTransitionDecision, HepaBoardTransitionError> {
    if task.status == next_status || task.status.can_transition_to(&next_status) {
        task.status = next_status;
        Ok(accepted(request, reason, task))
    } else {
        Ok(rejected(
            request,
            "requested status transition is not allowed by HEPA state machine",
        ))
    }
}

fn accepted(
    request: &HepaBoardTransitionRequest,
    reason: impl Into<String>,
    updated_task: HepaFleetTask,
) -> HepaBoardTransitionDecision {
    HepaBoardTransitionDecision {
        request_id: request.request_id.clone(),
        task_id: request.task_id.clone(),
        status: HepaBoardTransitionDecisionStatus::Accepted,
        reason: reason.into(),
        updated_task: Some(updated_task),
    }
}

fn rejected(
    request: &HepaBoardTransitionRequest,
    reason: impl Into<String>,
) -> HepaBoardTransitionDecision {
    HepaBoardTransitionDecision {
        request_id: request.request_id.clone(),
        task_id: request.task_id.clone(),
        status: HepaBoardTransitionDecisionStatus::Rejected,
        reason: reason.into(),
        updated_task: None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaBoardTransitionError {
    pub field: String,
    pub message: String,
}

impl HepaBoardTransitionError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaBoardTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaBoardTransitionError {}

fn require_single_line(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaBoardTransitionError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaBoardTransitionError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaBoardTransitionError::new(
            field,
            "must be a single line",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hepa_core::contracts::CONTRACT_SCHEMA_VERSION;

    fn request(action: HepaBoardTransitionAction) -> HepaBoardTransitionRequest {
        HepaBoardTransitionRequest {
            request_id: "request-1".to_string(),
            task_id: "task-1".to_string(),
            card_id: Some("hermes-card-1".to_string()),
            requested_by: "operator".to_string(),
            action,
            requested_at: "2026-06-16T00:00:00Z".to_string(),
        }
    }

    fn task(status: HepaTaskStatus, readiness: HepaReadinessState) -> HepaFleetTask {
        HepaFleetTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            title: "Update docs".to_string(),
            description: "Documentation task".to_string(),
            status,
            readiness,
            dependencies: Vec::new(),
            lane_ids: Vec::new(),
            external_card_id: Some("hermes-card-1".to_string()),
            priority: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: None,
        }
    }

    fn lane(state: HepaLaneState) -> HepaLane {
        HepaLane {
            schema_version: CONTRACT_SCHEMA_VERSION,
            lane_id: "lane-1".to_string(),
            project_id: "project-1".to_string(),
            task_id: "task-1".to_string(),
            adapter_id: "fake".to_string(),
            state,
            worktree_ref: "<WORKTREE>".to_string(),
            branch: "hepa/task-1".to_string(),
            run_dir_ref: "<RUN_DIR>".to_string(),
            attempt_count: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: None,
        }
    }

    #[test]
    fn board_transition_requests_cover_supported_actions() {
        for request in [
            request(HepaBoardTransitionAction::SetPriority { priority: 5 }),
            request(HepaBoardTransitionAction::MarkReady),
            request(HepaBoardTransitionAction::Block {
                reason: "Needs clarification".to_string(),
            }),
            request(HepaBoardTransitionAction::Cancel {
                reason: "Superseded".to_string(),
            }),
            request(HepaBoardTransitionAction::Resume),
            request(HepaBoardTransitionAction::SetDependencies {
                dependencies: vec!["task-0".to_string()],
            }),
        ] {
            request
                .validate()
                .expect("supported requests should validate");
        }
    }

    #[test]
    fn board_transition_requests_use_stable_action_names() {
        let json = serde_json::to_string(&request(HepaBoardTransitionAction::MarkReady))
            .expect("request should serialize");

        assert!(json.contains("\"type\":\"mark_ready\""));
    }

    #[test]
    fn board_transition_requests_reject_duplicate_dependencies() {
        let error = request(HepaBoardTransitionAction::SetDependencies {
            dependencies: vec!["task-0".to_string(), "task-0".to_string()],
        })
        .validate()
        .expect_err("duplicate dependencies must fail");

        assert_eq!(error.field, "action.dependencies");
    }

    #[test]
    fn board_transition_evaluator_accepts_allowed_state_changes() {
        let decision = evaluate_board_transition(
            &task(HepaTaskStatus::Queued, HepaReadinessState::NotReady),
            &request(HepaBoardTransitionAction::MarkReady),
        )
        .expect("transition should evaluate");

        assert_eq!(decision.status, HepaBoardTransitionDecisionStatus::Accepted);
        let updated = decision
            .updated_task
            .expect("accepted transition updates task");
        assert_eq!(updated.status, HepaTaskStatus::Ready);
        assert_eq!(updated.readiness, HepaReadinessState::Ready);
    }

    #[test]
    fn board_transition_evaluator_rejects_disallowed_state_changes() {
        let decision = evaluate_board_transition(
            &task(HepaTaskStatus::Completed, HepaReadinessState::Ready),
            &request(HepaBoardTransitionAction::Resume),
        )
        .expect("transition should evaluate");

        assert_eq!(decision.status, HepaBoardTransitionDecisionStatus::Rejected);
        assert!(decision.reason.contains("state machine"));
        assert!(decision.updated_task.is_none());
        let card_response = decision.to_card_response();
        assert!(card_response.visible_reason.contains("rejected"));
        assert!(card_response.visible_reason.contains("state machine"));
    }

    #[test]
    fn board_transition_evaluator_accepts_priority_and_dependency_updates() {
        let priority = evaluate_board_transition(
            &task(HepaTaskStatus::Draft, HepaReadinessState::NotReady),
            &request(HepaBoardTransitionAction::SetPriority { priority: 9 }),
        )
        .expect("priority should evaluate");
        let dependencies = evaluate_board_transition(
            &task(HepaTaskStatus::Draft, HepaReadinessState::NotReady),
            &request(HepaBoardTransitionAction::SetDependencies {
                dependencies: vec!["task-0".to_string()],
            }),
        )
        .expect("dependencies should evaluate");

        assert_eq!(priority.updated_task.expect("priority update").priority, 9);
        assert_eq!(
            dependencies
                .updated_task
                .expect("dependency update")
                .dependencies,
            vec!["task-0".to_string()]
        );
    }

    #[test]
    fn board_transitions_cannot_bypass_lane_gates() {
        let decision = evaluate_board_transition_with_lanes(
            &task(HepaTaskStatus::Blocked, HepaReadinessState::Ready),
            &[lane(HepaLaneState::Reviewing)],
            &request(HepaBoardTransitionAction::Resume),
        )
        .expect("transition should evaluate");

        assert_eq!(decision.status, HepaBoardTransitionDecisionStatus::Rejected);
        assert!(decision.reason.contains("cannot bypass"));
        assert!(
            decision
                .to_card_response()
                .visible_reason
                .contains("review")
        );
    }

    #[test]
    fn board_transition_requests_can_be_logged_as_artifacts() {
        let request = request(HepaBoardTransitionAction::Cancel {
            reason: "Superseded".to_string(),
        });
        let decision = evaluate_board_transition(
            &task(HepaTaskStatus::Draft, HepaReadinessState::NotReady),
            &request,
        )
        .expect("transition should evaluate");
        let artifact = board_transition_artifact(request.clone(), decision.clone());
        let json = serde_json::to_string(&artifact).expect("artifact should serialize");
        let round_trip: HepaBoardTransitionArtifact =
            serde_json::from_str(&json).expect("artifact should deserialize");

        assert_eq!(
            round_trip.schema_version,
            BOARD_TRANSITION_ARTIFACT_SCHEMA_VERSION
        );
        assert_eq!(round_trip.request, request);
        assert_eq!(round_trip.decision, decision);
        assert!(json.contains("\"request_id\":\"request-1\""));
    }
}
