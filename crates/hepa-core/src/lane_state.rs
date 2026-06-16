use crate::contracts::{HepaLane, HepaLaneState};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaLaneTransitionRequest {
    pub next_state: HepaLaneState,
    pub reason: String,
    pub occurred_at: String,
    pub explicit_resume: bool,
}

impl HepaLaneTransitionRequest {
    pub fn new(
        next_state: HepaLaneState,
        reason: impl Into<String>,
        occurred_at: impl Into<String>,
    ) -> Self {
        Self {
            next_state,
            reason: reason.into(),
            occurred_at: occurred_at.into(),
            explicit_resume: false,
        }
    }

    pub fn with_explicit_resume(mut self) -> Self {
        self.explicit_resume = true;
        self
    }
}

pub fn transition_lane(
    lane: &HepaLane,
    request: &HepaLaneTransitionRequest,
) -> Result<HepaLane, HepaLaneStateError> {
    validate_request(request)?;
    if lane.state == request.next_state {
        return Ok(lane.clone());
    }
    if lane.state.is_terminal() && !request.explicit_resume {
        return Err(HepaLaneStateError::new(
            "state",
            "terminal lane transitions require an explicit resume path",
        ));
    }
    if !lane.state.can_transition_to(&request.next_state) {
        return Err(HepaLaneStateError::new(
            "state",
            format!(
                "illegal lane transition from {:?} to {:?}",
                lane.state, request.next_state
            ),
        ));
    }

    let mut updated = lane.clone();
    updated.state = request.next_state.clone();
    updated.updated_at = request.occurred_at.clone();
    if updated.state.is_terminal() {
        updated.completed_at = Some(request.occurred_at.clone());
    } else {
        updated.completed_at = None;
    }
    Ok(updated)
}

pub trait HepaLaneStateExt {
    fn is_terminal(&self) -> bool;
    fn is_relaunch_state(&self) -> bool;
}

impl HepaLaneStateExt for HepaLaneState {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            HepaLaneState::Completed
                | HepaLaneState::Failed
                | HepaLaneState::Cancelled
                | HepaLaneState::Cleaned
        )
    }

    fn is_relaunch_state(&self) -> bool {
        matches!(
            self,
            HepaLaneState::Ready
                | HepaLaneState::Allocated
                | HepaLaneState::Starting
                | HepaLaneState::Running
                | HepaLaneState::Repairing
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaLaneStateError {
    pub field: String,
    pub message: String,
}

impl HepaLaneStateError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaLaneStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaLaneStateError {}

fn validate_request(request: &HepaLaneTransitionRequest) -> Result<(), HepaLaneStateError> {
    require_single_line("reason", &request.reason)?;
    require_single_line("occurred_at", &request.occurred_at)?;
    Ok(())
}

fn require_single_line(field: &str, value: &str) -> Result<(), HepaLaneStateError> {
    if value.trim().is_empty() {
        return Err(HepaLaneStateError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaLaneStateError::new(field, "must be a single line"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::CONTRACT_SCHEMA_VERSION;

    #[test]
    fn lane_transition_updates_state_and_terminal_timestamp() {
        let lane = lane(HepaLaneState::Ready);
        let running = transition_lane(
            &lane,
            &HepaLaneTransitionRequest::new(
                HepaLaneState::Allocated,
                "scheduler claimed lane",
                "2026-06-16T00:00:01Z",
            ),
        )
        .expect("ready to allocated should be legal");
        let completed = transition_lane(
            &running,
            &HepaLaneTransitionRequest::new(
                HepaLaneState::Starting,
                "worktree ready",
                "2026-06-16T00:00:02Z",
            ),
        )
        .and_then(|lane| {
            transition_lane(
                &lane,
                &HepaLaneTransitionRequest::new(
                    HepaLaneState::Running,
                    "adapter started",
                    "2026-06-16T00:00:03Z",
                ),
            )
        })
        .and_then(|lane| {
            transition_lane(
                &lane,
                &HepaLaneTransitionRequest::new(
                    HepaLaneState::Validating,
                    "adapter finished",
                    "2026-06-16T00:00:04Z",
                ),
            )
        })
        .and_then(|lane| {
            transition_lane(
                &lane,
                &HepaLaneTransitionRequest::new(
                    HepaLaneState::Reviewing,
                    "validation passed",
                    "2026-06-16T00:00:05Z",
                ),
            )
        })
        .and_then(|lane| {
            transition_lane(
                &lane,
                &HepaLaneTransitionRequest::new(
                    HepaLaneState::Staging,
                    "review passed",
                    "2026-06-16T00:00:06Z",
                ),
            )
        })
        .and_then(|lane| {
            transition_lane(
                &lane,
                &HepaLaneTransitionRequest::new(
                    HepaLaneState::PrCreated,
                    "staged safely",
                    "2026-06-16T00:00:07Z",
                ),
            )
        })
        .and_then(|lane| {
            transition_lane(
                &lane,
                &HepaLaneTransitionRequest::new(
                    HepaLaneState::Completed,
                    "done gate passed",
                    "2026-06-16T00:00:08Z",
                ),
            )
        })
        .expect("happy path should be legal");

        assert_eq!(completed.state, HepaLaneState::Completed);
        assert_eq!(
            completed.completed_at.as_deref(),
            Some("2026-06-16T00:00:08Z")
        );
    }

    #[test]
    fn illegal_transitions_fail_with_clear_errors() {
        let error = transition_lane(
            &lane(HepaLaneState::Ready),
            &HepaLaneTransitionRequest::new(
                HepaLaneState::Reviewing,
                "skip execution",
                "2026-06-16T00:00:00Z",
            ),
        )
        .expect_err("ready to reviewing must fail");

        assert_eq!(error.field, "state");
        assert!(error.message.contains("illegal lane transition"));
    }

    #[test]
    fn terminal_lanes_require_explicit_resume_for_relaunch() {
        let failed = lane(HepaLaneState::Failed);
        let failed = HepaLane {
            completed_at: Some("2026-06-16T00:00:00Z".to_string()),
            ..failed
        };
        let blocked = transition_lane(
            &failed,
            &HepaLaneTransitionRequest::new(
                HepaLaneState::Repairing,
                "retry after failure",
                "2026-06-16T00:00:00Z",
            ),
        )
        .expect_err("terminal relaunch without resume must fail");
        let resumed = transition_lane(
            &failed,
            &HepaLaneTransitionRequest::new(
                HepaLaneState::Repairing,
                "approved retry",
                "2026-06-16T00:00:01Z",
            )
            .with_explicit_resume(),
        )
        .expect("explicit resume may enter repair");

        assert!(blocked.message.contains("explicit resume"));
        assert_eq!(resumed.state, HepaLaneState::Repairing);
        assert_eq!(resumed.completed_at, None);
    }

    fn lane(state: HepaLaneState) -> HepaLane {
        HepaLane {
            schema_version: CONTRACT_SCHEMA_VERSION,
            lane_id: "lane-1".to_string(),
            project_id: "project-1".to_string(),
            task_id: "task-1".to_string(),
            adapter_id: "fake".to_string(),
            state,
            worktree_ref: "worktree:lane-1".to_string(),
            branch: "hepa/manager/lane-1".to_string(),
            run_dir_ref: "control:runs/run-1".to_string(),
            attempt_count: 0,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: None,
        }
    }
}
