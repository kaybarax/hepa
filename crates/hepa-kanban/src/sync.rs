use crate::card_mapping::{
    HepaHermesCardMappingInput, HepaHermesCardPayload, map_task_to_hermes_card,
};
use hepa_core::contracts::HepaLane;
use std::{collections::BTreeMap, error::Error, fmt};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HepaKanbanSyncEngine;

impl HepaKanbanSyncEngine {
    pub fn new() -> Self {
        Self
    }

    pub fn sync_tasks(
        &self,
        tasks: &[HepaHermesCardMappingInput],
        store: &mut dyn HepaHermesCardStore,
    ) -> Result<HepaKanbanSyncSummary, String> {
        let mut summary = HepaKanbanSyncSummary::default();
        if let HepaHermesStoreAvailability::Unavailable { reason } = store.availability() {
            summary.status = HepaKanbanSyncStatus::Degraded;
            summary.degraded_reason = Some(reason);
            summary.skipped = tasks.len() as u32;
            return Ok(summary);
        }
        for task in tasks {
            let payload = map_task_to_hermes_card(task).map_err(|error| error.to_string())?;
            let existing_card_id = task.task.external_card_id.as_deref();
            let outcome = store
                .upsert_card(existing_card_id, &payload)
                .map_err(|error| error.to_string())?;
            match outcome.action {
                HepaKanbanSyncAction::Created => summary.created += 1,
                HepaKanbanSyncAction::Updated => summary.updated += 1,
            }
            summary.results.push(HepaKanbanSyncTaskResult {
                task_id: task.task.task_id.clone(),
                external_card_id: outcome.external_card_id,
                action: outcome.action,
            });
        }
        Ok(summary)
    }

    pub fn sync_lane_transition(
        &self,
        task: &HepaHermesCardMappingInput,
        updated_lane: HepaLane,
        store: &mut dyn HepaHermesCardStore,
    ) -> Result<HepaKanbanSyncSummary, String> {
        if let HepaHermesStoreAvailability::Unavailable { reason } = store.availability() {
            return Ok(HepaKanbanSyncSummary {
                status: HepaKanbanSyncStatus::Degraded,
                skipped: 1,
                degraded_reason: Some(reason),
                ..HepaKanbanSyncSummary::default()
            });
        }
        let mut updated_task = task.clone();
        let mut replaced = false;
        for lane in &mut updated_task.lanes {
            if lane.lane_id == updated_lane.lane_id {
                *lane = updated_lane.clone();
                replaced = true;
            }
        }
        if !replaced {
            updated_task.lanes.push(updated_lane.clone());
        }
        if !updated_task.task.lane_ids.contains(&updated_lane.lane_id) {
            updated_task
                .task
                .lane_ids
                .push(updated_lane.lane_id.clone());
        }

        self.sync_tasks(&[updated_task], store)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HepaKanbanReconciliationReport {
    pub drift_detected: bool,
    pub reason: String,
    pub reconciled_payload: HepaHermesCardPayload,
}

pub fn reconcile_card_with_hepa_state(
    task: &HepaHermesCardMappingInput,
    board_payload: &HepaHermesCardPayload,
) -> Result<HepaKanbanReconciliationReport, String> {
    let expected_payload = map_task_to_hermes_card(task).map_err(|error| error.to_string())?;
    let drift_detected = &expected_payload != board_payload;
    Ok(HepaKanbanReconciliationReport {
        drift_detected,
        reason: if drift_detected {
            "board card payload drifted from authoritative HEPA state".to_string()
        } else {
            "board card payload matches authoritative HEPA state".to_string()
        },
        reconciled_payload: expected_payload,
    })
}

pub trait HepaHermesCardStore {
    fn availability(&self) -> HepaHermesStoreAvailability {
        HepaHermesStoreAvailability::Available
    }

    fn upsert_card(
        &mut self,
        existing_card_id: Option<&str>,
        payload: &HepaHermesCardPayload,
    ) -> Result<HepaHermesCardUpsert, HepaKanbanSyncError>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HepaKanbanSyncSummary {
    pub status: HepaKanbanSyncStatus,
    pub created: u32,
    pub updated: u32,
    pub skipped: u32,
    pub degraded_reason: Option<String>,
    pub results: Vec<HepaKanbanSyncTaskResult>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum HepaKanbanSyncStatus {
    #[default]
    Synced,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaHermesStoreAvailability {
    Available,
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaKanbanSyncTaskResult {
    pub task_id: String,
    pub external_card_id: String,
    pub action: HepaKanbanSyncAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaHermesCardUpsert {
    pub external_card_id: String,
    pub action: HepaKanbanSyncAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaKanbanSyncAction {
    Created,
    Updated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaKanbanSyncError {
    pub message: String,
}

impl HepaKanbanSyncError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaKanbanSyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl Error for HepaKanbanSyncError {}

#[derive(Debug, Default)]
pub struct HepaNullHermesCardStore;

impl HepaHermesCardStore for HepaNullHermesCardStore {
    fn upsert_card(
        &mut self,
        existing_card_id: Option<&str>,
        _payload: &HepaHermesCardPayload,
    ) -> Result<HepaHermesCardUpsert, HepaKanbanSyncError> {
        Ok(HepaHermesCardUpsert {
            external_card_id: existing_card_id.unwrap_or("none").to_string(),
            action: if existing_card_id.is_some() {
                HepaKanbanSyncAction::Updated
            } else {
                HepaKanbanSyncAction::Created
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaUnavailableHermesCardStore {
    reason: String,
}

impl HepaUnavailableHermesCardStore {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl HepaHermesCardStore for HepaUnavailableHermesCardStore {
    fn availability(&self) -> HepaHermesStoreAvailability {
        HepaHermesStoreAvailability::Unavailable {
            reason: self.reason.clone(),
        }
    }

    fn upsert_card(
        &mut self,
        _existing_card_id: Option<&str>,
        _payload: &HepaHermesCardPayload,
    ) -> Result<HepaHermesCardUpsert, HepaKanbanSyncError> {
        Err(HepaKanbanSyncError::new(self.reason.clone()))
    }
}

#[derive(Debug, Default)]
pub struct HepaMemoryHermesCardStore {
    cards: BTreeMap<String, HepaHermesCardPayload>,
    next_id: u32,
}

impl HepaMemoryHermesCardStore {
    pub fn card(&self, external_card_id: &str) -> Option<&HepaHermesCardPayload> {
        self.cards.get(external_card_id)
    }

    pub fn card_count(&self) -> usize {
        self.cards.len()
    }
}

impl HepaHermesCardStore for HepaMemoryHermesCardStore {
    fn upsert_card(
        &mut self,
        existing_card_id: Option<&str>,
        payload: &HepaHermesCardPayload,
    ) -> Result<HepaHermesCardUpsert, HepaKanbanSyncError> {
        if let Some(existing_card_id) = existing_card_id {
            self.cards
                .insert(existing_card_id.to_string(), payload.clone());
            return Ok(HepaHermesCardUpsert {
                external_card_id: existing_card_id.to_string(),
                action: HepaKanbanSyncAction::Updated,
            });
        }

        if let Some(task_id) = payload_task_id(payload) {
            if let Some((external_card_id, _)) = self.cards.iter().find(|(_, card)| {
                payload_task_id(card)
                    .as_deref()
                    .is_some_and(|existing_task_id| existing_task_id == task_id)
            }) {
                let external_card_id = external_card_id.clone();
                self.cards.insert(external_card_id.clone(), payload.clone());
                return Ok(HepaHermesCardUpsert {
                    external_card_id,
                    action: HepaKanbanSyncAction::Updated,
                });
            }
        }

        self.next_id += 1;
        let external_card_id = format!("hermes-card-{}", self.next_id);
        if self.cards.contains_key(&external_card_id) {
            return Err(HepaKanbanSyncError::new("generated duplicate card ID"));
        }
        self.cards.insert(external_card_id.clone(), payload.clone());
        Ok(HepaHermesCardUpsert {
            external_card_id,
            action: HepaKanbanSyncAction::Created,
        })
    }
}

fn payload_task_id(payload: &HepaHermesCardPayload) -> Option<String> {
    match payload.fields.get("task_id") {
        Some(crate::card_mapping::HepaHermesFieldValue::Text(task_id)) => Some(task_id.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_mapping::HepaHermesFieldValue;
    use hepa_core::contracts::{
        CONTRACT_SCHEMA_VERSION, HepaFleetTask, HepaLane, HepaLaneState, HepaProject,
        HepaReadinessState, HepaRiskLevel, HepaTaskSpec, HepaTaskStatus,
    };

    fn sample_task(external_card_id: Option<&str>) -> HepaHermesCardMappingInput {
        let project = HepaProject {
            schema_version: CONTRACT_SCHEMA_VERSION,
            project_id: "project-1".to_string(),
            display_name: "Project One".to_string(),
            repo_ref: "<PROJECT_REPO>".to_string(),
            default_branch: "main".to_string(),
            routing_policy_ref: None,
            is_active: true,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
        };
        let task_spec = HepaTaskSpec {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            goal: "Update docs".to_string(),
            non_goals: Vec::new(),
            expected_areas: vec!["README.md".to_string()],
            acceptance_criteria: vec!["Docs updated".to_string()],
            validation_commands: vec!["cargo test".to_string()],
            dependencies: Vec::new(),
            target_branch: Some("main".to_string()),
            risk_level: HepaRiskLevel::Low,
            max_total_rounds: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
        };
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
            external_card_id: external_card_id.map(str::to_string),
            priority: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: None,
        };

        HepaHermesCardMappingInput {
            project,
            task_spec,
            task,
            lanes: Vec::new(),
            readiness: None,
            validation: None,
            review_signals: Vec::new(),
            terminal_report: None,
            timing: None,
            steering_records: Vec::new(),
            blocked_questions: Vec::new(),
        }
    }

    fn sample_lane(state: HepaLaneState) -> HepaLane {
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
            attempt_count: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: None,
        }
    }

    #[test]
    fn sync_creates_cards_from_hepa_tasks() {
        let mut store = HepaMemoryHermesCardStore::default();

        let summary = HepaKanbanSyncEngine::new()
            .sync_tasks(&[sample_task(None)], &mut store)
            .expect("sync should create cards");

        assert_eq!(summary.status, HepaKanbanSyncStatus::Synced);
        assert_eq!(summary.created, 1);
        assert_eq!(summary.updated, 0);
        assert_eq!(summary.results[0].external_card_id, "hermes-card-1");
        let card = store
            .card("hermes-card-1")
            .expect("created card should be stored");
        assert_eq!(
            card.fields.get("task_id"),
            Some(&HepaHermesFieldValue::Text("task-1".to_string()))
        );
    }

    #[test]
    fn sync_updates_existing_cards_from_hepa_tasks() {
        let mut store = HepaMemoryHermesCardStore::default();

        let summary = HepaKanbanSyncEngine::new()
            .sync_tasks(&[sample_task(Some("hermes-card-7"))], &mut store)
            .expect("sync should update cards");

        assert_eq!(summary.created, 0);
        assert_eq!(summary.updated, 1);
        assert_eq!(summary.results[0].action, HepaKanbanSyncAction::Updated);
        assert!(store.card("hermes-card-7").is_some());
    }

    #[test]
    fn sync_reports_degraded_when_hermes_store_is_unavailable() {
        let mut store = HepaUnavailableHermesCardStore::new("Hermes CLI/API unavailable");

        let summary = HepaKanbanSyncEngine::new()
            .sync_tasks(&[sample_task(None)], &mut store)
            .expect("unavailable Hermes should degrade rather than fail");

        assert_eq!(summary.status, HepaKanbanSyncStatus::Degraded);
        assert_eq!(summary.created, 0);
        assert_eq!(summary.updated, 0);
        assert_eq!(summary.skipped, 1);
        assert_eq!(
            summary.degraded_reason.as_deref(),
            Some("Hermes CLI/API unavailable")
        );
    }

    #[test]
    fn sync_catches_up_after_outage_without_duplicate_cards() {
        let mut unavailable = HepaUnavailableHermesCardStore::new("Hermes CLI/API unavailable");
        let skipped = HepaKanbanSyncEngine::new()
            .sync_tasks(&[sample_task(None)], &mut unavailable)
            .expect("outage should degrade");
        assert_eq!(skipped.status, HepaKanbanSyncStatus::Degraded);
        assert_eq!(skipped.skipped, 1);

        let mut store = HepaMemoryHermesCardStore::default();
        let first_catch_up = HepaKanbanSyncEngine::new()
            .sync_tasks(&[sample_task(None)], &mut store)
            .expect("first catch-up should create");
        let second_catch_up = HepaKanbanSyncEngine::new()
            .sync_tasks(&[sample_task(None)], &mut store)
            .expect("second catch-up should update");

        assert_eq!(first_catch_up.created, 1);
        assert_eq!(second_catch_up.created, 0);
        assert_eq!(second_catch_up.updated, 1);
        assert_eq!(store.card_count(), 1);
        assert_eq!(
            first_catch_up.results[0].external_card_id,
            second_catch_up.results[0].external_card_id
        );
    }

    #[test]
    fn sync_lane_transition_refreshes_hermes_card_state_projection() {
        let mut input = sample_task(Some("hermes-card-7"));
        input.task.lane_ids = vec!["lane-1".to_string()];
        input.lanes = vec![sample_lane(HepaLaneState::Allocated)];
        let mut store = HepaMemoryHermesCardStore::default();
        HepaKanbanSyncEngine::new()
            .sync_tasks(&[input.clone()], &mut store)
            .expect("initial sync should work");

        let summary = HepaKanbanSyncEngine::new()
            .sync_lane_transition(&input, sample_lane(HepaLaneState::Running), &mut store)
            .expect("transition sync should work");
        let card = store
            .card("hermes-card-7")
            .expect("existing card should be updated");

        assert_eq!(summary.updated, 1);
        assert_eq!(
            card.fields.get("lane_states"),
            Some(&HepaHermesFieldValue::List(vec![
                "lane-1:running".to_string()
            ]))
        );
    }

    #[test]
    fn sync_lane_transition_degrades_when_hermes_is_unavailable() {
        let mut input = sample_task(Some("hermes-card-7"));
        input.task.lane_ids = vec!["lane-1".to_string()];
        input.lanes = vec![sample_lane(HepaLaneState::Allocated)];
        let mut store = HepaUnavailableHermesCardStore::new("Hermes CLI/API unavailable");

        let summary = HepaKanbanSyncEngine::new()
            .sync_lane_transition(&input, sample_lane(HepaLaneState::Running), &mut store)
            .expect("unavailable Hermes should degrade");

        assert_eq!(summary.status, HepaKanbanSyncStatus::Degraded);
        assert_eq!(summary.skipped, 1);
        assert_eq!(
            summary.degraded_reason.as_deref(),
            Some("Hermes CLI/API unavailable")
        );
    }

    #[test]
    fn reconciliation_restores_card_payload_from_hepa_state_after_drift() {
        let task = sample_task(None);
        let expected = map_task_to_hermes_card(&task).expect("mapping should work");
        let mut drifted = expected.clone();
        drifted.fields.insert(
            "task_status".to_string(),
            HepaHermesFieldValue::Text("completed".to_string()),
        );

        let report = reconcile_card_with_hepa_state(&task, &drifted)
            .expect("reconciliation should compare payloads");

        assert!(report.drift_detected);
        assert!(report.reason.contains("authoritative HEPA state"));
        assert_eq!(report.reconciled_payload, expected);
        assert_eq!(
            report.reconciled_payload.fields.get("task_status"),
            Some(&HepaHermesFieldValue::Text("queued".to_string()))
        );
    }
}
