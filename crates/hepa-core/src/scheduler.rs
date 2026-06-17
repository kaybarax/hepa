use crate::contracts::HepaTaskStatus;
use crate::fleet_registry::{HepaFleetError, HepaFleetRegistry};

/// Whether the scheduler loop is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HepaSchedulerRunState {
    Stopped,
    Running,
}

/// Scheduling limits applied on each tick. Extended with cost/adapter/conflict
/// rules in later checkboxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaSchedulerLimits {
    pub max_parallel_lanes: u32,
}

/// A minimal view of an active lane the scheduler must account for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaActiveLaneSummary {
    pub lane_id: String,
    pub task_id: String,
}

/// Why a ready task cannot be claimed this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaWaitReason {
    DependencyPending { dependency: String },
    CapacityFull,
    PaidLaneCapReached,
    AdapterCapReached { adapter_id: String },
    FileAreaReserved { area: String },
    ConflictGroupBusy { group: String },
    LockfileSerialized,
}

impl HepaWaitReason {
    /// Human-readable, dashboard-observable wait reason.
    pub fn describe(&self) -> String {
        match self {
            HepaWaitReason::DependencyPending { dependency } => {
                format!("waiting on dependency {dependency}")
            }
            HepaWaitReason::CapacityFull => "waiting for lane capacity".to_string(),
            HepaWaitReason::PaidLaneCapReached => "waiting for a paid-cloud lane slot".to_string(),
            HepaWaitReason::AdapterCapReached { adapter_id } => {
                format!("waiting for an open slot on adapter {adapter_id}")
            }
            HepaWaitReason::FileAreaReserved { area } => {
                format!("waiting: file area {area} is reserved by an active lane")
            }
            HepaWaitReason::ConflictGroupBusy { group } => {
                format!("waiting: conflict group {group} is busy")
            }
            HepaWaitReason::LockfileSerialized => {
                "waiting: lockfile changes are serialized with an active lane".to_string()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaTaskWait {
    pub task_id: String,
    pub reasons: Vec<HepaWaitReason>,
}

/// Result of one scheduler tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaTickOutcome {
    NotRunning,
    Idle,
    Claimable { task_id: String },
    Waiting { waits: Vec<HepaTaskWait> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaSchedulerStatus {
    pub run_state: HepaSchedulerRunState,
    pub ready: u32,
    pub running: u32,
    pub blocked: u32,
    pub queued: u32,
    pub active_lanes: u32,
    pub waits: Vec<HepaTaskWait>,
}

/// Deterministic fleet scheduler. Selection is stateless apart from the
/// running/stopped flag; all task state lives in the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaScheduler {
    run_state: HepaSchedulerRunState,
}

impl Default for HepaScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl HepaScheduler {
    pub fn new() -> Self {
        Self {
            run_state: HepaSchedulerRunState::Stopped,
        }
    }

    pub fn start(&mut self) {
        self.run_state = HepaSchedulerRunState::Running;
    }

    pub fn stop(&mut self) {
        self.run_state = HepaSchedulerRunState::Stopped;
    }

    pub fn run_state(&self) -> HepaSchedulerRunState {
        self.run_state
    }

    /// Attempt to select one ready task to claim, respecting dependencies and
    /// overall lane capacity. Selection is by priority (desc) then task id.
    pub fn tick(
        &self,
        registry: &HepaFleetRegistry,
        limits: &HepaSchedulerLimits,
        active_lanes: &[HepaActiveLaneSummary],
    ) -> Result<HepaTickOutcome, HepaFleetError> {
        if self.run_state != HepaSchedulerRunState::Running {
            return Ok(HepaTickOutcome::NotRunning);
        }

        let ready = self.ready_tasks_by_priority(registry)?;
        if ready.is_empty() {
            return Ok(HepaTickOutcome::Idle);
        }

        let capacity_full = active_lanes.len() as u32 >= limits.max_parallel_lanes;
        let mut waits = Vec::new();
        for task_id in ready {
            let unmet = registry.unmet_dependencies(&task_id)?;
            if let Some(dependency) = unmet.into_iter().next() {
                waits.push(HepaTaskWait {
                    task_id,
                    reasons: vec![HepaWaitReason::DependencyPending { dependency }],
                });
                continue;
            }
            if capacity_full {
                waits.push(HepaTaskWait {
                    task_id,
                    reasons: vec![HepaWaitReason::CapacityFull],
                });
                continue;
            }
            return Ok(HepaTickOutcome::Claimable { task_id });
        }

        Ok(HepaTickOutcome::Waiting { waits })
    }

    /// Observable scheduler status with per-task wait reasons.
    pub fn status(
        &self,
        registry: &HepaFleetRegistry,
        limits: &HepaSchedulerLimits,
        active_lanes: &[HepaActiveLaneSummary],
    ) -> Result<HepaSchedulerStatus, HepaFleetError> {
        let tasks = registry.list_tasks()?;
        let mut ready = 0;
        let mut running = 0;
        let mut blocked = 0;
        let mut queued = 0;
        for task in &tasks {
            match task.status {
                HepaTaskStatus::Ready => ready += 1,
                HepaTaskStatus::Running => running += 1,
                HepaTaskStatus::Blocked => blocked += 1,
                HepaTaskStatus::Queued => queued += 1,
                _ => {}
            }
        }
        let waits = match self.tick(registry, limits, active_lanes)? {
            HepaTickOutcome::Waiting { waits } => waits,
            _ => Vec::new(),
        };
        Ok(HepaSchedulerStatus {
            run_state: self.run_state,
            ready,
            running,
            blocked,
            queued,
            active_lanes: active_lanes.len() as u32,
            waits,
        })
    }

    fn ready_tasks_by_priority(
        &self,
        registry: &HepaFleetRegistry,
    ) -> Result<Vec<String>, HepaFleetError> {
        let mut ready: Vec<(u32, String)> = registry
            .list_tasks()?
            .into_iter()
            .filter(|task| task.status == HepaTaskStatus::Ready)
            .map(|task| (task.priority, task.task_id))
            .collect();
        // Higher priority first; ties break on task id for determinism.
        ready.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        Ok(ready.into_iter().map(|(_, task_id)| task_id).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{CONTRACT_SCHEMA_VERSION, HepaFleetTask, HepaReadinessState};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn ready_task(task_id: &str, priority: u32) -> HepaFleetTask {
        HepaFleetTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: task_id.to_string(),
            project_id: "project-1".to_string(),
            title: "Task".to_string(),
            description: "desc".to_string(),
            status: HepaTaskStatus::Ready,
            readiness: HepaReadinessState::Ready,
            dependencies: Vec::new(),
            lane_ids: Vec::new(),
            external_card_id: None,
            priority,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: None,
        }
    }

    fn limits(max: u32) -> HepaSchedulerLimits {
        HepaSchedulerLimits {
            max_parallel_lanes: max,
        }
    }

    #[test]
    fn stopped_scheduler_does_not_claim() {
        let root = unique_test_dir("stopped");
        let registry = HepaFleetRegistry::new(&root);
        registry
            .create_task(&ready_task("task-1", 1))
            .expect("create");
        let scheduler = HepaScheduler::new();

        let outcome = scheduler.tick(&registry, &limits(2), &[]).expect("tick");
        assert_eq!(outcome, HepaTickOutcome::NotRunning);

        remove_test_dir(root);
    }

    #[test]
    fn running_scheduler_selects_highest_priority_ready_task() {
        let root = unique_test_dir("select");
        let registry = HepaFleetRegistry::new(&root);
        registry
            .create_task(&ready_task("task-low", 1))
            .expect("low");
        registry
            .create_task(&ready_task("task-high", 9))
            .expect("high");
        let mut scheduler = HepaScheduler::new();
        scheduler.start();

        let outcome = scheduler.tick(&registry, &limits(2), &[]).expect("tick");
        assert_eq!(
            outcome,
            HepaTickOutcome::Claimable {
                task_id: "task-high".to_string()
            }
        );

        remove_test_dir(root);
    }

    #[test]
    fn capacity_full_records_wait_reasons() {
        let root = unique_test_dir("capacity");
        let registry = HepaFleetRegistry::new(&root);
        registry
            .create_task(&ready_task("task-1", 1))
            .expect("create");
        let mut scheduler = HepaScheduler::new();
        scheduler.start();
        let active = vec![HepaActiveLaneSummary {
            lane_id: "lane-1".to_string(),
            task_id: "task-0".to_string(),
        }];

        let outcome = scheduler
            .tick(&registry, &limits(1), &active)
            .expect("tick");
        match outcome {
            HepaTickOutcome::Waiting { waits } => {
                assert_eq!(waits.len(), 1);
                assert_eq!(waits[0].reasons, vec![HepaWaitReason::CapacityFull]);
            }
            other => panic!("expected waiting, got {other:?}"),
        }

        let status = scheduler
            .status(&registry, &limits(1), &active)
            .expect("status");
        assert_eq!(status.run_state, HepaSchedulerRunState::Running);
        assert_eq!(status.ready, 1);
        assert_eq!(status.active_lanes, 1);
        assert!(!status.waits.is_empty());

        remove_test_dir(root);
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-scheduler-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            std::fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
