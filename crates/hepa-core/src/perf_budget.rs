//! Structural performance budgets. These guard the one-loop model so a
//! regression that reintroduces structural overhead fails CI.

use crate::contracts::HepaTimingRecord;

/// The structural metrics a run must stay within budget on.
#[derive(Debug, Clone, PartialEq)]
pub struct HepaStructuralMetrics {
    /// Per-attempt Hermes wrapper process spawns. Must be zero — HEPA does not
    /// wrap each attempt in an orchestrator session.
    pub hermes_wrapper_spawns: u32,
    /// Worker-profile LLM calls. Must be zero on the happy path.
    pub worker_profile_llm_calls: u32,
    pub manager_passes: u32,
    pub prompt_bytes: usize,
    pub install_events: u32,
    pub lockfile_changed: bool,
    pub container_count: u32,
    pub board_sync_overhead_seconds: f64,
}

impl HepaStructuralMetrics {
    /// Derive the timing-backed metrics from a run's timing record. The fields
    /// not captured by timing (wrapper spawns, prompt size, lockfile state,
    /// board-sync overhead) are supplied by the caller.
    pub fn from_timing(
        timing: &HepaTimingRecord,
        hermes_wrapper_spawns: u32,
        prompt_bytes: usize,
        lockfile_changed: bool,
        board_sync_overhead_seconds: f64,
    ) -> Self {
        Self {
            hermes_wrapper_spawns,
            worker_profile_llm_calls: timing.counters.worker_profile_llm_calls,
            manager_passes: timing.counters.manager_passes,
            prompt_bytes,
            install_events: timing.counters.install_events,
            lockfile_changed,
            container_count: timing.counters.container_count,
            board_sync_overhead_seconds,
        }
    }
}

/// The budget thresholds.
#[derive(Debug, Clone, PartialEq)]
pub struct HepaPerfBudget {
    pub max_manager_passes: u32,
    pub max_prompt_bytes: usize,
    pub max_board_sync_overhead_seconds: f64,
}

impl Default for HepaPerfBudget {
    fn default() -> Self {
        Self {
            max_manager_passes: 2,
            max_prompt_bytes: 16 * 1024,
            max_board_sync_overhead_seconds: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaBudgetViolation {
    HermesWrapperSpawned,
    WorkerProfileCalledOnHappyPath,
    ManagerPassesExceeded,
    PromptSizeExceeded,
    InstallOnUnchangedLockfile,
    ContainerStartedInDefaultMode,
    BoardSyncOverheadExceeded,
}

/// Return every budget violation. An empty result means the run is within
/// budget.
pub fn check_perf_budget(
    metrics: &HepaStructuralMetrics,
    budget: &HepaPerfBudget,
) -> Vec<HepaBudgetViolation> {
    let mut violations = Vec::new();
    if metrics.hermes_wrapper_spawns > 0 {
        violations.push(HepaBudgetViolation::HermesWrapperSpawned);
    }
    if metrics.worker_profile_llm_calls > 0 {
        violations.push(HepaBudgetViolation::WorkerProfileCalledOnHappyPath);
    }
    if metrics.manager_passes > budget.max_manager_passes {
        violations.push(HepaBudgetViolation::ManagerPassesExceeded);
    }
    if metrics.prompt_bytes > budget.max_prompt_bytes {
        violations.push(HepaBudgetViolation::PromptSizeExceeded);
    }
    if !metrics.lockfile_changed && metrics.install_events > 0 {
        violations.push(HepaBudgetViolation::InstallOnUnchangedLockfile);
    }
    if metrics.container_count > 0 {
        violations.push(HepaBudgetViolation::ContainerStartedInDefaultMode);
    }
    if metrics.board_sync_overhead_seconds > budget.max_board_sync_overhead_seconds {
        violations.push(HepaBudgetViolation::BoardSyncOverheadExceeded);
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{CONTRACT_SCHEMA_VERSION, HepaTimingCounters};

    fn timing(counters: HepaTimingCounters) -> HepaTimingRecord {
        HepaTimingRecord {
            schema_version: CONTRACT_SCHEMA_VERSION,
            run_id: "run-1".to_string(),
            phases: Vec::new(),
            counters,
        }
    }

    fn happy_counters() -> HepaTimingCounters {
        HepaTimingCounters {
            agent_loops: 1,
            manager_passes: 1,
            worker_profile_llm_calls: 0,
            reviewer_passes: 1,
            install_events: 0,
            container_count: 0,
        }
    }

    #[test]
    fn happy_path_run_is_within_budget() {
        let metrics =
            HepaStructuralMetrics::from_timing(&timing(happy_counters()), 0, 4096, false, 0.0);
        assert!(check_perf_budget(&metrics, &HepaPerfBudget::default()).is_empty());
    }

    #[test]
    fn each_structural_regression_is_caught() {
        let budget = HepaPerfBudget::default();

        // Per-attempt Hermes wrapper spawn.
        let mut counters = happy_counters();
        let mut metrics =
            HepaStructuralMetrics::from_timing(&timing(counters.clone()), 1, 4096, false, 0.0);
        assert!(
            check_perf_budget(&metrics, &budget)
                .contains(&HepaBudgetViolation::HermesWrapperSpawned)
        );

        // Worker-profile LLM call on the happy path.
        counters.worker_profile_llm_calls = 1;
        metrics =
            HepaStructuralMetrics::from_timing(&timing(counters.clone()), 0, 4096, false, 0.0);
        assert!(
            check_perf_budget(&metrics, &budget)
                .contains(&HepaBudgetViolation::WorkerProfileCalledOnHappyPath)
        );

        // Manager passes exceeded.
        counters = happy_counters();
        counters.manager_passes = 3;
        metrics =
            HepaStructuralMetrics::from_timing(&timing(counters.clone()), 0, 4096, false, 0.0);
        assert!(
            check_perf_budget(&metrics, &budget)
                .contains(&HepaBudgetViolation::ManagerPassesExceeded)
        );

        // Container started in default mode.
        counters = happy_counters();
        counters.container_count = 1;
        metrics =
            HepaStructuralMetrics::from_timing(&timing(counters.clone()), 0, 4096, false, 0.0);
        assert!(
            check_perf_budget(&metrics, &budget)
                .contains(&HepaBudgetViolation::ContainerStartedInDefaultMode)
        );

        // Install on an unchanged lockfile.
        counters = happy_counters();
        counters.install_events = 1;
        metrics = HepaStructuralMetrics::from_timing(&timing(counters), 0, 4096, false, 0.0);
        assert!(
            check_perf_budget(&metrics, &budget)
                .contains(&HepaBudgetViolation::InstallOnUnchangedLockfile)
        );

        // Prompt size and board-sync overhead.
        let big =
            HepaStructuralMetrics::from_timing(&timing(happy_counters()), 0, 32 * 1024, false, 5.0);
        let violations = check_perf_budget(&big, &budget);
        assert!(violations.contains(&HepaBudgetViolation::PromptSizeExceeded));
        assert!(violations.contains(&HepaBudgetViolation::BoardSyncOverheadExceeded));
    }
}
