use hepa_core::contracts::{
    CONTRACT_SCHEMA_VERSION, HepaFleetTask, HepaProject, HepaReadinessState, HepaRiskLevel,
    HepaTaskSpec, HepaTaskStatus,
};
use hepa_core::fleet_registry::{
    HepaCostClass, HepaCostPolicy, HepaFleetRegistry, HepaMemoryPolicy, HepaRegisteredProject,
};
use hepa_core::resource_governor::{HepaResourceLimits, HepaScheduleCandidate};
use hepa_core::scheduler::{HepaClaimOutcome, HepaScheduler, HepaWaitReason};
use hepa_kanban::card_mapping::{HepaHermesCardMappingInput, HepaHermesFieldValue};
use hepa_kanban::sync::{HepaKanbanSyncEngine, HepaMemoryHermesCardStore};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn ready_task(task_id: &str, project_id: &str) -> HepaFleetTask {
    HepaFleetTask {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: task_id.to_string(),
        project_id: project_id.to_string(),
        title: "Task".to_string(),
        description: "desc".to_string(),
        status: HepaTaskStatus::Ready,
        readiness: HepaReadinessState::Ready,
        dependencies: Vec::new(),
        lane_ids: Vec::new(),
        external_card_id: None,
        priority: 1,
        created_at: "2026-06-16T00:00:00Z".to_string(),
        updated_at: "2026-06-16T00:00:00Z".to_string(),
        completed_at: None,
    }
}

fn candidate(task_id: &str) -> HepaScheduleCandidate {
    HepaScheduleCandidate {
        task_id: task_id.to_string(),
        adapter_id: "fake".to_string(),
        cost_class: HepaCostClass::Local,
        file_areas: vec![format!("area-{task_id}")],
        conflict_group: None,
        touches_lockfile: false,
    }
}

#[test]
fn two_projects_run_concurrent_lanes_under_capacity_caps() {
    let root_a = temp_root("proj-a");
    let root_b = temp_root("proj-b");
    let registry_a = HepaFleetRegistry::new(&root_a);
    let registry_b = HepaFleetRegistry::new(&root_b);
    registry_a
        .create_task(&ready_task("task-a1", "project-a"))
        .expect("a1");
    registry_a
        .create_task(&ready_task("task-a2", "project-a"))
        .expect("a2");
    registry_b
        .create_task(&ready_task("task-b1", "project-b"))
        .expect("b1");

    let mut scheduler = HepaScheduler::new();
    scheduler.start();
    let limits = HepaResourceLimits::new(2, 4);

    // Project A claims a lane.
    let lane_a = match scheduler
        .claim_one(
            &registry_a,
            &limits,
            &[],
            &candidate("task-a1"),
            "lane-a1",
            "t1",
        )
        .expect("claim a1")
    {
        HepaClaimOutcome::Claimed { lane } => lane,
        other => panic!("expected claim, got {other:?}"),
    };

    // Project B claims a concurrent lane while A is active (one shared cap of 2).
    let lane_b = match scheduler
        .claim_one(
            &registry_b,
            &limits,
            std::slice::from_ref(&lane_a),
            &candidate("task-b1"),
            "lane-b1",
            "t2",
        )
        .expect("claim b1")
    {
        HepaClaimOutcome::Claimed { lane } => lane,
        other => panic!("expected claim, got {other:?}"),
    };

    // Both projects now have a running lane concurrently.
    assert_eq!(
        registry_a.show_task("task-a1").unwrap().unwrap().status,
        HepaTaskStatus::Running
    );
    assert_eq!(
        registry_b.show_task("task-b1").unwrap().unwrap().status,
        HepaTaskStatus::Running
    );

    // A third lane is blocked by the shared capacity cap of 2.
    let third = scheduler
        .claim_one(
            &registry_a,
            &limits,
            &[lane_a, lane_b],
            &candidate("task-a2"),
            "lane-a2",
            "t3",
        )
        .expect("claim a2");
    match third {
        HepaClaimOutcome::Rejected { reasons } => {
            assert!(reasons.contains(&HepaWaitReason::CapacityFull));
        }
        other => panic!("expected capacity rejection, got {other:?}"),
    }
    // The over-cap task stays ready.
    assert_eq!(
        registry_a.show_task("task-a2").unwrap().unwrap().status,
        HepaTaskStatus::Ready
    );

    std::fs::remove_dir_all(&root_a).expect("cleanup a");
    std::fs::remove_dir_all(&root_b).expect("cleanup b");
}

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
