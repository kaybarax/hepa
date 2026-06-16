use hepa_adapters::fake::{HepaFakeAdapter, HepaFakeReviewerInput, HepaFakeWorkerInput};
#[cfg(test)]
use hepa_core::contracts::HepaProject;
use hepa_core::{
    artifacts::{HepaArchiveOutcome, HepaArtifactLayout, HepaStateTransitionRecord},
    contracts::{
        CONTRACT_SCHEMA_VERSION, HepaAgentRole, HepaAttemptReport, HepaFleetTask, HepaLane,
        HepaLaneState, HepaPhaseStatus, HepaReadinessResult, HepaReadinessState,
        HepaReadinessStatus, HepaReviewSignal, HepaRiskLevel, HepaTaskSpec, HepaTaskStatus,
        HepaTerminalStatus, HepaTerminalTaskReport, HepaTimingCounters, HepaTimingPhase,
        HepaTimingRecord, HepaValidate, HepaValidationStatus, HepaValidationSummary,
    },
    lane_state::{HepaLaneTransitionRequest, transition_lane},
};
use hepa_git::worktree::{HepaWorktreeAllocation, HepaWorktreeAllocator};
#[cfg(test)]
use hepa_kanban::{
    card_mapping::HepaHermesCardMappingInput,
    sync::{HepaHermesCardStore, HepaKanbanSyncEngine, HepaKanbanSyncSummary},
};
use serde::Serialize;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaFakeRunConfig {
    pub repo_path: PathBuf,
    pub control_root: PathBuf,
    pub worktree_root: PathBuf,
    pub archive_root: PathBuf,
    pub run_id: String,
    pub task_id: String,
    pub lane_id: String,
    pub task_text: String,
    pub timing: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HepaFakeRunResult {
    pub run_id: String,
    pub lane_id: String,
    pub status: String,
    pub timing: HepaTimingRecord,
    pub terminal_report: HepaTerminalTaskReport,
    pub cleanup_performed: bool,
}

pub fn run_fake_task(config: &HepaFakeRunConfig) -> Result<HepaFakeRunResult, String> {
    validate_config(config)?;
    let task_spec = task_spec(config);
    task_spec.validate().map_err(|error| error.to_string())?;
    let mut task = fleet_task(config);
    let layout = HepaArtifactLayout::new(&config.control_root, &config.archive_root)
        .map_err(|error| error.to_string())?;
    let run_paths = layout
        .run(&config.run_id, &config.task_id)
        .map_err(|error| error.to_string())?;
    let lane_paths = run_paths
        .lane(&config.lane_id)
        .map_err(|error| error.to_string())?;
    let allocator = HepaWorktreeAllocator::new(&config.repo_path, &config.worktree_root);
    let allocation = allocator
        .allocate_lane_with_metadata(&config.lane_id, "2026-06-16T00:00:00Z")
        .map_err(|error| error.to_string())?;

    fs::create_dir_all(&lane_paths.lane_dir).map_err(|error| error.to_string())?;
    write_json(&run_paths.task_state, &task_spec).map_err(|error| error.to_string())?;

    let mut lane = initial_lane(config, &allocation);
    write_json(&lane_paths.lane_state, &lane).map_err(|error| error.to_string())?;

    transition_and_record(
        &lane_paths,
        &mut lane,
        1,
        HepaLaneState::Starting,
        "worktree allocated",
        "2026-06-16T00:00:01Z",
    )?;
    transition_and_record(
        &lane_paths,
        &mut lane,
        2,
        HepaLaneState::Running,
        "fake worker started",
        "2026-06-16T00:00:02Z",
    )?;

    let fake = HepaFakeAdapter::default();
    let attempt = fake
        .run_worker_attempt(&HepaFakeWorkerInput {
            task_spec: task_spec.clone(),
            lane_id: config.lane_id.clone(),
            attempt_id: "attempt-1".to_string(),
            round: 1,
            started_at: "2026-06-16T00:00:02Z".to_string(),
            completed_at: "2026-06-16T00:00:03Z".to_string(),
        })
        .map_err(|error| error.to_string())?;
    lane.attempt_count = 1;
    write_attempt(&lane_paths, &attempt)?;

    transition_and_record(
        &lane_paths,
        &mut lane,
        3,
        HepaLaneState::Validating,
        "fake worker completed",
        "2026-06-16T00:00:03Z",
    )?;
    let validation = validation_summary();
    write_json(&lane_paths.validation_summary, &validation).map_err(|error| error.to_string())?;

    transition_and_record(
        &lane_paths,
        &mut lane,
        4,
        HepaLaneState::Reviewing,
        "validation placeholder passed",
        "2026-06-16T00:00:04Z",
    )?;
    let review = fake
        .run_reviewer(&HepaFakeReviewerInput {
            lane_id: config.lane_id.clone(),
            review_id: "review-1".to_string(),
            completed_at: "2026-06-16T00:00:05Z".to_string(),
        })
        .map_err(|error| error.to_string())?;
    write_json(
        &lane_paths
            .review_signal("review-1")
            .map_err(|error| error.to_string())?,
        &review,
    )
    .map_err(|error| error.to_string())?;

    transition_and_record(
        &lane_paths,
        &mut lane,
        5,
        HepaLaneState::Staging,
        "fake review approved",
        "2026-06-16T00:00:05Z",
    )?;
    transition_and_record(
        &lane_paths,
        &mut lane,
        6,
        HepaLaneState::PrCreated,
        "fake staging completed",
        "2026-06-16T00:00:06Z",
    )?;
    transition_and_record(
        &lane_paths,
        &mut lane,
        7,
        HepaLaneState::Completed,
        "fake done gate passed",
        "2026-06-16T00:00:07Z",
    )?;
    write_json(&lane_paths.lane_state, &lane).map_err(|error| error.to_string())?;

    let readiness = readiness_result(config);
    task.status = HepaTaskStatus::Completed;
    task.readiness = HepaReadinessState::Ready;
    task.completed_at = Some("2026-06-16T00:00:07Z".to_string());
    write_json(&run_paths.task_state, &task).map_err(|error| error.to_string())?;

    let timing = timing_record(config);
    lane_paths
        .write_timing_record(&timing)
        .map_err(|error| error.to_string())?;
    let terminal_report = terminal_report(config, validation, review, timing.clone());
    write_json(&lane_paths.final_report, &terminal_report).map_err(|error| error.to_string())?;
    write_json(&run_paths.run_state, &readiness).map_err(|error| error.to_string())?;
    run_paths
        .archive_on_exit("2026-06-16T00:00:08Z", HepaArchiveOutcome::Completed)
        .map_err(|error| error.to_string())?;

    let cleanup = allocator
        .cleanup_lane(&config.lane_id, "2026-06-16T00:00:09Z")
        .map_err(|error| error.to_string())?;

    Ok(HepaFakeRunResult {
        run_id: config.run_id.clone(),
        lane_id: config.lane_id.clone(),
        status: "completed".to_string(),
        timing,
        terminal_report,
        cleanup_performed: matches!(
            cleanup.status,
            hepa_git::worktree::HepaWorktreeCleanupStatus::Cleaned
        ),
    })
}

#[cfg(test)]
fn sync_fake_run_to_hermes_fixture(
    config: &HepaFakeRunConfig,
    result: &HepaFakeRunResult,
    store: &mut dyn HepaHermesCardStore,
) -> Result<HepaKanbanSyncSummary, String> {
    let validation = result.terminal_report.validation.clone();
    let input = HepaHermesCardMappingInput {
        project: HepaProject {
            schema_version: CONTRACT_SCHEMA_VERSION,
            project_id: "project-1".to_string(),
            display_name: "HEPA Fixture Project".to_string(),
            repo_ref: "<TARGET_REPO>".to_string(),
            default_branch: "main".to_string(),
            routing_policy_ref: None,
            is_active: true,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:07Z".to_string(),
        },
        task_spec: task_spec(config),
        task: completed_fleet_task(config),
        lanes: vec![completed_lane(config)],
        readiness: Some(readiness_result(config)),
        validation,
        review_signals: result.terminal_report.review_signals.clone(),
        terminal_report: Some(result.terminal_report.clone()),
        timing: Some(result.timing.clone()),
        blocked_questions: Vec::new(),
    };

    HepaKanbanSyncEngine::new().sync_tasks(&[input], store)
}

fn validate_config(config: &HepaFakeRunConfig) -> Result<(), String> {
    for (field, value) in [
        ("run_id", &config.run_id),
        ("task_id", &config.task_id),
        ("lane_id", &config.lane_id),
        ("task_text", &config.task_text),
    ] {
        if value.trim().is_empty() || value.contains('\n') || value.contains('\r') {
            return Err(format!("{field}: must be a non-empty single line"));
        }
    }
    Ok(())
}

fn task_spec(config: &HepaFakeRunConfig) -> HepaTaskSpec {
    HepaTaskSpec {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: config.task_id.clone(),
        project_id: "project-1".to_string(),
        goal: config.task_text.clone(),
        non_goals: Vec::new(),
        expected_areas: vec!["README.md".to_string()],
        acceptance_criteria: vec!["Fake task completed".to_string()],
        validation_commands: vec!["fake validation placeholder".to_string()],
        dependencies: Vec::new(),
        target_branch: Some("main".to_string()),
        risk_level: HepaRiskLevel::Low,
        max_total_rounds: 1,
        created_at: "2026-06-16T00:00:00Z".to_string(),
    }
}

fn fleet_task(config: &HepaFakeRunConfig) -> HepaFleetTask {
    HepaFleetTask {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: config.task_id.clone(),
        project_id: "project-1".to_string(),
        title: config.task_text.clone(),
        description: "Deterministic fake run task".to_string(),
        status: HepaTaskStatus::Running,
        readiness: HepaReadinessState::Ready,
        dependencies: Vec::new(),
        lane_ids: vec![config.lane_id.clone()],
        external_card_id: None,
        priority: 1,
        created_at: "2026-06-16T00:00:00Z".to_string(),
        updated_at: "2026-06-16T00:00:00Z".to_string(),
        completed_at: None,
    }
}

#[cfg(test)]
fn completed_fleet_task(config: &HepaFakeRunConfig) -> HepaFleetTask {
    let mut task = fleet_task(config);
    task.status = HepaTaskStatus::Completed;
    task.completed_at = Some("2026-06-16T00:00:07Z".to_string());
    task.updated_at = "2026-06-16T00:00:07Z".to_string();
    task
}

fn initial_lane(config: &HepaFakeRunConfig, allocation: &HepaWorktreeAllocation) -> HepaLane {
    HepaLane {
        schema_version: CONTRACT_SCHEMA_VERSION,
        lane_id: config.lane_id.clone(),
        project_id: "project-1".to_string(),
        task_id: config.task_id.clone(),
        adapter_id: "fake".to_string(),
        state: HepaLaneState::Allocated,
        worktree_ref: format!("worktree:{}", config.lane_id),
        branch: allocation.branch.clone(),
        run_dir_ref: format!("control:runs/{}", config.run_id),
        attempt_count: 0,
        created_at: "2026-06-16T00:00:00Z".to_string(),
        updated_at: "2026-06-16T00:00:00Z".to_string(),
        completed_at: None,
    }
}

#[cfg(test)]
fn completed_lane(config: &HepaFakeRunConfig) -> HepaLane {
    HepaLane {
        schema_version: CONTRACT_SCHEMA_VERSION,
        lane_id: config.lane_id.clone(),
        project_id: "project-1".to_string(),
        task_id: config.task_id.clone(),
        adapter_id: "fake".to_string(),
        state: HepaLaneState::Completed,
        worktree_ref: format!("worktree:{}", config.lane_id),
        branch: format!("hepa/manager/{}", config.lane_id),
        run_dir_ref: format!("control:runs/{}", config.run_id),
        attempt_count: 1,
        created_at: "2026-06-16T00:00:00Z".to_string(),
        updated_at: "2026-06-16T00:00:07Z".to_string(),
        completed_at: Some("2026-06-16T00:00:07Z".to_string()),
    }
}

fn transition_and_record(
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    lane: &mut HepaLane,
    sequence: u32,
    next_state: HepaLaneState,
    reason: &str,
    occurred_at: &str,
) -> Result<(), String> {
    let from_state = serde_json::to_value(&lane.state)
        .map_err(|error| error.to_string())?
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let updated = transition_lane(
        lane,
        &HepaLaneTransitionRequest::new(next_state, reason, occurred_at),
    )
    .map_err(|error| error.to_string())?;
    let to_state = serde_json::to_value(&updated.state)
        .map_err(|error| error.to_string())?
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let record = HepaStateTransitionRecord::lane(
        lane_paths.run_id.as_str(),
        lane_paths.task_id.as_str(),
        lane_paths.lane_id.as_str(),
        format!("{sequence:03}-{to_state}"),
        Some(from_state),
        to_state,
        occurred_at,
    )
    .with_reason(reason);
    lane_paths
        .write_transition_state(&record)
        .map_err(|error| error.to_string())?;
    *lane = updated;
    Ok(())
}

fn write_attempt(
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    attempt: &HepaAttemptReport,
) -> Result<(), String> {
    let paths = lane_paths
        .attempt(&attempt.attempt_id)
        .map_err(|error| error.to_string())?;
    write_json(&paths.attempt_report, attempt).map_err(|error| error.to_string())
}

fn validation_summary() -> HepaValidationSummary {
    HepaValidationSummary {
        schema_version: CONTRACT_SCHEMA_VERSION,
        status: HepaValidationStatus::Passed,
        commands: Vec::new(),
        no_tests_detected: false,
        failure_type: None,
        summary: vec!["Fake validation placeholder passed.".to_string()],
    }
}

fn readiness_result(config: &HepaFakeRunConfig) -> HepaReadinessResult {
    HepaReadinessResult {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: config.task_id.clone(),
        status: HepaReadinessStatus::Ready,
        blockers: Vec::new(),
        questions: Vec::new(),
        checked_at: "2026-06-16T00:00:00Z".to_string(),
    }
}

fn timing_record(config: &HepaFakeRunConfig) -> HepaTimingRecord {
    HepaTimingRecord {
        schema_version: CONTRACT_SCHEMA_VERSION,
        run_id: config.run_id.clone(),
        phases: vec![
            HepaTimingPhase {
                name: "fake_worker".to_string(),
                status: HepaPhaseStatus::Completed,
                duration_seconds: 1.0,
                round: Some(1),
                role: Some(HepaAgentRole::Worker),
                adapter_id: Some("fake".to_string()),
                routing_reason: Some("default fake adapter".to_string()),
                sandbox_posture: Some("host-worktree".to_string()),
            },
            HepaTimingPhase {
                name: "fake_review".to_string(),
                status: HepaPhaseStatus::Completed,
                duration_seconds: 1.0,
                round: Some(1),
                role: Some(HepaAgentRole::Reviewer),
                adapter_id: Some("fake".to_string()),
                routing_reason: Some("fake review fanout".to_string()),
                sandbox_posture: Some("host-worktree".to_string()),
            },
        ],
        counters: HepaTimingCounters {
            agent_loops: 1,
            manager_passes: 1,
            reviewer_passes: 1,
            install_events: 0,
            container_count: 0,
        },
    }
}

fn terminal_report(
    config: &HepaFakeRunConfig,
    validation: HepaValidationSummary,
    review: HepaReviewSignal,
    timing: HepaTimingRecord,
) -> HepaTerminalTaskReport {
    HepaTerminalTaskReport {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: config.task_id.clone(),
        lane_id: config.lane_id.clone(),
        status: HepaTerminalStatus::Completed,
        pr_url: None,
        validation: Some(validation),
        review_signals: vec![review],
        timing: Some(timing),
        summary: vec!["Fake run completed deterministically.".to_string()],
        human_attention_required: false,
        completed_at: "2026-06-16T00:00:07Z".to_string(),
    }
}

fn write_json<T>(path: &Path, value: &T) -> io::Result<()>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut json = serde_json::to_string_pretty(value)?;
    if !json.ends_with('\n') {
        json.push('\n');
    }
    fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hepa_kanban::{card_mapping::HepaHermesFieldValue, sync::HepaMemoryHermesCardStore};
    use std::{
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn fake_run_drives_temp_repo_through_pipeline_and_cleanup() {
        let root = unique_test_dir("fake-run");
        let repo = root.join("repo");
        init_repo(&repo);
        let config = HepaFakeRunConfig {
            repo_path: repo.clone(),
            control_root: root.join("control"),
            worktree_root: root.join("worktrees"),
            archive_root: root.join("archive"),
            run_id: "run-1".to_string(),
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            task_text: "Update docs".to_string(),
            timing: true,
        };

        let result = run_fake_task(&config).expect("fake run should complete");

        assert_eq!(result.status, "completed");
        assert!(result.cleanup_performed);
        assert_eq!(result.timing.counters.agent_loops, 1);
        assert_eq!(result.timing.counters.container_count, 0);
        assert!(!config.worktree_root.join("lane-1").exists());
        let run_dir = config.control_root.join("runs/run-1");
        let lane_dir = run_dir.join("tasks/task-1/lanes/lane-1");
        for artifact in [
            run_dir.join("run.json"),
            run_dir.join("tasks/task-1/task.json"),
            lane_dir.join("lane.json"),
            lane_dir.join("state/current.json"),
            lane_dir.join("state/transitions/001-starting.json"),
            lane_dir.join("state/transitions/002-running.json"),
            lane_dir.join("state/transitions/003-validating.json"),
            lane_dir.join("state/transitions/004-reviewing.json"),
            lane_dir.join("state/transitions/005-staging.json"),
            lane_dir.join("state/transitions/006-pr_created.json"),
            lane_dir.join("state/transitions/007-completed.json"),
            lane_dir.join("attempts/attempt-1/attempt.json"),
            lane_dir.join("validation/summary.json"),
            lane_dir.join("review/signals/review-1.json"),
            lane_dir.join("timing.json"),
            lane_dir.join("final-report.json"),
            config.archive_root.join("runs/run-1/manifest.json"),
            config
                .archive_root
                .join("runs/run-1/tasks/task-1/lanes/lane-1/final-report.json"),
        ] {
            assert!(artifact.exists(), "missing artifact {}", artifact.display());
        }

        remove_test_dir(root);
    }

    #[test]
    fn fake_run_syncs_to_hermes_fixture_card() {
        let root = unique_test_dir("fake-hermes-sync");
        let repo = root.join("repo");
        init_repo(&repo);
        let config = HepaFakeRunConfig {
            repo_path: repo,
            control_root: root.join("control"),
            worktree_root: root.join("worktrees"),
            archive_root: root.join("archive"),
            run_id: "run-1".to_string(),
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            task_text: "Update docs".to_string(),
            timing: true,
        };
        let result = run_fake_task(&config).expect("fake run should complete");
        let mut store = HepaMemoryHermesCardStore::default();

        let summary = sync_fake_run_to_hermes_fixture(&config, &result, &mut store)
            .expect("fixture sync should update cards");

        assert_eq!(summary.created, 1);
        assert_eq!(summary.updated, 0);
        let card = store
            .card("hermes-card-1")
            .expect("created fixture card should be stored");
        assert_eq!(
            card.fields.get("task_id"),
            Some(&HepaHermesFieldValue::Text("task-1".to_string()))
        );
        assert_eq!(
            card.fields.get("lane_states"),
            Some(&HepaHermesFieldValue::List(vec![
                "lane-1:completed".to_string()
            ]))
        );
        assert_eq!(
            card.fields.get("agent_loops"),
            Some(&HepaHermesFieldValue::Number(1))
        );

        remove_test_dir(root);
    }

    fn init_repo(repo: &Path) {
        fs::create_dir_all(repo).expect("repo dir");
        git(repo, ["init"]);
        git(repo, ["config", "user.email", "hepa-test"]);
        git(repo, ["config", "user.name", "HEPA Test"]);
        fs::write(repo.join("README.md"), "fixture\n").expect("fixture write");
        git(repo, ["add", "README.md"]);
        git(repo, ["commit", "-m", "initial"]);
    }

    fn git<const N: usize>(repo: &Path, args: [&str; N]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-cli-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
