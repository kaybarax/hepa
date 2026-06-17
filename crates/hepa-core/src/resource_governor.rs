use crate::fleet_registry::HepaCostClass;
use crate::scheduler::HepaWaitReason;
use std::collections::BTreeMap;

/// An active lane reservation the governor and conflict planner reason about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaLaneReservation {
    pub lane_id: String,
    pub task_id: String,
    pub adapter_id: String,
    pub cost_class: HepaCostClass,
    pub file_areas: Vec<String>,
    pub conflict_group: Option<String>,
    pub touches_lockfile: bool,
}

/// A candidate task being considered for a new lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaScheduleCandidate {
    pub task_id: String,
    pub adapter_id: String,
    pub cost_class: HepaCostClass,
    pub file_areas: Vec<String>,
    pub conflict_group: Option<String>,
    pub touches_lockfile: bool,
}

/// Resource limits: overall capacity, paid-cloud lane budget, and per-adapter
/// concurrency caps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaResourceLimits {
    pub max_parallel_lanes: u32,
    pub max_paid_lanes: u32,
    pub per_adapter_caps: BTreeMap<String, u32>,
}

impl HepaResourceLimits {
    pub fn new(max_parallel_lanes: u32, max_paid_lanes: u32) -> Self {
        Self {
            max_parallel_lanes,
            max_paid_lanes,
            per_adapter_caps: BTreeMap::new(),
        }
    }

    pub fn with_adapter_cap(mut self, adapter_id: impl Into<String>, cap: u32) -> Self {
        self.per_adapter_caps.insert(adapter_id.into(), cap);
        self
    }
}

/// Deterministic governor enforcing capacity, paid-lane budget, and per-adapter
/// caps. Returns every wait reason that applies (empty means admissible).
#[derive(Debug, Default, Clone, Copy)]
pub struct HepaResourceGovernor;

impl HepaResourceGovernor {
    pub fn evaluate(
        limits: &HepaResourceLimits,
        reservations: &[HepaLaneReservation],
        candidate: &HepaScheduleCandidate,
    ) -> Vec<HepaWaitReason> {
        let mut reasons = Vec::new();

        if reservations.len() as u32 >= limits.max_parallel_lanes {
            reasons.push(HepaWaitReason::CapacityFull);
        }

        if candidate.cost_class == HepaCostClass::Paid {
            let paid = reservations
                .iter()
                .filter(|reservation| reservation.cost_class == HepaCostClass::Paid)
                .count() as u32;
            if paid >= limits.max_paid_lanes {
                reasons.push(HepaWaitReason::PaidLaneCapReached);
            }
        }

        if let Some(cap) = limits.per_adapter_caps.get(&candidate.adapter_id) {
            let used = reservations
                .iter()
                .filter(|reservation| reservation.adapter_id == candidate.adapter_id)
                .count() as u32;
            if used >= *cap {
                reasons.push(HepaWaitReason::AdapterCapReached {
                    adapter_id: candidate.adapter_id.clone(),
                });
            }
        }

        reasons
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reservation(
        lane_id: &str,
        adapter_id: &str,
        cost_class: HepaCostClass,
    ) -> HepaLaneReservation {
        HepaLaneReservation {
            lane_id: lane_id.to_string(),
            task_id: format!("task-{lane_id}"),
            adapter_id: adapter_id.to_string(),
            cost_class,
            file_areas: Vec::new(),
            conflict_group: None,
            touches_lockfile: false,
        }
    }

    fn candidate(adapter_id: &str, cost_class: HepaCostClass) -> HepaScheduleCandidate {
        HepaScheduleCandidate {
            task_id: "task-new".to_string(),
            adapter_id: adapter_id.to_string(),
            cost_class,
            file_areas: Vec::new(),
            conflict_group: None,
            touches_lockfile: false,
        }
    }

    #[test]
    fn paid_lane_cap_blocks_nth_cloud_lane_while_local_proceeds() {
        let limits = HepaResourceLimits::new(8, 1);
        let active = vec![reservation("lane-1", "cloud", HepaCostClass::Paid)];

        // A second paid lane is blocked by the paid cap.
        let paid = HepaResourceGovernor::evaluate(
            &limits,
            &active,
            &candidate("cloud", HepaCostClass::Paid),
        );
        assert!(paid.contains(&HepaWaitReason::PaidLaneCapReached));

        // A local lane still proceeds under the same active set.
        let local = HepaResourceGovernor::evaluate(
            &limits,
            &active,
            &candidate("local", HepaCostClass::Local),
        );
        assert!(local.is_empty());
    }

    #[test]
    fn per_adapter_cap_blocks_when_reached() {
        let limits = HepaResourceLimits::new(8, 4).with_adapter_cap("claude", 1);
        let active = vec![reservation("lane-1", "claude", HepaCostClass::Local)];

        let reasons = HepaResourceGovernor::evaluate(
            &limits,
            &active,
            &candidate("claude", HepaCostClass::Local),
        );
        assert_eq!(
            reasons,
            vec![HepaWaitReason::AdapterCapReached {
                adapter_id: "claude".to_string()
            }]
        );
    }

    #[test]
    fn capacity_full_blocks_any_candidate() {
        let limits = HepaResourceLimits::new(1, 4);
        let active = vec![reservation("lane-1", "local", HepaCostClass::Local)];

        let reasons = HepaResourceGovernor::evaluate(
            &limits,
            &active,
            &candidate("local", HepaCostClass::Local),
        );
        assert!(reasons.contains(&HepaWaitReason::CapacityFull));
    }
}
