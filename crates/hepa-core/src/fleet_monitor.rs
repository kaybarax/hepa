use crate::contracts::HepaLaneState;

/// A sampled observation of one active lane, gathered from process state, git,
/// validation/review artifacts, and the Hermes board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaLaneObservation {
    pub lane_id: String,
    pub task_id: String,
    pub lane_state: HepaLaneState,
    pub process_alive: bool,
    pub branch_present: bool,
    pub pr_status: Option<String>,
    pub validation_state: Option<String>,
    pub review_state: Option<String>,
    pub card_status: Option<String>,
    pub worktree_present: bool,
}

/// A point-in-time resource sample for the fleet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaResourceSample {
    pub active_lanes: u32,
    pub memory_mb: u64,
}

/// Per-lane health derived during a refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaLaneHealth {
    pub lane_id: String,
    pub process_alive: bool,
    pub branch_present: bool,
    pub pr_status: Option<String>,
    pub validation_state: Option<String>,
    pub review_state: Option<String>,
    pub card_drift: bool,
}

/// A refreshed snapshot of the fleet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaFleetSnapshot {
    pub lanes: Vec<HepaLaneHealth>,
    pub resource_sample: HepaResourceSample,
    pub drifted_lanes: Vec<String>,
}

/// Whether a Hermes card status agrees with a lane state. Drift is any lane
/// whose board status contradicts the authoritative lane state.
fn card_drift(lane_state: &HepaLaneState, card_status: Option<&str>) -> bool {
    let Some(card_status) = card_status else {
        // No card where one is expected for an active lane is handled by
        // reconcile (missing card); it is not counted as a status contradiction.
        return false;
    };
    let expected = expected_card_status(lane_state);
    !expected.contains(&card_status.trim().to_ascii_lowercase().as_str())
}

fn expected_card_status(lane_state: &HepaLaneState) -> Vec<&'static str> {
    match lane_state {
        HepaLaneState::DraftSpec | HepaLaneState::Ready => vec!["todo", "ready"],
        HepaLaneState::Allocated
        | HepaLaneState::Starting
        | HepaLaneState::Running
        | HepaLaneState::Validating
        | HepaLaneState::Reviewing
        | HepaLaneState::Repairing
        | HepaLaneState::Staging => vec!["in_progress", "running"],
        HepaLaneState::PrCreated | HepaLaneState::ReadyForHuman => {
            vec!["in_review", "ready_for_human"]
        }
        HepaLaneState::Completed | HepaLaneState::Cleaned => vec!["done", "completed"],
        HepaLaneState::Blocked | HepaLaneState::Failed => vec!["blocked", "failed"],
        HepaLaneState::Cancelled => vec!["cancelled"],
    }
}

/// Deterministic fleet monitor.
#[derive(Debug, Default, Clone, Copy)]
pub struct HepaFleetMonitor;

impl HepaFleetMonitor {
    /// Refresh fleet health from lane observations and a resource sample:
    /// process liveness, branch/PR status, validation/review state, resource
    /// usage, and card drift.
    pub fn refresh(
        observations: &[HepaLaneObservation],
        resource_sample: HepaResourceSample,
    ) -> HepaFleetSnapshot {
        let mut lanes = Vec::new();
        let mut drifted_lanes = Vec::new();
        for observation in observations {
            let drift = card_drift(&observation.lane_state, observation.card_status.as_deref());
            if drift {
                drifted_lanes.push(observation.lane_id.clone());
            }
            lanes.push(HepaLaneHealth {
                lane_id: observation.lane_id.clone(),
                process_alive: observation.process_alive,
                branch_present: observation.branch_present,
                pr_status: observation.pr_status.clone(),
                validation_state: observation.validation_state.clone(),
                review_state: observation.review_state.clone(),
                card_drift: drift,
            });
        }
        lanes.sort_by(|left, right| left.lane_id.cmp(&right.lane_id));
        drifted_lanes.sort();
        HepaFleetSnapshot {
            lanes,
            resource_sample,
            drifted_lanes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(lane_id: &str, lane_state: HepaLaneState) -> HepaLaneObservation {
        HepaLaneObservation {
            lane_id: lane_id.to_string(),
            task_id: format!("task-{lane_id}"),
            lane_state,
            process_alive: true,
            branch_present: true,
            pr_status: None,
            validation_state: Some("passed".to_string()),
            review_state: Some("approved".to_string()),
            card_status: None,
            worktree_present: true,
        }
    }

    #[test]
    fn refresh_reports_liveness_and_resource_sample() {
        let mut running = observation("lane-1", HepaLaneState::Running);
        running.process_alive = false;
        let snapshot = HepaFleetMonitor::refresh(
            &[running],
            HepaResourceSample {
                active_lanes: 1,
                memory_mb: 2048,
            },
        );

        assert_eq!(snapshot.lanes.len(), 1);
        assert!(!snapshot.lanes[0].process_alive);
        assert_eq!(
            snapshot.lanes[0].validation_state.as_deref(),
            Some("passed")
        );
        assert_eq!(snapshot.resource_sample.memory_mb, 2048);
    }

    #[test]
    fn refresh_detects_card_drift() {
        let mut drifting = observation("lane-1", HepaLaneState::Running);
        drifting.card_status = Some("done".to_string()); // board says done, lane is running
        let mut agreeing = observation("lane-2", HepaLaneState::Running);
        agreeing.card_status = Some("in_progress".to_string());

        let snapshot = HepaFleetMonitor::refresh(
            &[drifting, agreeing],
            HepaResourceSample {
                active_lanes: 2,
                memory_mb: 1024,
            },
        );

        assert_eq!(snapshot.drifted_lanes, vec!["lane-1".to_string()]);
        let lane1 = snapshot
            .lanes
            .iter()
            .find(|lane| lane.lane_id == "lane-1")
            .expect("lane-1");
        assert!(lane1.card_drift);
    }
}
