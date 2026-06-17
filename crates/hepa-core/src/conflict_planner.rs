use crate::resource_governor::{HepaLaneReservation, HepaScheduleCandidate};
use crate::scheduler::HepaWaitReason;
use std::collections::BTreeSet;

/// Deterministic planner that serializes overlapping work: shared file areas,
/// the same conflict group, and concurrent lockfile changes all block a
/// candidate with a recorded reason.
#[derive(Debug, Default, Clone, Copy)]
pub struct HepaConflictPlanner;

impl HepaConflictPlanner {
    pub fn evaluate(
        reservations: &[HepaLaneReservation],
        candidate: &HepaScheduleCandidate,
    ) -> Vec<HepaWaitReason> {
        let mut reasons = Vec::new();

        // File-area reservations: a candidate touching an area an active lane
        // holds must wait. Reasons are sorted for deterministic output.
        let reserved_areas: BTreeSet<&str> = reservations
            .iter()
            .flat_map(|reservation| reservation.file_areas.iter().map(String::as_str))
            .collect();
        let mut overlaps: Vec<String> = candidate
            .file_areas
            .iter()
            .filter(|area| reserved_areas.contains(area.as_str()))
            .cloned()
            .collect();
        overlaps.sort();
        overlaps.dedup();
        for area in overlaps {
            reasons.push(HepaWaitReason::FileAreaReserved { area });
        }

        // Conflict groups: only one active lane per group.
        if let Some(group) = &candidate.conflict_group {
            let busy = reservations
                .iter()
                .any(|reservation| reservation.conflict_group.as_deref() == Some(group.as_str()));
            if busy {
                reasons.push(HepaWaitReason::ConflictGroupBusy {
                    group: group.clone(),
                });
            }
        }

        // Serialize-on-lockfile: only one lockfile-touching lane at a time.
        if candidate.touches_lockfile
            && reservations
                .iter()
                .any(|reservation| reservation.touches_lockfile)
        {
            reasons.push(HepaWaitReason::LockfileSerialized);
        }

        reasons
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet_registry::HepaCostClass;

    fn reservation(
        lane_id: &str,
        file_areas: &[&str],
        conflict_group: Option<&str>,
        touches_lockfile: bool,
    ) -> HepaLaneReservation {
        HepaLaneReservation {
            lane_id: lane_id.to_string(),
            task_id: format!("task-{lane_id}"),
            adapter_id: "local".to_string(),
            cost_class: HepaCostClass::Local,
            file_areas: file_areas.iter().map(|area| area.to_string()).collect(),
            conflict_group: conflict_group.map(str::to_string),
            touches_lockfile,
        }
    }

    fn candidate(
        file_areas: &[&str],
        conflict_group: Option<&str>,
        touches_lockfile: bool,
    ) -> HepaScheduleCandidate {
        HepaScheduleCandidate {
            task_id: "task-new".to_string(),
            adapter_id: "local".to_string(),
            cost_class: HepaCostClass::Local,
            file_areas: file_areas.iter().map(|area| area.to_string()).collect(),
            conflict_group: conflict_group.map(str::to_string),
            touches_lockfile,
        }
    }

    #[test]
    fn overlapping_file_areas_serialize_with_reason() {
        let active = vec![reservation("lane-1", &["src/api"], None, false)];
        let reasons =
            HepaConflictPlanner::evaluate(&active, &candidate(&["src/api", "src/ui"], None, false));

        assert_eq!(
            reasons,
            vec![HepaWaitReason::FileAreaReserved {
                area: "src/api".to_string()
            }]
        );
    }

    #[test]
    fn same_conflict_group_serializes() {
        let active = vec![reservation("lane-1", &[], Some("payments"), false)];
        let reasons =
            HepaConflictPlanner::evaluate(&active, &candidate(&[], Some("payments"), false));

        assert_eq!(
            reasons,
            vec![HepaWaitReason::ConflictGroupBusy {
                group: "payments".to_string()
            }]
        );
    }

    #[test]
    fn concurrent_lockfile_changes_serialize() {
        let active = vec![reservation("lane-1", &[], None, true)];
        let reasons = HepaConflictPlanner::evaluate(&active, &candidate(&[], None, true));

        assert_eq!(reasons, vec![HepaWaitReason::LockfileSerialized]);
    }

    #[test]
    fn disjoint_work_does_not_conflict() {
        let active = vec![reservation("lane-1", &["src/api"], Some("payments"), true)];
        let reasons =
            HepaConflictPlanner::evaluate(&active, &candidate(&["docs"], Some("billing"), false));

        assert!(reasons.is_empty());
    }
}
