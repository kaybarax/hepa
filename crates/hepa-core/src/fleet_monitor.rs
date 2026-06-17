use crate::contracts::HepaLaneState;
use std::{fs, io, path::Path};

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

fn is_terminal_lane(lane_state: &HepaLaneState) -> bool {
    matches!(
        lane_state,
        HepaLaneState::Completed
            | HepaLaneState::Cleaned
            | HepaLaneState::Failed
            | HepaLaneState::Cancelled
    )
}

/// A repair the reconciler wants applied to bring board/state back in sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaReconcileAction {
    MarkLaneStale { lane_id: String },
    RecreateCard { lane_id: String, task_id: String },
    PruneWorktree { lane_id: String },
    FinalizeTerminalLane { lane_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaReconciliationReport {
    pub actions: Vec<HepaReconcileAction>,
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

    /// Reconcile board/state drift into concrete repair actions: stale leases,
    /// missing cards, orphaned worktrees, and terminal lanes. Actions are
    /// deterministically ordered.
    pub fn reconcile(observations: &[HepaLaneObservation]) -> HepaReconciliationReport {
        let mut actions = Vec::new();
        for observation in observations {
            let terminal = is_terminal_lane(&observation.lane_state);
            if !terminal && !observation.process_alive {
                actions.push(HepaReconcileAction::MarkLaneStale {
                    lane_id: observation.lane_id.clone(),
                });
            }
            if !terminal && observation.card_status.is_none() {
                actions.push(HepaReconcileAction::RecreateCard {
                    lane_id: observation.lane_id.clone(),
                    task_id: observation.task_id.clone(),
                });
            }
            if terminal && observation.worktree_present {
                actions.push(HepaReconcileAction::PruneWorktree {
                    lane_id: observation.lane_id.clone(),
                });
            }
            if terminal
                && observation.card_status.as_deref().is_some_and(|status| {
                    !expected_card_status(&observation.lane_state)
                        .contains(&status.trim().to_ascii_lowercase().as_str())
                })
            {
                actions.push(HepaReconcileAction::FinalizeTerminalLane {
                    lane_id: observation.lane_id.clone(),
                });
            }
        }
        actions.sort_by_key(action_key);
        HepaReconciliationReport { actions }
    }
}

/// Report of a runtime cleanup pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaCleanupReport {
    pub removed_runtime_dirs: Vec<String>,
    pub preserved_unrelated: Vec<String>,
}

impl HepaFleetMonitor {
    /// Remove only the named HEPA-created lane runtime directories under the
    /// runtime root. Any directory not in `hepa_lane_ids` (an unrelated user
    /// directory) is preserved, and lane ids that would escape the runtime root
    /// are refused.
    pub fn cleanup_runtime(
        runtime_root: &Path,
        hepa_lane_ids: &[String],
    ) -> io::Result<HepaCleanupReport> {
        let mut removed = Vec::new();
        let owned: std::collections::BTreeSet<&str> =
            hepa_lane_ids.iter().map(String::as_str).collect();

        for lane_id in &owned {
            if !is_safe_segment(lane_id) {
                continue;
            }
            let path = runtime_root.join(lane_id);
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
                removed.push((*lane_id).to_string());
            }
        }

        let mut preserved = Vec::new();
        if runtime_root.is_dir() {
            for entry in fs::read_dir(runtime_root)? {
                let entry = entry?;
                if let Some(name) = entry.file_name().to_str() {
                    if !owned.contains(name) {
                        preserved.push(name.to_string());
                    }
                }
            }
        }

        removed.sort();
        preserved.sort();
        Ok(HepaCleanupReport {
            removed_runtime_dirs: removed,
            preserved_unrelated: preserved,
        })
    }
}

fn is_safe_segment(value: &str) -> bool {
    !value.trim().is_empty()
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains("..")
}

fn action_key(action: &HepaReconcileAction) -> (u8, String) {
    match action {
        HepaReconcileAction::MarkLaneStale { lane_id } => (0, lane_id.clone()),
        HepaReconcileAction::RecreateCard { lane_id, .. } => (1, lane_id.clone()),
        HepaReconcileAction::PruneWorktree { lane_id } => (2, lane_id.clone()),
        HepaReconcileAction::FinalizeTerminalLane { lane_id } => (3, lane_id.clone()),
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
    fn reconcile_repairs_stale_missing_orphan_and_terminal_drift() {
        // Stale lease: active lane with a dead process.
        let mut stale = observation("lane-stale", HepaLaneState::Running);
        stale.process_alive = false;
        stale.card_status = Some("in_progress".to_string());

        // Missing card: active lane with no board card.
        let missing = observation("lane-missing", HepaLaneState::Running);

        // Orphaned worktree: terminal lane still holding a worktree.
        let mut orphan = observation("lane-orphan", HepaLaneState::Completed);
        orphan.worktree_present = true;
        orphan.card_status = Some("done".to_string());

        // Terminal lane whose card still says in_progress.
        let mut terminal_drift = observation("lane-term", HepaLaneState::Completed);
        terminal_drift.worktree_present = false;
        terminal_drift.card_status = Some("in_progress".to_string());

        let report = HepaFleetMonitor::reconcile(&[stale, missing, orphan, terminal_drift]);

        assert!(
            report
                .actions
                .contains(&HepaReconcileAction::MarkLaneStale {
                    lane_id: "lane-stale".to_string()
                })
        );
        assert!(report.actions.contains(&HepaReconcileAction::RecreateCard {
            lane_id: "lane-missing".to_string(),
            task_id: "task-lane-missing".to_string()
        }));
        assert!(
            report
                .actions
                .contains(&HepaReconcileAction::PruneWorktree {
                    lane_id: "lane-orphan".to_string()
                })
        );
        assert!(
            report
                .actions
                .contains(&HepaReconcileAction::FinalizeTerminalLane {
                    lane_id: "lane-term".to_string()
                })
        );
    }

    #[test]
    fn cleanup_removes_only_hepa_runtime_dirs() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let runtime_root = std::env::temp_dir().join(format!("hepa-cleanup-{nonce}"));
        std::fs::create_dir_all(runtime_root.join("lane-1")).expect("lane-1");
        std::fs::create_dir_all(runtime_root.join("lane-2")).expect("lane-2");
        // An unrelated user directory that must be preserved.
        std::fs::create_dir_all(runtime_root.join("user-notes")).expect("user-notes");
        std::fs::write(runtime_root.join("user-notes/keep.txt"), b"keep\n").expect("user file");

        let report = HepaFleetMonitor::cleanup_runtime(
            &runtime_root,
            &["lane-1".to_string(), "lane-2".to_string()],
        )
        .expect("cleanup");

        assert_eq!(
            report.removed_runtime_dirs,
            vec!["lane-1".to_string(), "lane-2".to_string()]
        );
        assert_eq!(report.preserved_unrelated, vec!["user-notes".to_string()]);
        assert!(!runtime_root.join("lane-1").exists());
        assert!(runtime_root.join("user-notes/keep.txt").exists());

        std::fs::remove_dir_all(&runtime_root).expect("cleanup test dir");
    }

    #[test]
    fn reconcile_leaves_healthy_lanes_untouched() {
        let mut healthy = observation("lane-1", HepaLaneState::Running);
        healthy.card_status = Some("in_progress".to_string());
        let report = HepaFleetMonitor::reconcile(&[healthy]);
        assert!(report.actions.is_empty());
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
