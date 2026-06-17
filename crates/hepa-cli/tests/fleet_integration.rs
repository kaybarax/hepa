use hepa_core::contracts::{
    CONTRACT_SCHEMA_VERSION, HepaFleetTask, HepaProject, HepaReadinessState, HepaRiskLevel,
    HepaTaskSpec, HepaTaskStatus,
};
use hepa_core::fleet_registry::{
    HepaCostClass, HepaCostPolicy, HepaFleetRegistry, HepaMemoryPolicy, HepaRegisteredProject,
};
use hepa_kanban::card_mapping::{HepaHermesCardMappingInput, HepaHermesFieldValue};
use hepa_kanban::sync::{HepaKanbanSyncEngine, HepaMemoryHermesCardStore};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("hepa-fleet-it-{label}-{nonce}"))
}

#[test]
fn registered_project_and_task_records_sync_to_hermes() {
    let root = temp_root("sync");
    let registry = HepaFleetRegistry::new(&root);

    let registration = HepaRegisteredProject {
        project: HepaProject {
            schema_version: CONTRACT_SCHEMA_VERSION,
            project_id: "project-1".to_string(),
            display_name: "Demo".to_string(),
            repo_ref: "<TARGET_REPO>".to_string(),
            default_branch: "main".to_string(),
            routing_policy_ref: None,
            is_active: true,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
        },
        max_parallel_tasks: 2,
        cost_policy: HepaCostPolicy {
            cost_class: HepaCostClass::Local,
            max_paid_lanes: 0,
        },
        memory_policy: HepaMemoryPolicy {
            max_resident_models: 1,
        },
        board_metadata: Some("board-1".to_string()),
    };
    registry
        .register_project(&registration)
        .expect("register project");

    let task = HepaFleetTask {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: "task-1".to_string(),
        project_id: "project-1".to_string(),
        title: "Fix login".to_string(),
        description: "Fix the login redirect".to_string(),
        status: HepaTaskStatus::Queued,
        readiness: HepaReadinessState::NotReady,
        dependencies: Vec::new(),
        lane_ids: Vec::new(),
        external_card_id: None,
        priority: 1,
        created_at: "2026-06-16T00:00:00Z".to_string(),
        updated_at: "2026-06-16T00:00:00Z".to_string(),
        completed_at: None,
    };
    registry.create_task(&task).expect("create task");

    // Sync the persisted registry records (not in-memory fixtures) to Hermes.
    let stored_project = registry
        .show_project("project-1")
        .expect("show project")
        .expect("project present");
    let stored_task = registry
        .show_task("task-1")
        .expect("show task")
        .expect("task present");

    let mapping_input = HepaHermesCardMappingInput {
        project: stored_project.project,
        task_spec: HepaTaskSpec {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: stored_task.task_id.clone(),
            project_id: stored_task.project_id.clone(),
            goal: stored_task.title.clone(),
            non_goals: Vec::new(),
            expected_areas: Vec::new(),
            acceptance_criteria: vec!["login works".to_string()],
            validation_commands: vec!["cargo test".to_string()],
            dependencies: Vec::new(),
            target_branch: Some("main".to_string()),
            risk_level: HepaRiskLevel::Low,
            max_total_rounds: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
        },
        task: stored_task,
        lanes: Vec::new(),
        readiness: None,
        validation: None,
        review_signals: Vec::new(),
        terminal_report: None,
        timing: None,
        steering_records: Vec::new(),
        blocked_questions: Vec::new(),
    };

    let mut store = HepaMemoryHermesCardStore::default();
    let summary = HepaKanbanSyncEngine::new()
        .sync_tasks(&[mapping_input], &mut store)
        .expect("sync should succeed");

    assert_eq!(summary.created, 1);
    assert_eq!(store.card_count(), 1);
    let card = store.card("hermes-card-1").expect("card created");
    assert_eq!(
        card.fields.get("task_id"),
        Some(&HepaHermesFieldValue::Text("task-1".to_string()))
    );

    std::fs::remove_dir_all(&root).expect("cleanup");
}
