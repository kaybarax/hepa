use hepa_adapters::{
    engine::{HepaOneshotAdapterExecutor, HepaOneshotAdapterInvocation},
    fake::{HepaFakeAdapter, HepaFakeReviewerInput, HepaFakeWorkerInput},
    pi::{HepaPiParsedOutput, parse_pi_json_events, pi_local_route_diagnostic},
    registry::HepaAdapterRegistry,
    routing::{HepaReviewFanout, HepaReviewPassPolicy},
    spec::{HepaAdapterOutputCapture, HepaAdapterRole, HepaAdapterTemplateContext},
    usage::extract_adapter_usage,
};
#[cfg(test)]
use hepa_core::contracts::HepaProject;
use hepa_core::{
    artifacts::{HepaArchiveOutcome, HepaArtifactLayout, HepaStateTransitionRecord},
    contracts::{
        CONTRACT_SCHEMA_VERSION, HepaAgentRole, HepaArbitrationSummary, HepaAttemptReport,
        HepaFindingSeverity, HepaFleetTask, HepaHermesPrIntent, HepaHermesReviewArtifact,
        HepaHermesReviewManagerArtifact, HepaHermesRunBrief, HepaLane, HepaLaneState,
        HepaPhaseStatus, HepaReadinessResult, HepaReadinessState, HepaReadinessStatus,
        HepaReviewFinding, HepaReviewSignal, HepaReviewStatus, HepaRiskLevel, HepaTaskSpec,
        HepaTaskStatus, HepaTerminalStatus, HepaTerminalTaskReport, HepaTimingCounters,
        HepaTimingPhase, HepaTimingRecord, HepaValidate, HepaValidationCommandResult,
        HepaValidationStatus, HepaValidationSummary,
    },
    cost_accounting::{HepaAdapterUsageEntry, HepaLaneCostReport},
    hard_blockers::block_from_monitor_stop,
    lane_state::{HepaLaneTransitionRequest, transition_lane},
    monitor::HepaMonitorPolicy,
};
use hepa_core::{config::HepaConfigOverrides, redaction::redact_secrets};
use hepa_git::{
    pr::{
        HepaCommitMessage, HepaManagerGitLifecycle, HepaPrBodyInput, HepaPrRequest,
        HepaSystemProcessRunner, build_pr_body, pr_request_from_hermes_intent_with_run_evidence,
    },
    staging::HepaSafeStaging,
    worktree::{HepaWorktreeAllocation, HepaWorktreeAllocator},
};
#[cfg(test)]
use hepa_kanban::{
    card_mapping::HepaHermesCardMappingInput,
    sync::{HepaHermesCardStore, HepaKanbanSyncEngine, HepaKanbanSyncSummary},
};
use hepa_memory::{HepaProjectMemory, HepaRewardSignal};
use hepa_review::{
    arbitration::{
        HepaArbitratedFinding, HepaManagerArbitrationAction, apply_deterministic_downgrade_rules,
        apply_manager_arbitration, evaluate_staging_after_arbitration,
        summarize_arbitration_results,
    },
    fanout::{
        HepaConfiguredReviewer, HepaReviewFanoutInput, aggregate_review_findings,
        apply_review_pass_policy, run_configured_reviewers_concurrently,
    },
    parser::{HepaReviewerOutputInput, normalize_reviewer_output_by_exception},
    repair::{
        HepaRepairBriefInput, HepaRepairRoundPolicy, HepaRepairRoundState,
        enforce_repair_round_budget, rewrite_repair_prompt_from_evidence,
    },
};
use serde::Serialize;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, MutexGuard, OnceLock, TryLockError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
    prepare_live_worktree_dependency_reuse(&config.repo_path, &allocation.worktree_path)?;

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
    record_terminal_memory(TerminalMemoryInput {
        control_root: &config.control_root,
        project_id: "single-repo",
        lane_id: &config.lane_id,
        lane_state: &HepaLaneState::Completed,
        adapter_id: "fake",
        prompt_pattern: &config.task_text,
        failure_pattern: None,
        validation_pass: true,
        reviewer_pass: true,
        pr_readiness: true,
        repair_convergence: true,
    });
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

pub fn run_live_task(
    config: &HepaFakeRunConfig,
    adapter_id: &str,
) -> Result<HepaFakeRunResult, String> {
    validate_config(config)?;
    let hermes_run_brief = live_run_brief(config)?;
    let task_spec = if let Some(brief) = &hermes_run_brief {
        live_task_spec_from_hermes_brief(config, brief)
    } else {
        live_task_spec(config)
    };
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
    if let Some(brief) = &hermes_run_brief {
        write_json(&lane_paths.lane_dir.join("hermes-run-brief.json"), brief)
            .map_err(|error| error.to_string())?;
    }
    write_json(&run_paths.task_state, &task_spec).map_err(|error| error.to_string())?;

    let mut lane = initial_lane(config, &allocation);
    lane.adapter_id = adapter_id.to_string();
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
        "live adapter started",
        "2026-06-16T00:00:02Z",
    )?;

    let live_config = hepa_core::config::HepaConfig::load(
        None,
        &std::collections::BTreeMap::new(),
        HepaConfigOverrides {
            control_root: Some(config.control_root.to_string_lossy().to_string()),
            worktree_root: Some(config.worktree_root.to_string_lossy().to_string()),
            archive_root: Some(config.archive_root.to_string_lossy().to_string()),
            pi_model: std::env::var("HEPA_PI_MODEL").ok(),
            pi_review_model: optional_env("HEPA_PI_REVIEW_MODEL"),
            pi_provider_key_env: optional_env("HEPA_PI_PROVIDER_KEY_ENV"),
            pi_base_url: optional_env("HEPA_PI_BASE_URL"),
            ..HepaConfigOverrides::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let registry =
        HepaAdapterRegistry::load_from_config(&live_config).map_err(|error| error.to_string())?;
    let spec = registry
        .get(adapter_id)
        .ok_or_else(|| format!("adapter not registered: {adapter_id}"))?
        .clone();
    let mut environment = std::collections::BTreeMap::new();
    for key in [
        "HEPA_PI_MODEL",
        "HEPA_PI_REVIEW_MODEL",
        "HEPA_PI_BASE_URL",
        "PI_CODING_AGENT_DIR",
        "PI_CODING_AGENT_SESSION_DIR",
        "PI_PACKAGE_DIR",
    ] {
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.to_string(), value);
        }
    }
    if let Some(provider_key_env) = live_config.pi.provider_key_env.as_ref() {
        if let Ok(value) = std::env::var(provider_key_env) {
            environment.insert(provider_key_env.clone(), value);
        }
    }
    let fallback_task_prompt = sanitized_task_text(config);
    let task_prompt = hermes_run_brief
        .as_ref()
        .map(|brief| brief.task_prompt.as_str())
        .unwrap_or(fallback_task_prompt.as_str());
    let prompt = live_worker_prompt_for_adapter(task_prompt, adapter_id, &live_config);
    let mut repair_timing = None;
    let mut attempt_outcome = match execute_live_worker_attempt(ExecuteLiveAttemptInput {
        config,
        lane_paths: &lane_paths,
        allocation: &allocation,
        spec: spec.clone(),
        adapter_id,
        environment: environment.clone(),
        attempt_id: "attempt-1",
        round: 1,
        prompt: prompt.clone(),
        started_at: "2026-06-16T00:00:02Z",
        completed_at: "2026-06-16T00:00:03Z",
    }) {
        Ok(outcome) => outcome,
        Err(error) => {
            lane.attempt_count = 1;
            transition_and_record(
                &lane_paths,
                &mut lane,
                3,
                HepaLaneState::Blocked,
                "live worker attempt blocked",
                "2026-06-16T00:00:03Z",
            )?;
            return finish_blocked_live_run(FinishBlockedInput {
                config,
                task,
                run_paths: &run_paths,
                lane_paths: &lane_paths,
                allocator: &allocator,
                lane: &mut lane,
                validation: blocked_worker_validation_summary(&error),
                review_signals: Vec::new(),
                arbitration: None,
                timing: live_timing_record(LiveTimingInput {
                    config,
                    adapter_id,
                    worker_duration_seconds: 0.0,
                    validation_duration_seconds: 0.0,
                    review_duration_seconds: 0.0,
                    reviewer_passes: 0,
                    terminal_phase: LivePipelinePhase::WorkerBlocked,
                    repair_timing: None,
                }),
                reason: format!(
                    "Live worker attempt failed before validation/review/staging: {error}"
                ),
            });
        }
    };
    lane.attempt_count = 1;
    let mut usage_entries = attempt_outcome.usage_entries.clone();
    write_lane_cost_report_if_present(config, &lane_paths, &usage_entries)?;
    if live_force_secret_path_change() {
        inject_secret_path_fixture(&allocation.worktree_path)?;
        attempt_outcome.changed_files = collect_changed_files(&allocation.worktree_path)?;
    }

    transition_and_record(
        &lane_paths,
        &mut lane,
        3,
        HepaLaneState::Validating,
        "live adapter completed",
        "2026-06-16T00:00:03Z",
    )?;
    let validation_started = Instant::now();
    let mut validation = run_live_validation(&allocation.worktree_path, &task_spec);
    if live_force_first_validation_failure() && validation.status == HepaValidationStatus::Passed {
        validation = force_validation_failure_for_repair_stress(validation);
    }
    let validation_duration_seconds = validation_started.elapsed().as_secs_f64();
    write_json(&lane_paths.validation_summary, &validation).map_err(|error| error.to_string())?;
    append_validation_stream_event(&lane_paths, 1, &validation)?;
    if live_force_git_lifecycle_violation() {
        let blocked = controlled_git_lifecycle_block()?;
        transition_and_record(
            &lane_paths,
            &mut lane,
            4,
            HepaLaneState::Blocked,
            "adapter git lifecycle command blocked",
            "2026-06-16T00:00:04Z",
        )?;
        return finish_blocked_live_run(FinishBlockedInput {
            config,
            task,
            run_paths: &run_paths,
            lane_paths: &lane_paths,
            allocator: &allocator,
            lane: &mut lane,
            validation,
            review_signals: Vec::new(),
            arbitration: None,
            timing: live_timing_record(LiveTimingInput {
                config,
                adapter_id,
                worker_duration_seconds: attempt_outcome.duration_seconds,
                validation_duration_seconds,
                review_duration_seconds: 0.0,
                reviewer_passes: 0,
                terminal_phase: LivePipelinePhase::SafetyBlocked,
                repair_timing: None,
            }),
            reason: format!(
                "Adapter Git lifecycle hard block before review/staging: reason={} evidence={}",
                blocked.reason, blocked.evidence
            ),
        });
    }
    if let Some(secret_path) = first_secret_like_changed_path(&attempt_outcome.changed_files) {
        transition_and_record(
            &lane_paths,
            &mut lane,
            4,
            HepaLaneState::Blocked,
            "secret-like changed path blocked",
            "2026-06-16T00:00:04Z",
        )?;
        return finish_blocked_live_run(FinishBlockedInput {
            config,
            task,
            run_paths: &run_paths,
            lane_paths: &lane_paths,
            allocator: &allocator,
            lane: &mut lane,
            validation,
            review_signals: Vec::new(),
            arbitration: None,
            timing: live_timing_record(LiveTimingInput {
                config,
                adapter_id,
                worker_duration_seconds: attempt_outcome.duration_seconds,
                validation_duration_seconds,
                review_duration_seconds: 0.0,
                reviewer_passes: 0,
                terminal_phase: LivePipelinePhase::SafetyBlocked,
                repair_timing: None,
            }),
            reason: format!(
                "Secret-like path hard block before review/staging: {}",
                redact_secret_like_path_for_report(&secret_path)
            ),
        });
    }
    if validation.status != HepaValidationStatus::Passed {
        transition_and_record(
            &lane_paths,
            &mut lane,
            4,
            HepaLaneState::Repairing,
            "live validation failed; repair brief started",
            "2026-06-16T00:00:04Z",
        )?;
        let repair_started = Instant::now();
        let repair_result = match run_live_repair_round(RunLiveRepairInput {
            config,
            lane_paths: &lane_paths,
            allocation: &allocation,
            task_spec: &task_spec,
            spec: spec.clone(),
            adapter_id,
            environment: environment.clone(),
            prior_prompt: prompt.clone(),
            failed_validation: validation.clone(),
            review_findings: Vec::new(),
            repair_round: 2,
            first_changed_files: attempt_outcome.changed_files.clone(),
        }) {
            Ok(result) => result,
            Err(error) => {
                transition_and_record(
                    &lane_paths,
                    &mut lane,
                    5,
                    HepaLaneState::Blocked,
                    "live repair preparation failed",
                    "2026-06-16T00:00:05Z",
                )?;
                return finish_blocked_live_run(FinishBlockedInput {
                    config,
                    task,
                    run_paths: &run_paths,
                    lane_paths: &lane_paths,
                    allocator: &allocator,
                    lane: &mut lane,
                    validation,
                    review_signals: Vec::new(),
                    arbitration: None,
                    timing: live_timing_record(LiveTimingInput {
                        config,
                        adapter_id,
                        worker_duration_seconds: attempt_outcome.duration_seconds,
                        validation_duration_seconds,
                        review_duration_seconds: 0.0,
                        reviewer_passes: 0,
                        terminal_phase: LivePipelinePhase::ValidationFailed,
                        repair_timing: None,
                    }),
                    reason: format!(
                        "Live repair preparation failed after validation evidence: {error}"
                    ),
                });
            }
        };
        let repair_duration_seconds = repair_started.elapsed().as_secs_f64();
        validation = repair_result.validation;
        attempt_outcome.duration_seconds += repair_result.worker_duration_seconds;
        attempt_outcome.changed_files = repair_result.changed_files;
        usage_entries.extend(repair_result.usage_entries.clone());
        write_lane_cost_report_if_present(config, &lane_paths, &usage_entries)?;
        lane.attempt_count = 2;
        write_json(&lane_paths.validation_summary, &validation)
            .map_err(|error| error.to_string())?;
        if validation.status != HepaValidationStatus::Passed {
            transition_and_record(
                &lane_paths,
                &mut lane,
                5,
                HepaLaneState::Blocked,
                "live repair validation failed",
                "2026-06-16T00:00:05Z",
            )?;
            repair_timing = Some(LiveRepairTiming {
                brief_duration_seconds: repair_duration_seconds,
                worker_duration_seconds: repair_result.worker_duration_seconds,
                validation_duration_seconds: repair_result.validation_duration_seconds,
                completed: false,
            });
            return finish_blocked_live_run(FinishBlockedInput {
                config,
                task,
                run_paths: &run_paths,
                lane_paths: &lane_paths,
                allocator: &allocator,
                lane: &mut lane,
                validation,
                review_signals: Vec::new(),
                arbitration: None,
                timing: live_timing_record(LiveTimingInput {
                    config,
                    adapter_id,
                    worker_duration_seconds: attempt_outcome.duration_seconds,
                    validation_duration_seconds,
                    review_duration_seconds: 0.0,
                    reviewer_passes: 0,
                    terminal_phase: LivePipelinePhase::ValidationFailed,
                    repair_timing,
                }),
                reason: "Live validation failed after bounded repair round; review, staging, and PR were not attempted."
                    .to_string(),
            });
        }
        repair_timing = Some(LiveRepairTiming {
            brief_duration_seconds: repair_duration_seconds,
            worker_duration_seconds: repair_result.worker_duration_seconds,
            validation_duration_seconds: repair_result.validation_duration_seconds,
            completed: true,
        });
        transition_and_record(
            &lane_paths,
            &mut lane,
            5,
            HepaLaneState::Validating,
            "live repair validation passed",
            "2026-06-16T00:00:05Z",
        )?;
    }

    transition_and_record(
        &lane_paths,
        &mut lane,
        6,
        HepaLaneState::Reviewing,
        "live validation passed",
        "2026-06-16T00:00:06Z",
    )?;
    let review_started = Instant::now();
    let diff_context = collect_live_diff(&allocation.worktree_path)?;
    let mut review_outcome = match live_review_fanout(LiveReviewInput {
        config,
        adapter_id,
        spec: &spec,
        environment: &environment,
        lane_paths: &lane_paths,
        allocation: &allocation,
        changed_files: &attempt_outcome.changed_files,
        validation: &validation,
        diff_context: &diff_context,
    }) {
        Ok(outcome) => outcome,
        Err(error) => {
            transition_and_record(
                &lane_paths,
                &mut lane,
                7,
                HepaLaneState::Blocked,
                "live review fanout failed",
                "2026-06-16T00:00:07Z",
            )?;
            return finish_blocked_live_run(FinishBlockedInput {
                config,
                task,
                run_paths: &run_paths,
                lane_paths: &lane_paths,
                allocator: &allocator,
                lane: &mut lane,
                validation,
                review_signals: Vec::new(),
                arbitration: None,
                timing: live_timing_record(LiveTimingInput {
                    config,
                    adapter_id,
                    worker_duration_seconds: attempt_outcome.duration_seconds,
                    validation_duration_seconds,
                    review_duration_seconds: review_started.elapsed().as_secs_f64(),
                    reviewer_passes: 0,
                    terminal_phase: LivePipelinePhase::ReviewFailed,
                    repair_timing,
                }),
                reason: format!("Live review fanout failed before staging/PR creation: {error}"),
            });
        }
    };
    let mut review_duration_seconds = review_started.elapsed().as_secs_f64();
    for signal in &review_outcome.signals {
        write_json(
            &lane_paths
                .review_signal(&signal.review_id)
                .map_err(|error| error.to_string())?,
            signal,
        )
        .map_err(|error| error.to_string())?;
    }
    write_json(
        &lane_paths.lane_dir.join("review/arbitration.json"),
        &review_outcome.arbitration,
    )
    .map_err(|error| error.to_string())?;
    while !review_outcome.staging_allowed
        && lane.attempt_count < task_spec.max_total_rounds.clamp(1, 3)
    {
        let repair_round = lane.attempt_count + 1;
        transition_and_record(
            &lane_paths,
            &mut lane,
            7,
            HepaLaneState::Repairing,
            "live review findings routed to worker repair",
            "2026-06-16T00:00:07Z",
        )?;
        let repair_started = Instant::now();
        let repair_result = match run_live_repair_round(RunLiveRepairInput {
            config,
            lane_paths: &lane_paths,
            allocation: &allocation,
            task_spec: &task_spec,
            spec: spec.clone(),
            adapter_id,
            environment: environment.clone(),
            prior_prompt: prompt.clone(),
            failed_validation: validation.clone(),
            review_findings: repair_findings_from_review_signals(&review_outcome.signals),
            repair_round,
            first_changed_files: attempt_outcome.changed_files.clone(),
        }) {
            Ok(result) => result,
            Err(error) => {
                transition_and_record(
                    &lane_paths,
                    &mut lane,
                    8,
                    HepaLaneState::ReadyForHuman,
                    "live repair budget exhausted",
                    "2026-06-16T00:00:08Z",
                )?;
                return finish_blocked_live_run(FinishBlockedInput {
                    config,
                    task,
                    run_paths: &run_paths,
                    lane_paths: &lane_paths,
                    allocator: &allocator,
                    lane: &mut lane,
                    validation,
                    review_signals: review_outcome.signals,
                    arbitration: Some(review_outcome.arbitration),
                    timing: live_timing_record(LiveTimingInput {
                        config,
                        adapter_id,
                        worker_duration_seconds: attempt_outcome.duration_seconds,
                        validation_duration_seconds,
                        review_duration_seconds,
                        reviewer_passes: review_outcome.reviewer_passes,
                        terminal_phase: LivePipelinePhase::ReviewFailed,
                        repair_timing,
                    }),
                    reason: format!(
                        "Live review repair cannot continue without human manager intervention: {error}"
                    ),
                });
            }
        };
        let repair_duration_seconds = repair_started.elapsed().as_secs_f64();
        repair_timing = Some(LiveRepairTiming {
            brief_duration_seconds: repair_duration_seconds,
            worker_duration_seconds: repair_result.worker_duration_seconds,
            validation_duration_seconds: repair_result.validation_duration_seconds,
            completed: repair_result.validation.status == HepaValidationStatus::Passed,
        });
        validation = repair_result.validation;
        attempt_outcome.duration_seconds += repair_result.worker_duration_seconds;
        attempt_outcome.changed_files = repair_result.changed_files;
        usage_entries.extend(repair_result.usage_entries.clone());
        write_lane_cost_report_if_present(config, &lane_paths, &usage_entries)?;
        lane.attempt_count = repair_round;
        write_json(&lane_paths.validation_summary, &validation)
            .map_err(|error| error.to_string())?;
        if validation.status != HepaValidationStatus::Passed {
            continue;
        }
        transition_and_record(
            &lane_paths,
            &mut lane,
            8,
            HepaLaneState::Reviewing,
            "live review repair validation passed",
            "2026-06-16T00:00:08Z",
        )?;
        let retry_review_started = Instant::now();
        let retry_diff_context = collect_live_diff(&allocation.worktree_path)?;
        review_outcome = match live_review_fanout(LiveReviewInput {
            config,
            adapter_id,
            spec: &spec,
            environment: &environment,
            lane_paths: &lane_paths,
            allocation: &allocation,
            changed_files: &attempt_outcome.changed_files,
            validation: &validation,
            diff_context: &retry_diff_context,
        }) {
            Ok(outcome) => outcome,
            Err(error) => {
                transition_and_record(
                    &lane_paths,
                    &mut lane,
                    9,
                    HepaLaneState::ReadyForHuman,
                    "live review retry failed",
                    "2026-06-16T00:00:09Z",
                )?;
                return finish_blocked_live_run(FinishBlockedInput {
                    config,
                    task,
                    run_paths: &run_paths,
                    lane_paths: &lane_paths,
                    allocator: &allocator,
                    lane: &mut lane,
                    validation,
                    review_signals: Vec::new(),
                    arbitration: None,
                    timing: live_timing_record(LiveTimingInput {
                        config,
                        adapter_id,
                        worker_duration_seconds: attempt_outcome.duration_seconds,
                        validation_duration_seconds,
                        review_duration_seconds,
                        reviewer_passes: 0,
                        terminal_phase: LivePipelinePhase::ReviewFailed,
                        repair_timing,
                    }),
                    reason: format!(
                        "Live review retry failed after worker repair; human manager intervention required: {error}"
                    ),
                });
            }
        };
        review_duration_seconds += retry_review_started.elapsed().as_secs_f64();
        for signal in &review_outcome.signals {
            write_json(
                &lane_paths
                    .review_signal(&signal.review_id)
                    .map_err(|error| error.to_string())?,
                signal,
            )
            .map_err(|error| error.to_string())?;
        }
        write_json(
            &lane_paths.lane_dir.join("review/arbitration.json"),
            &review_outcome.arbitration,
        )
        .map_err(|error| error.to_string())?;
    }
    if !review_outcome.staging_allowed {
        transition_and_record(
            &lane_paths,
            &mut lane,
            7,
            HepaLaneState::ReadyForHuman,
            "live review repair budget exhausted",
            "2026-06-16T00:00:07Z",
        )?;
        return finish_blocked_live_run(FinishBlockedInput {
            config,
            task,
            run_paths: &run_paths,
            lane_paths: &lane_paths,
            allocator: &allocator,
            lane: &mut lane,
            validation,
            review_signals: review_outcome.signals,
            arbitration: Some(review_outcome.arbitration),
            timing: live_timing_record(LiveTimingInput {
                config,
                adapter_id,
                worker_duration_seconds: attempt_outcome.duration_seconds,
                validation_duration_seconds,
                review_duration_seconds,
                reviewer_passes: review_outcome.reviewer_passes,
                terminal_phase: LivePipelinePhase::ReviewFailed,
                repair_timing,
            }),
            reason: format!(
                "Live review fanout exhausted the bounded task/work/review rounds; human manager intervention is required before staging/PR: {}",
                review_outcome.blockers.join("; ")
            ),
        });
    }

    attempt_outcome.changed_files = discard_unrequested_package_manager_churn(
        &allocation.worktree_path,
        &attempt_outcome.changed_files,
        &task_spec.goal,
    )?;
    if let Some(reason) =
        manager_changed_file_policy_blocker(&attempt_outcome.changed_files, &task_spec.goal)
    {
        transition_and_record(
            &lane_paths,
            &mut lane,
            8,
            HepaLaneState::Blocked,
            "live manager changed-file policy blocked staging",
            "2026-06-16T00:00:08Z",
        )?;
        return finish_blocked_live_run(FinishBlockedInput {
            config,
            task,
            run_paths: &run_paths,
            lane_paths: &lane_paths,
            allocator: &allocator,
            lane: &mut lane,
            validation,
            review_signals: review_outcome.signals.clone(),
            arbitration: Some(review_outcome.arbitration.clone()),
            timing: live_timing_record(LiveTimingInput {
                config,
                adapter_id,
                worker_duration_seconds: attempt_outcome.duration_seconds,
                validation_duration_seconds,
                review_duration_seconds,
                reviewer_passes: review_outcome.reviewer_passes,
                terminal_phase: LivePipelinePhase::PrFailed,
                repair_timing,
            }),
            reason: format!(
                "Manager changed-file policy blocked staging before PR creation: {reason}"
            ),
        });
    }

    transition_and_record(
        &lane_paths,
        &mut lane,
        7,
        HepaLaneState::Staging,
        "live review fanout approved",
        "2026-06-16T00:00:07Z",
    )?;
    let staging_report = match HepaSafeStaging::new(&allocation.worktree_path)
        .stage_approved_files(&attempt_outcome.changed_files)
    {
        Ok(report) => report,
        Err(error) => {
            transition_and_record(
                &lane_paths,
                &mut lane,
                8,
                HepaLaneState::Blocked,
                "live manager staging failed",
                "2026-06-16T00:00:08Z",
            )?;
            return finish_blocked_live_run(FinishBlockedInput {
                config,
                task,
                run_paths: &run_paths,
                lane_paths: &lane_paths,
                allocator: &allocator,
                lane: &mut lane,
                validation,
                review_signals: review_outcome.signals.clone(),
                arbitration: Some(review_outcome.arbitration.clone()),
                timing: live_timing_record(LiveTimingInput {
                    config,
                    adapter_id,
                    worker_duration_seconds: attempt_outcome.duration_seconds,
                    validation_duration_seconds,
                    review_duration_seconds,
                    reviewer_passes: review_outcome.reviewer_passes,
                    terminal_phase: LivePipelinePhase::PrFailed,
                    repair_timing,
                }),
                reason: format!("Manager staging failed before commit/PR creation: {error}"),
            });
        }
    };
    let commit = match HepaManagerGitLifecycle::manager(&allocation.worktree_path).commit_staged(
        &HepaCommitMessage::new(commit_title(&sanitized_task_text(config)))
            .with_body(vec![format!("Task: {}", sanitized_task_text(config))]),
    ) {
        Ok(commit) => commit,
        Err(error) => {
            transition_and_record(
                &lane_paths,
                &mut lane,
                8,
                HepaLaneState::Blocked,
                "live manager commit failed",
                "2026-06-16T00:00:08Z",
            )?;
            return finish_blocked_live_run(FinishBlockedInput {
                config,
                task,
                run_paths: &run_paths,
                lane_paths: &lane_paths,
                allocator: &allocator,
                lane: &mut lane,
                validation,
                review_signals: review_outcome.signals.clone(),
                arbitration: Some(review_outcome.arbitration.clone()),
                timing: live_timing_record(LiveTimingInput {
                    config,
                    adapter_id,
                    worker_duration_seconds: attempt_outcome.duration_seconds,
                    validation_duration_seconds,
                    review_duration_seconds,
                    reviewer_passes: review_outcome.reviewer_passes,
                    terminal_phase: LivePipelinePhase::PrFailed,
                    repair_timing,
                }),
                reason: format!("Manager commit failed before PR creation: {error}"),
            });
        }
    };

    let timing_for_pr = live_timing_record(LiveTimingInput {
        config,
        adapter_id,
        worker_duration_seconds: attempt_outcome.duration_seconds,
        validation_duration_seconds,
        review_duration_seconds,
        reviewer_passes: review_outcome.reviewer_passes,
        terminal_phase: LivePipelinePhase::PrCreated,
        repair_timing,
    });
    let mut terminal_report = live_terminal_report(
        config,
        validation.clone(),
        review_outcome.signals.clone(),
        timing_for_pr.clone(),
        review_outcome.arbitration.clone(),
        None,
        vec![
            format!(
                "Live worker changed {} file(s).",
                attempt_outcome.changed_files.len()
            ),
            format!(
                "Changed files: {}.",
                summarize_changed_files(&attempt_outcome.changed_files)
            ),
            format!(
                "Validation passed for {} command(s).",
                validation.commands.len()
            ),
            format!(
                "Review fanout passed with {} reviewer signal(s) and arbitration status {}.",
                review_outcome.reviewer_passes, review_outcome.arbitration.status
            ),
            format!(
                "Manager staged {} file(s) and committed {}.",
                staging_report.staged_files.len(),
                commit.commit_sha
            ),
        ],
    );
    let branch = lane.branch.clone();
    let pr_request = match live_pr_request(
        config,
        &task_spec,
        &terminal_report,
        &lane,
        &attempt_outcome.changed_files,
        "main",
        &branch,
    ) {
        Ok(request) => request,
        Err(error) => {
            transition_and_record(
                &lane_paths,
                &mut lane,
                6,
                HepaLaneState::Blocked,
                "live Hermes PR intent failed",
                "2026-06-16T00:00:06Z",
            )?;
            return finish_blocked_live_run(FinishBlockedInput {
                config,
                task,
                run_paths: &run_paths,
                lane_paths: &lane_paths,
                allocator: &allocator,
                lane: &mut lane,
                validation,
                review_signals: review_outcome.signals.clone(),
                arbitration: Some(review_outcome.arbitration.clone()),
                timing: live_timing_record(LiveTimingInput {
                    config,
                    adapter_id,
                    worker_duration_seconds: attempt_outcome.duration_seconds,
                    validation_duration_seconds,
                    review_duration_seconds,
                    reviewer_passes: review_outcome.reviewer_passes,
                    terminal_phase: LivePipelinePhase::PrFailed,
                    repair_timing,
                }),
                reason: error,
            });
        }
    };
    let lifecycle = HepaManagerGitLifecycle::manager(&allocation.worktree_path);
    if let Err(error) = lifecycle.push_branch("origin", &branch, &HepaSystemProcessRunner) {
        transition_and_record(
            &lane_paths,
            &mut lane,
            6,
            HepaLaneState::Blocked,
            "live manager push failed",
            "2026-06-16T00:00:06Z",
        )?;
        return finish_blocked_live_run(FinishBlockedInput {
            config,
            task,
            run_paths: &run_paths,
            lane_paths: &lane_paths,
            allocator: &allocator,
            lane: &mut lane,
            validation,
            review_signals: review_outcome.signals.clone(),
            arbitration: Some(review_outcome.arbitration.clone()),
            timing: live_timing_record(LiveTimingInput {
                config,
                adapter_id,
                worker_duration_seconds: attempt_outcome.duration_seconds,
                validation_duration_seconds,
                review_duration_seconds,
                reviewer_passes: review_outcome.reviewer_passes,
                terminal_phase: LivePipelinePhase::PrFailed,
                repair_timing,
            }),
            reason: format!("Manager push failed before PR creation: {error}"),
        });
    }
    let pr = match lifecycle.create_pr(&pr_request, &HepaSystemProcessRunner) {
        Ok(pr) => pr,
        Err(error) => {
            transition_and_record(
                &lane_paths,
                &mut lane,
                6,
                HepaLaneState::Blocked,
                "live PR creation failed",
                "2026-06-16T00:00:06Z",
            )?;
            return finish_blocked_live_run(FinishBlockedInput {
                config,
                task,
                run_paths: &run_paths,
                lane_paths: &lane_paths,
                allocator: &allocator,
                lane: &mut lane,
                validation,
                review_signals: review_outcome.signals.clone(),
                arbitration: Some(review_outcome.arbitration.clone()),
                timing: live_timing_record(LiveTimingInput {
                    config,
                    adapter_id,
                    worker_duration_seconds: attempt_outcome.duration_seconds,
                    validation_duration_seconds,
                    review_duration_seconds,
                    reviewer_passes: review_outcome.reviewer_passes,
                    terminal_phase: LivePipelinePhase::PrFailed,
                    repair_timing,
                }),
                reason: format!("Manager PR creation failed: {error}"),
            });
        }
    };

    transition_and_record(
        &lane_paths,
        &mut lane,
        6,
        HepaLaneState::PrCreated,
        "live manager PR created",
        "2026-06-16T00:00:06Z",
    )?;
    transition_and_record(
        &lane_paths,
        &mut lane,
        7,
        HepaLaneState::Completed,
        "live done gate passed",
        "2026-06-16T00:00:07Z",
    )?;
    write_json(&lane_paths.lane_state, &lane).map_err(|error| error.to_string())?;

    let readiness = readiness_result(config);
    task.status = HepaTaskStatus::Completed;
    task.readiness = HepaReadinessState::Ready;
    task.completed_at = Some("2026-06-16T00:00:07Z".to_string());
    write_json(&run_paths.task_state, &task).map_err(|error| error.to_string())?;

    let timing = live_timing_record(LiveTimingInput {
        config,
        adapter_id,
        worker_duration_seconds: attempt_outcome.duration_seconds,
        validation_duration_seconds,
        review_duration_seconds,
        reviewer_passes: review_outcome.reviewer_passes,
        terminal_phase: LivePipelinePhase::Completed,
        repair_timing,
    });
    terminal_report.pr_url = Some(pr.url);
    terminal_report.timing = Some(timing.clone());
    terminal_report
        .summary
        .push("Manager-created PR is ready for validation cleanup.".to_string());
    lane_paths
        .write_timing_record(&timing)
        .map_err(|error| error.to_string())?;
    write_json(&lane_paths.final_report, &terminal_report).map_err(|error| error.to_string())?;
    write_json(&run_paths.run_state, &readiness).map_err(|error| error.to_string())?;
    record_terminal_memory(TerminalMemoryInput {
        control_root: &config.control_root,
        project_id: &task_spec.project_id,
        lane_id: &config.lane_id,
        lane_state: &HepaLaneState::Completed,
        adapter_id,
        prompt_pattern: &config.task_text,
        failure_pattern: None,
        validation_pass: true,
        reviewer_pass: true,
        pr_readiness: true,
        repair_convergence: repair_timing
            .as_ref()
            .map(|timing| timing.completed)
            .unwrap_or(true),
    });
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

struct ExecuteLiveAttemptInput<'a> {
    config: &'a HepaFakeRunConfig,
    lane_paths: &'a hepa_core::artifacts::HepaLaneArtifactPaths,
    allocation: &'a HepaWorktreeAllocation,
    spec: hepa_adapters::spec::HepaAdapterSpec,
    adapter_id: &'a str,
    environment: std::collections::BTreeMap<String, String>,
    attempt_id: &'a str,
    round: u32,
    prompt: String,
    started_at: &'a str,
    completed_at: &'a str,
}

struct LiveAttemptOutcome {
    duration_seconds: f64,
    changed_files: Vec<String>,
    usage_entries: Vec<HepaAdapterUsageEntry>,
}

fn execute_live_worker_attempt(
    input: ExecuteLiveAttemptInput<'_>,
) -> Result<LiveAttemptOutcome, String> {
    let ExecuteLiveAttemptInput {
        config,
        lane_paths,
        allocation,
        spec,
        adapter_id,
        environment,
        attempt_id,
        round,
        prompt,
        started_at,
        completed_at,
    } = input;
    let attempt_paths = lane_paths
        .attempt(attempt_id)
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(&attempt_paths.attempt_dir).map_err(|error| error.to_string())?;
    fs::write(attempt_paths.attempt_dir.join("prompt.md"), &prompt)
        .map_err(|error| error.to_string())?;
    let cost_class = spec.cost_class.clone();
    let output_capture = spec.output_capture.clone();
    if let Some(blocked_reason) =
        pi_local_worker_route_preflight_block_reason(adapter_id, &environment)
    {
        let attempt = HepaAttemptReport {
            schema_version: CONTRACT_SCHEMA_VERSION,
            attempt_id: attempt_id.to_string(),
            lane_id: config.lane_id.clone(),
            task_id: config.task_id.clone(),
            round,
            role: HepaAgentRole::Worker,
            adapter_id: adapter_id.to_string(),
            status: hepa_core::contracts::HepaAttemptStatus::Blocked,
            commands_run: vec![format!("live adapter invocation: {adapter_id}")],
            changed_files: collect_changed_files(&allocation.worktree_path).unwrap_or_default(),
            summary: vec![
                "Local Pi provider did not satisfy HEPA's tool-call preflight.".to_string(),
            ],
            blocked_reason: Some(blocked_reason.clone()),
            started_at: started_at.to_string(),
            completed_at: Some(completed_at.to_string()),
        };
        write_live_adapter_stream_logs(&attempt_paths, "", "")?;
        write_attempt(lane_paths, &attempt)?;
        return Err(blocked_reason);
    }
    let _local_generation_permit = match local_pi_generation_concurrency_permit(
        adapter_id,
        &environment,
        HepaAdapterRole::Worker,
    ) {
        Ok(permit) => permit,
        Err(error) => {
            let attempt = HepaAttemptReport {
                schema_version: CONTRACT_SCHEMA_VERSION,
                attempt_id: attempt_id.to_string(),
                lane_id: config.lane_id.clone(),
                task_id: config.task_id.clone(),
                round,
                role: HepaAgentRole::Worker,
                adapter_id: adapter_id.to_string(),
                status: hepa_core::contracts::HepaAttemptStatus::Blocked,
                commands_run: vec![format!("live adapter invocation: {adapter_id}")],
                changed_files: collect_changed_files(&allocation.worktree_path)
                    .unwrap_or_default(),
                summary: vec![
                    "Local-provider generation did not acquire its concurrency permit before the bounded wait elapsed."
                        .to_string(),
                ],
                blocked_reason: Some(error.clone()),
                started_at: started_at.to_string(),
                completed_at: Some(completed_at.to_string()),
            };
            write_live_adapter_stream_logs(&attempt_paths, "", "")?;
            write_attempt(lane_paths, &attempt)?;
            return Err(error);
        }
    };
    let invocation = HepaOneshotAdapterInvocation {
        spec,
        role: HepaAdapterRole::Worker,
        context: HepaAdapterTemplateContext {
            prompt_file: attempt_paths
                .attempt_dir
                .join("prompt.md")
                .display()
                .to_string(),
            worktree: allocation.worktree_path.display().to_string(),
            review_prompt_file: lane_paths.lane_dir.join("review.md").display().to_string(),
            output_file: attempt_paths.attempt_report.display().to_string(),
            review_output_file: lane_paths
                .lane_dir
                .join("review.json")
                .display()
                .to_string(),
            artifact_dir: lane_paths.lane_dir.display().to_string(),
        },
        prompt,
        environment: environment.clone(),
        monitor_policy: live_monitor_policy(),
    };
    let worker_started = Instant::now();
    let result = match HepaOneshotAdapterExecutor::new().run(&invocation) {
        Ok(result) => result,
        Err(error) => {
            write_live_adapter_stream_logs(&attempt_paths, &error.stdout, &error.stderr)?;
            let changed_files =
                collect_changed_files(&allocation.worktree_path).unwrap_or_default();
            if pi_local_monitor_stop_with_changed_files_can_continue(
                adapter_id,
                &environment,
                &error,
                &changed_files,
            ) {
                append_pi_monitor_stop_continued_stream_event(
                    lane_paths,
                    round,
                    &error.sanitized_summary(),
                    &changed_files,
                )?;
                let attempt = HepaAttemptReport {
                    schema_version: CONTRACT_SCHEMA_VERSION,
                    attempt_id: attempt_id.to_string(),
                    lane_id: config.lane_id.clone(),
                    task_id: config.task_id.clone(),
                    round,
                    role: HepaAgentRole::Worker,
                    adapter_id: adapter_id.to_string(),
                    status: hepa_core::contracts::HepaAttemptStatus::Completed,
                    commands_run: vec![format!("live adapter invocation: {adapter_id}")],
                    changed_files: changed_files.clone(),
                    summary: live_attempt_summary(
                        &allocation.worktree_path,
                        &error.stdout,
                        &error.stderr,
                    ),
                    blocked_reason: None,
                    started_at: started_at.to_string(),
                    completed_at: Some(completed_at.to_string()),
                };
                write_attempt(lane_paths, &attempt)?;
                return Ok(LiveAttemptOutcome {
                    duration_seconds: worker_started.elapsed().as_secs_f64(),
                    changed_files,
                    usage_entries: Vec::new(),
                });
            }
            let attempt = HepaAttemptReport {
                schema_version: CONTRACT_SCHEMA_VERSION,
                attempt_id: attempt_id.to_string(),
                lane_id: config.lane_id.clone(),
                task_id: config.task_id.clone(),
                round,
                role: HepaAgentRole::Worker,
                adapter_id: adapter_id.to_string(),
                status: if error.status.as_deref() == Some("blocked") {
                    hepa_core::contracts::HepaAttemptStatus::Blocked
                } else {
                    hepa_core::contracts::HepaAttemptStatus::Failed
                },
                commands_run: vec![format!("live adapter invocation: {adapter_id}")],
                changed_files,
                summary: live_attempt_summary(
                    &allocation.worktree_path,
                    &error.stdout,
                    &error.stderr,
                ),
                blocked_reason: Some(error.sanitized_summary()),
                started_at: started_at.to_string(),
                completed_at: Some(completed_at.to_string()),
            };
            write_attempt(lane_paths, &attempt)?;
            return Err(error.to_string());
        }
    };
    let duration_seconds = worker_started.elapsed().as_secs_f64();
    write_live_adapter_stream_logs(&attempt_paths, &result.stdout, &result.stderr)?;
    let changed_files = collect_changed_files(&allocation.worktree_path)?;
    if adapter_id == "pi" {
        match parse_pi_json_events(&result.stdout) {
            Ok(parsed) => {
                append_tool_summary_stream_event(lane_paths, round, &parsed)?;
                if parsed.tool_activity.is_empty() && changed_files.is_empty() {
                    let blocked_reason = "local_provider_no_tool_activity_or_changes: Pi completed without tool calls or changed files";
                    let attempt = HepaAttemptReport {
                        schema_version: CONTRACT_SCHEMA_VERSION,
                        attempt_id: attempt_id.to_string(),
                        lane_id: config.lane_id.clone(),
                        task_id: config.task_id.clone(),
                        round,
                        role: HepaAgentRole::Worker,
                        adapter_id: adapter_id.to_string(),
                        status: hepa_core::contracts::HepaAttemptStatus::Blocked,
                        commands_run: vec![result.command],
                        changed_files,
                        summary: live_attempt_summary(
                            &allocation.worktree_path,
                            &result.stdout,
                            &result.stderr,
                        ),
                        blocked_reason: Some(blocked_reason.to_string()),
                        started_at: started_at.to_string(),
                        completed_at: Some(completed_at.to_string()),
                    };
                    write_attempt(lane_paths, &attempt)?;
                    return Err(blocked_reason.to_string());
                }
            }
            Err(error) => {
                if pi_parse_error_is_eof_truncation(&error) && !changed_files.is_empty() {
                    append_pi_truncated_stream_event(
                        lane_paths,
                        round,
                        &error.to_string(),
                        &changed_files,
                    )?;
                } else {
                    let blocked_reason = pi_local_provider_output_failure_reason(
                        &error.to_string(),
                        &result.stdout,
                        &result.stderr,
                    );
                    let attempt = HepaAttemptReport {
                        schema_version: CONTRACT_SCHEMA_VERSION,
                        attempt_id: attempt_id.to_string(),
                        lane_id: config.lane_id.clone(),
                        task_id: config.task_id.clone(),
                        round,
                        role: HepaAgentRole::Worker,
                        adapter_id: adapter_id.to_string(),
                        status: hepa_core::contracts::HepaAttemptStatus::Blocked,
                        commands_run: vec![result.command],
                        changed_files,
                        summary: live_attempt_summary(
                            &allocation.worktree_path,
                            &result.stdout,
                            &result.stderr,
                        ),
                        blocked_reason: Some(blocked_reason.clone()),
                        started_at: started_at.to_string(),
                        completed_at: Some(completed_at.to_string()),
                    };
                    write_attempt(lane_paths, &attempt)?;
                    return Err(blocked_reason);
                }
            }
        }
    }
    let usage_entries = extract_live_usage_entries(
        adapter_id,
        attempt_id,
        &cost_class,
        &output_capture,
        &attempt_paths.attempt_report,
        &result.stdout,
    )?;
    let attempt = HepaAttemptReport {
        schema_version: CONTRACT_SCHEMA_VERSION,
        attempt_id: attempt_id.to_string(),
        lane_id: config.lane_id.clone(),
        task_id: config.task_id.clone(),
        round,
        role: HepaAgentRole::Worker,
        adapter_id: adapter_id.to_string(),
        status: if result.exit_code.unwrap_or_default() == 0 {
            hepa_core::contracts::HepaAttemptStatus::Completed
        } else {
            hepa_core::contracts::HepaAttemptStatus::Failed
        },
        commands_run: vec![result.command],
        changed_files: changed_files.clone(),
        summary: live_attempt_summary(&allocation.worktree_path, &result.stdout, &result.stderr),
        blocked_reason: result
            .exit_code
            .filter(|code| *code != 0)
            .map(|code| format!("adapter exited with code {code}")),
        started_at: started_at.to_string(),
        completed_at: Some(completed_at.to_string()),
    };
    write_attempt(lane_paths, &attempt)?;
    Ok(LiveAttemptOutcome {
        duration_seconds,
        changed_files,
        usage_entries,
    })
}

fn write_live_adapter_stream_logs(
    attempt_paths: &hepa_core::artifacts::HepaAttemptArtifactPaths,
    stdout: &str,
    stderr: &str,
) -> Result<(), String> {
    fs::write(&attempt_paths.stdout_log, stdout).map_err(|error| error.to_string())?;
    fs::write(&attempt_paths.stderr_log, stderr).map_err(|error| error.to_string())?;
    Ok(())
}

fn append_validation_stream_event(
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    round: u32,
    validation: &HepaValidationSummary,
) -> Result<(), String> {
    let stream_path = lane_paths
        .lane_dir
        .join("streams")
        .join("manager-validation-stream.jsonl");
    if let Some(parent) = stream_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stream_path)
        .map_err(|error| error.to_string())?;
    let event = serde_json::json!({
        "schema_version": CONTRACT_SCHEMA_VERSION,
        "source": "hepa-manager",
        "event": "validation_summary",
        "round": round,
        "status": &validation.status,
        "command_count": validation.commands.len(),
        "failure_type": &validation.failure_type,
        "summary": &validation.summary,
    });
    serde_json::to_writer(&mut file, &event).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.flush().map_err(|error| error.to_string())
}

fn append_tool_summary_stream_event(
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    round: u32,
    parsed: &HepaPiParsedOutput,
) -> Result<(), String> {
    let stream_path = lane_paths
        .lane_dir
        .join("streams")
        .join("manager-tool-summary-stream.jsonl");
    if let Some(parent) = stream_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut tool_activity = parsed.tool_activity.clone();
    tool_activity.sort();
    tool_activity.dedup();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stream_path)
        .map_err(|error| error.to_string())?;
    let event = serde_json::json!({
        "schema_version": CONTRACT_SCHEMA_VERSION,
        "source": "hepa-manager",
        "event": "tool_activity_summary",
        "round": round,
        "tool_event_count": parsed.tool_activity.len(),
        "tool_event_types": tool_activity,
        "final_message_bytes": parsed.final_message.len(),
        "final_message_preview": bounded_model_visible_summary(&parsed.final_message),
    });
    serde_json::to_writer(&mut file, &event).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.flush().map_err(|error| error.to_string())
}

fn append_pi_truncated_stream_event(
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    round: u32,
    error: &str,
    changed_files: &[String],
) -> Result<(), String> {
    let stream_path = lane_paths
        .lane_dir
        .join("streams")
        .join("manager-tool-summary-stream.jsonl");
    if let Some(parent) = stream_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stream_path)
        .map_err(|error| error.to_string())?;
    let event = serde_json::json!({
        "schema_version": CONTRACT_SCHEMA_VERSION,
        "source": "hepa-manager",
        "event": "pi_truncated_stream_continued",
        "round": round,
        "error": bounded_model_visible_summary(error),
        "changed_files": changed_files,
        "policy": "Pi stdout ended with an EOF-truncated JSON event after producing changed files; HEPA continued to validation/review instead of discarding the attempt."
    });
    serde_json::to_writer(&mut file, &event).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.flush().map_err(|error| error.to_string())
}

fn append_pi_monitor_stop_continued_stream_event(
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    round: u32,
    error: &str,
    changed_files: &[String],
) -> Result<(), String> {
    let stream_path = lane_paths
        .lane_dir
        .join("streams")
        .join("manager-tool-summary-stream.jsonl");
    if let Some(parent) = stream_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stream_path)
        .map_err(|error| error.to_string())?;
    let event = serde_json::json!({
        "schema_version": CONTRACT_SCHEMA_VERSION,
        "source": "hepa-manager",
        "event": "pi_local_monitor_stop_with_changes_continued",
        "round": round,
        "error": bounded_model_visible_summary(error),
        "changed_files": changed_files,
        "policy": "Local Pi stopped on the monitor budget after producing changed files; HEPA continued to manager-owned validation/review instead of discarding the attempt."
    });
    serde_json::to_writer(&mut file, &event).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.flush().map_err(|error| error.to_string())
}

fn pi_parse_error_is_eof_truncation(error: &hepa_adapters::pi::HepaPiParseError) -> bool {
    let message = error.to_string();
    message.contains("EOF while parsing")
}

fn pi_local_monitor_stop_with_changed_files_can_continue(
    adapter_id: &str,
    environment: &std::collections::BTreeMap<String, String>,
    error: &hepa_adapters::engine::HepaAdapterExecutionError,
    changed_files: &[String],
) -> bool {
    adapter_id == "pi"
        && !changed_files.is_empty()
        && pi_worker_model_needs_local_permit(environment)
        && error.status.as_deref() == Some("blocked")
        && error.field == "monitor"
        && (error.message.contains("stall:") || error.message.contains("timeout:"))
}

fn pi_local_provider_output_failure_reason(error: &str, stdout: &str, stderr: &str) -> String {
    if pi_local_provider_output_indicates_context_overflow(error, stdout, stderr) {
        format!(
            "local_provider_context_window_exceeded: {}",
            bounded_model_visible_summary(error)
        )
    } else if pi_local_provider_output_indicates_tool_protocol_failure(error, stdout, stderr) {
        format!(
            "local_provider_tool_call_protocol_error: {}",
            bounded_model_visible_summary(error)
        )
    } else {
        format!(
            "local_provider_empty_or_malformed_response: {}",
            bounded_model_visible_summary(error)
        )
    }
}

fn pi_local_provider_output_indicates_context_overflow(
    error: &str,
    stdout: &str,
    stderr: &str,
) -> bool {
    let combined = format!("{error}\n{stdout}\n{stderr}").to_ascii_lowercase();
    [
        "exceeds the available context size",
        "context window",
        "context length exceeded",
        "maximum context length",
        "maximum context",
        "too many tokens",
        "token limit",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
}

fn pi_local_provider_output_indicates_tool_protocol_failure(
    error: &str,
    stdout: &str,
    stderr: &str,
) -> bool {
    let combined = format!("{error}\n{stdout}\n{stderr}").to_ascii_lowercase();
    [
        "peg-native format",
        "does not match the expected",
        "invalid_tool",
        "malformed tool",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
}

fn bounded_model_visible_summary(message: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 240;
    let redacted = redact_secrets(message);
    let mut chars = redacted.chars();
    let preview = chars.by_ref().take(MAX_PREVIEW_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

struct RunLiveRepairInput<'a> {
    config: &'a HepaFakeRunConfig,
    lane_paths: &'a hepa_core::artifacts::HepaLaneArtifactPaths,
    allocation: &'a HepaWorktreeAllocation,
    task_spec: &'a HepaTaskSpec,
    spec: hepa_adapters::spec::HepaAdapterSpec,
    adapter_id: &'a str,
    environment: std::collections::BTreeMap<String, String>,
    prior_prompt: String,
    failed_validation: HepaValidationSummary,
    review_findings: Vec<HepaReviewFinding>,
    repair_round: u32,
    first_changed_files: Vec<String>,
}

#[derive(Debug)]
struct LiveRepairOutcome {
    worker_duration_seconds: f64,
    validation_duration_seconds: f64,
    validation: HepaValidationSummary,
    changed_files: Vec<String>,
    usage_entries: Vec<HepaAdapterUsageEntry>,
}

#[derive(Debug, Clone, Copy)]
struct LiveRepairTiming {
    brief_duration_seconds: f64,
    worker_duration_seconds: f64,
    validation_duration_seconds: f64,
    completed: bool,
}

#[derive(Debug, Serialize)]
struct LiveRepairBudgetArtifact {
    schema_version: u32,
    lane_id: String,
    repair_round: u32,
    max_repair_rounds: u32,
    max_total_attempts: u32,
    allowed: bool,
    reason: String,
}

#[derive(Debug, Serialize)]
struct LiveRepairBriefArtifact {
    schema_version: u32,
    lane_id: String,
    repair_round: u32,
    prompt: String,
    evidence: Vec<String>,
}

fn run_live_repair_round(input: RunLiveRepairInput<'_>) -> Result<LiveRepairOutcome, String> {
    let RunLiveRepairInput {
        config,
        lane_paths,
        allocation,
        task_spec,
        spec,
        adapter_id,
        environment,
        prior_prompt,
        failed_validation,
        review_findings,
        repair_round,
        first_changed_files,
    } = input;
    let max_total_attempts = task_spec.max_total_rounds.clamp(1, 3);
    let policy = HepaRepairRoundPolicy {
        max_repair_rounds: max_total_attempts.saturating_sub(1).max(1),
        max_total_attempts,
    };
    let repair_index = repair_round.saturating_sub(1).max(1);
    let decision = enforce_repair_round_budget(
        policy.clone(),
        HepaRepairRoundState {
            next_repair_round: repair_index,
            total_attempts_after_next: repair_round,
        },
    )
    .map_err(|error| format!("repair budget invalid: {}: {}", error.field, error.message))?;
    let repair_dir = lane_paths.lane_dir.join("repair");
    fs::create_dir_all(&repair_dir).map_err(|error| error.to_string())?;
    write_json(
        &repair_dir.join(format!("round-{repair_round}-budget.json")),
        &LiveRepairBudgetArtifact {
            schema_version: CONTRACT_SCHEMA_VERSION,
            lane_id: config.lane_id.clone(),
            repair_round,
            max_repair_rounds: policy.max_repair_rounds,
            max_total_attempts: policy.max_total_attempts,
            allowed: decision.allowed,
            reason: decision.reason.clone(),
        },
    )
    .map_err(|error| error.to_string())?;
    if !decision.allowed {
        return Err(format!(
            "repair budget blocked round {repair_round}: {}",
            decision.reason
        ));
    }

    let failing_commands = failed_validation
        .commands
        .iter()
        .filter(|command| command.exit_code != 0)
        .cloned()
        .collect::<Vec<_>>();
    let diff_state = collect_live_diff(&allocation.worktree_path)?;
    let mut review_findings = review_findings;
    review_findings.extend(
        memory_failure_context(&config.control_root, &task_spec.project_id)
            .into_iter()
            .enumerate()
            .map(|(index, evidence)| HepaReviewFinding {
                finding_id: format!("memory-failure-pattern-{}", index + 1),
                severity: HepaFindingSeverity::Low,
                category: "memory-failure-context".to_string(),
                evidence,
                in_scope: true,
                release_risk: false,
                recommended_action:
                    "Consult this prior failure pattern while preparing the repair.".to_string(),
                file_ref: None,
                line: None,
                message: "Prior HEPA memory failure pattern for this project.".to_string(),
                accepted: true,
            })
            .collect::<Vec<_>>(),
    );
    let brief = rewrite_repair_prompt_from_evidence(HepaRepairBriefInput {
        lane_id: config.lane_id.clone(),
        repair_round,
        prior_prompt,
        failing_commands,
        review_findings,
        diff_state,
        files_touched: first_changed_files,
    })
    .map_err(|error| format!("repair brief invalid: {}: {}", error.field, error.message))?;
    write_json(
        &repair_dir.join(format!("round-{repair_round}-brief.json")),
        &LiveRepairBriefArtifact {
            schema_version: CONTRACT_SCHEMA_VERSION,
            lane_id: brief.lane_id.clone(),
            repair_round: brief.repair_round,
            prompt: brief.prompt.clone(),
            evidence: brief.evidence.clone(),
        },
    )
    .map_err(|error| error.to_string())?;
    let repair_prompt = live_repair_worker_prompt_for_adapter(
        &brief.prompt,
        adapter_id,
        &hepa_core::config::HepaConfig::load(
            None,
            &std::collections::BTreeMap::new(),
            HepaConfigOverrides {
                pi_model: std::env::var("HEPA_PI_MODEL").ok(),
                pi_base_url: optional_env("HEPA_PI_BASE_URL"),
                ..HepaConfigOverrides::default()
            },
        )
        .map_err(|error| error.to_string())?,
    );
    fs::write(
        repair_dir.join(format!("round-{repair_round}-prompt.md")),
        &repair_prompt,
    )
    .map_err(|error| error.to_string())?;
    let attempt_id = format!("attempt-{repair_round}");
    let attempt = execute_live_worker_attempt(ExecuteLiveAttemptInput {
        config,
        lane_paths,
        allocation,
        spec,
        adapter_id,
        environment,
        attempt_id: &attempt_id,
        round: repair_round,
        prompt: repair_prompt,
        started_at: "2026-06-16T00:00:04Z",
        completed_at: "2026-06-16T00:00:05Z",
    })?;
    let validation_started = Instant::now();
    let validation = run_live_validation(&allocation.worktree_path, task_spec);
    let validation_duration_seconds = validation_started.elapsed().as_secs_f64();
    write_json(
        &repair_dir.join(format!("round-{repair_round}-validation.json")),
        &validation,
    )
    .map_err(|error| error.to_string())?;
    append_validation_stream_event(lane_paths, repair_round, &validation)?;
    Ok(LiveRepairOutcome {
        worker_duration_seconds: attempt.duration_seconds,
        validation_duration_seconds,
        validation,
        changed_files: attempt.changed_files,
        usage_entries: attempt.usage_entries,
    })
}

fn extract_live_usage_entries(
    adapter_id: &str,
    invocation_id: &str,
    cost_class: &hepa_adapters::spec::HepaAdapterCostClass,
    output_capture: &HepaAdapterOutputCapture,
    adapter_output_file: &Path,
    stdout: &str,
) -> Result<Vec<HepaAdapterUsageEntry>, String> {
    let raw = match output_capture {
        HepaAdapterOutputCapture::Stdout => stdout.to_string(),
        HepaAdapterOutputCapture::AdapterFile => {
            fs::read_to_string(adapter_output_file).unwrap_or_default()
        }
    };
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let extraction = extract_adapter_usage(&raw, adapter_id, invocation_id, cost_class)
        .map_err(|error| format!("adapter usage parse failed: {error}"))?;
    Ok(extraction.entry.into_iter().collect())
}

fn write_lane_cost_report_if_present(
    config: &HepaFakeRunConfig,
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    entries: &[HepaAdapterUsageEntry],
) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let report = HepaLaneCostReport::from_entries(
        config.run_id.clone(),
        config.task_id.clone(),
        config.lane_id.clone(),
        entries.to_vec(),
        "2026-06-16T00:00:06Z",
    )
    .map_err(|error| error.to_string())?;
    lane_paths
        .write_cost_report(&report)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn optional_env(key: &str) -> Option<Option<String>> {
    match std::env::var(key) {
        Ok(value) => {
            let value = value.trim().to_string();
            Some(if value.is_empty() { None } else { Some(value) })
        }
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => None,
    }
}

fn live_run_brief(config: &HepaFakeRunConfig) -> Result<Option<HepaHermesRunBrief>, String> {
    if let Ok(command) = std::env::var("HEPA_HERMES_RUN_BRIEF_COMMAND") {
        return live_run_brief_from_runtime_command(&command, config).map(Some);
    }
    let default_brief_path = default_hermes_run_brief_path(config);
    if default_brief_path.exists() {
        let brief = hermes_run_brief_from_file(&default_brief_path)?;
        if brief.task_id != config.task_id {
            return Err(format!(
                "Hermes run brief task_id mismatch: expected {} got {}",
                config.task_id, brief.task_id
            ));
        }
        if brief.lane_id != config.lane_id {
            return Err(format!(
                "Hermes run brief lane_id mismatch: expected {} got {}",
                config.lane_id, brief.lane_id
            ));
        }
        return Ok(Some(brief));
    }
    let Ok(brief_path) = std::env::var("HEPA_HERMES_RUN_BRIEF_FILE") else {
        if hermes_required() {
            return Err(
                "Hermes-present mode requires HEPA_HERMES_RUN_BRIEF_COMMAND or HEPA_HERMES_RUN_BRIEF_FILE"
                    .to_string(),
            );
        }
        return Ok(None);
    };
    let brief = hermes_run_brief_from_file(Path::new(&brief_path))?;
    if brief.task_id != config.task_id {
        return Err(format!(
            "Hermes run brief task_id mismatch: expected {} got {}",
            config.task_id, brief.task_id
        ));
    }
    if brief.lane_id != config.lane_id {
        return Err(format!(
            "Hermes run brief lane_id mismatch: expected {} got {}",
            config.lane_id, brief.lane_id
        ));
    }
    Ok(Some(brief))
}

fn default_hermes_run_brief_path(config: &HepaFakeRunConfig) -> PathBuf {
    config
        .control_root
        .join("hermes-run-brief")
        .join(&config.run_id)
        .join(&config.lane_id)
        .join("hermes-run-brief.runtime.json")
}

fn live_run_brief_from_runtime_command(
    command: &str,
    config: &HepaFakeRunConfig,
) -> Result<HepaHermesRunBrief, String> {
    let brief_dir = config
        .control_root
        .join("hermes-run-brief")
        .join(&config.run_id)
        .join(&config.lane_id);
    fs::create_dir_all(&brief_dir).map_err(|error| error.to_string())?;
    let context_path = brief_dir.join("hermes-run-brief-context.json");
    let output_path = brief_dir.join("hermes-run-brief.runtime.json");
    let stdout_path = brief_dir.join("hermes-run-brief-runtime.stdout.log");
    let stderr_path = brief_dir.join("hermes-run-brief-runtime.stderr.log");
    let context = serde_json::json!({
        "schema_version": CONTRACT_SCHEMA_VERSION,
        "profile_id": "hepa-worker",
        "run_id": config.run_id,
        "task_id": config.task_id,
        "lane_id": config.lane_id,
        "task_text": sanitized_task_text(config),
        "artifact_output": output_path,
    });
    write_json(&context_path, &context).map_err(|error| error.to_string())?;
    run_hermes_profile_runtime_command(HermesProfileRuntimeCommand {
        command,
        profile_id: "hepa-worker",
        context_path: &context_path,
        output_path: &output_path,
        stdout_path: &stdout_path,
        stderr_path: &stderr_path,
    })?;
    let brief = hermes_run_brief_from_file(&output_path)?;
    if brief.task_id != config.task_id {
        return Err(format!(
            "Hermes worker runtime brief task_id mismatch: expected {} got {}",
            config.task_id, brief.task_id
        ));
    }
    if brief.lane_id != config.lane_id {
        return Err(format!(
            "Hermes worker runtime brief lane_id mismatch: expected {} got {}",
            config.lane_id, brief.lane_id
        ));
    }
    Ok(brief)
}

fn hermes_run_brief_from_file(brief_path: &Path) -> Result<HepaHermesRunBrief, String> {
    let raw = fs::read_to_string(brief_path)
        .map_err(|error| format!("Hermes run brief could not be read: {error}"))?;
    let brief: HepaHermesRunBrief = serde_json::from_str(&raw)
        .map_err(|error| format!("Hermes run brief JSON is invalid: {error}"))?;
    brief
        .validate()
        .map_err(|error| format!("Hermes run brief failed validation: {error}"))?;
    Ok(brief)
}

fn live_pr_request(
    config: &HepaFakeRunConfig,
    task_spec: &HepaTaskSpec,
    terminal_report: &HepaTerminalTaskReport,
    lane: &HepaLane,
    changed_files: &[String],
    base_branch: &str,
    head_branch: &str,
) -> Result<HepaPrRequest, String> {
    if let Ok(command) = std::env::var("HEPA_HERMES_PR_INTENT_COMMAND") {
        return pr_request_from_hermes_intent_runtime_command(
            &command,
            config,
            task_spec,
            terminal_report,
            lane,
            changed_files,
            base_branch.to_string(),
            head_branch.to_string(),
        );
    }
    let default_intent_path = default_hermes_pr_intent_path(config);
    if default_intent_path.exists() {
        return pr_request_from_hermes_intent_file(
            &default_intent_path,
            task_spec,
            terminal_report,
            lane,
            base_branch.to_string(),
            head_branch.to_string(),
        );
    }
    if let Ok(intent_path) = std::env::var("HEPA_HERMES_PR_INTENT_FILE") {
        return pr_request_from_hermes_intent_file(
            Path::new(&intent_path),
            task_spec,
            terminal_report,
            lane,
            base_branch.to_string(),
            head_branch.to_string(),
        );
    }
    if hermes_required() {
        return Err(
            "Hermes-present mode requires HEPA_HERMES_PR_INTENT_COMMAND or HEPA_HERMES_PR_INTENT_FILE"
                .to_string(),
        );
    }

    let pr_body = build_pr_body(&HepaPrBodyInput {
        task_spec,
        terminal_report,
        lane,
        external_card_id: None,
    });
    Ok(HepaPrRequest {
        title: format!(
            "HEPA validation: {}",
            commit_title(&sanitized_task_text(config))
        ),
        body: pr_body,
        base_branch: base_branch.to_string(),
        head_branch: head_branch.to_string(),
    })
}

fn default_hermes_pr_intent_path(config: &HepaFakeRunConfig) -> PathBuf {
    config
        .control_root
        .join("hermes-pr-intent")
        .join(&config.run_id)
        .join(&config.lane_id)
        .join("hermes-pr-intent.runtime.json")
}

#[allow(clippy::too_many_arguments)]
fn pr_request_from_hermes_intent_runtime_command(
    command: &str,
    config: &HepaFakeRunConfig,
    task_spec: &HepaTaskSpec,
    terminal_report: &HepaTerminalTaskReport,
    lane: &HepaLane,
    changed_files: &[String],
    base_branch: String,
    head_branch: String,
) -> Result<HepaPrRequest, String> {
    let intent_dir = config
        .control_root
        .join("hermes-pr-intent")
        .join(&config.run_id)
        .join(&config.lane_id);
    fs::create_dir_all(&intent_dir).map_err(|error| error.to_string())?;
    let context_path = intent_dir.join("hermes-pr-intent-context.json");
    let output_path = intent_dir.join("hermes-pr-intent.runtime.json");
    let stdout_path = intent_dir.join("hermes-pr-intent-runtime.stdout.log");
    let stderr_path = intent_dir.join("hermes-pr-intent-runtime.stderr.log");
    let context = serde_json::json!({
        "schema_version": CONTRACT_SCHEMA_VERSION,
        "profile_id": "hepa-manager",
        "task_id": config.task_id,
        "lane_id": config.lane_id,
        "task_spec": task_spec,
        "terminal_report": terminal_report,
        "changed_files": changed_files,
        "lane": lane,
        "base_branch": &base_branch,
        "head_branch": &head_branch,
        "artifact_output": output_path,
    });
    write_json(&context_path, &context).map_err(|error| error.to_string())?;
    run_hermes_profile_runtime_command(HermesProfileRuntimeCommand {
        command,
        profile_id: "hepa-manager",
        context_path: &context_path,
        output_path: &output_path,
        stdout_path: &stdout_path,
        stderr_path: &stderr_path,
    })?;
    pr_request_from_hermes_intent_file(
        &output_path,
        task_spec,
        terminal_report,
        lane,
        base_branch,
        head_branch,
    )
}

fn pr_request_from_hermes_intent_file(
    intent_path: &Path,
    task_spec: &HepaTaskSpec,
    terminal_report: &HepaTerminalTaskReport,
    lane: &HepaLane,
    base_branch: String,
    head_branch: String,
) -> Result<HepaPrRequest, String> {
    let raw = fs::read_to_string(intent_path)
        .map_err(|error| format!("Hermes PR intent could not be read: {error}"))?;
    let intent: HepaHermesPrIntent = serde_json::from_str(&raw)
        .map_err(|error| format!("Hermes PR intent JSON is invalid: {error}"))?;
    let evidence = HepaPrBodyInput {
        task_spec,
        terminal_report,
        lane,
        external_card_id: None,
    };
    pr_request_from_hermes_intent_with_run_evidence(&intent, &evidence, base_branch, head_branch)
        .map_err(|error| format!("Hermes PR intent failed validation: {error}"))
}

fn live_monitor_policy() -> HepaMonitorPolicy {
    const DEFAULT_PI_LIVE_TIMEOUT_MS: u64 = 300_000;
    const DEFAULT_PI_LIVE_STALL_MS: u64 = 240_000;
    const MAX_PI_LIVE_BUDGET_MS: u64 = 600_000;
    HepaMonitorPolicy {
        timeout_ms: Some(live_budget_ms(
            "HEPA_PI_LIVE_TIMEOUT_MS",
            DEFAULT_PI_LIVE_TIMEOUT_MS,
            MAX_PI_LIVE_BUDGET_MS,
        )),
        stall_ms: Some(live_budget_ms(
            "HEPA_PI_LIVE_STALL_MS",
            DEFAULT_PI_LIVE_STALL_MS,
            MAX_PI_LIVE_BUDGET_MS,
        )),
        ..HepaMonitorPolicy::default()
    }
}

fn live_budget_ms(key: &str, default_ms: u64, max_ms: u64) -> u64 {
    clamp_live_budget_ms(std::env::var(key).ok().as_deref(), default_ms, max_ms)
}

fn clamp_live_budget_ms(raw: Option<&str>, default_ms: u64, max_ms: u64) -> u64 {
    raw.and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.clamp(1, max_ms))
        .unwrap_or(default_ms)
}

fn live_worker_prompt(task_text: &str) -> String {
    let mut prompt = format!(
        "You are HEPA's live stress-test worker.\n\nTask:\n{task_text}\n\nRepository worktree: current directory.\n\nExecution rules:\n- You are already running inside the lane worktree.\n- Make only the changes needed to satisfy the task.\n- Use relative paths when reading or editing files.\n- Do not create commits, branches, tags, pull requests, or Git remotes; HEPA owns the Git lifecycle.\n- Do not read or print provider keys, credentials, or unrelated local files.\n- Do not run package install commands that can rewrite lockfiles or workspace/package-manager configuration unless the task explicitly asks for dependency or package-manager changes.\n- Run the smallest relevant validation command when practical, but only if it does not mutate dependency lockfiles or workspace/package-manager configuration; manager-owned validation will run the configured validation commands after your attempt.\n- Finish by reporting changed files, validation results if you ran a non-mutating check, and any blockers.\n",
    );
    prompt.push_str(repo_privacy_rules());
    prompt
}

fn live_worker_prompt_for_adapter(
    task_text: &str,
    adapter_id: &str,
    config: &hepa_core::config::HepaConfig,
) -> String {
    let mut prompt = live_worker_prompt(task_text);
    if adapter_id == "pi" {
        prompt.push_str(pi_tool_path_rules());
    }
    if adapter_id == "pi" && pi_model_is_local(&config.pi.model, &config.pi.base_url) {
        prompt.push_str(pi_local_provider_bounded_task_rules());
    }
    if adapter_id == "pi" && pi_model_needs_no_think_suffix(&config.pi.model, &config.pi.base_url) {
        prompt.push_str(
            "\nAdapter-local model note: answer directly and do not emit hidden reasoning. /no_think\n",
        );
    }
    prompt
}

fn live_repair_worker_prompt_for_adapter(
    repair_prompt: &str,
    adapter_id: &str,
    config: &hepa_core::config::HepaConfig,
) -> String {
    let mut prompt = format!(
        "{repair_prompt}\n\nExecution rules:\n- You are already running inside the same lane worktree.\n- Fix only the evidenced failures named above.\n- Do not create commits, branches, tags, pull requests, or Git remotes; HEPA owns the Git lifecycle.\n- Do not read or print provider keys, credentials, or unrelated local files.\n- Finish by reporting changed files, rerun validation results, and any remaining blockers.\n"
    );
    prompt.push_str(repo_privacy_rules());
    if adapter_id == "pi" {
        prompt.push_str(pi_tool_path_rules());
    }
    if adapter_id == "pi" && pi_model_is_local(&config.pi.model, &config.pi.base_url) {
        prompt.push_str(pi_local_provider_bounded_task_rules());
        prompt.push_str(
            "\nLocal-provider repair override: do not rerun the failing validation commands yourself; make the minimal repair and let HEPA's manager rerun validation.\n",
        );
    }
    if adapter_id == "pi" && pi_model_needs_no_think_suffix(&config.pi.model, &config.pi.base_url) {
        prompt.push_str(
            "\nAdapter-local model note: answer directly and do not emit hidden reasoning. /no_think\n",
        );
    }
    prompt
}

fn pi_tool_path_rules() -> &'static str {
    "\nPi tool path rules:\n- Treat the lane worktree root as the current directory for all read/edit/write calls.\n- If a find call uses a search root such as `./src` and returns `app/file.tsx`, read or edit `src/app/file.tsx`.\n- Prefer find from `.` when practical so returned paths are already worktree-root relative.\n- Before editing a file discovered by find, verify the exact worktree-relative path exists.\n"
}

fn repo_privacy_rules() -> &'static str {
    "\nRepository privacy rules:\n- Do not write absolute local filesystem paths, home-directory paths, usernames, hostnames, machine names, HEPA control/archive/worktree paths, provider names, or credentials into repository files or tests.\n- When a test needs a path-like value, use a relative path or neutral placeholder fixture such as `fixtures/example-project`.\n"
}

fn pi_local_provider_bounded_task_rules() -> &'static str {
    "\nLocal-provider bounded task rules:\n- Keep the first attempt small and direct: inspect only the files needed for the task, edit the smallest existing file set that satisfies it, then stop.\n- Use at most a few targeted grep/find/read calls before editing; never repeatedly re-read the same broad tree or file family.\n- Prefer grep/find and targeted reads over reading large test, lockfile, bundle, build-output, archive, or generated files into context.\n- Do not create new styling, helper, config, package manager, lockfile, or test-support files unless the task explicitly requires them or no existing file can satisfy the acceptance criteria.\n- Do not run dependency installs or package-manager commands that can rewrite lockfiles.\n- Do not run validation commands yourself; HEPA's manager owns validation after your edit and will run the task's validation commands.\n- Keep the final response concise: changed files, what changed, and any blocker. Do not include reasoning traces, transcript-like notes, or repeated planning.\n- If you cannot find the correct file after a few targeted reads/finds, report the blocker instead of looping.\n"
}

fn pi_model_needs_no_think_suffix(model: &str, base_url: &Option<String>) -> bool {
    let model = model.to_ascii_lowercase();
    let is_reasoning_local_model = ["qwen", "gpt-oss", "deepseek-r1", "reasoning", "r1-", "-r1"]
        .iter()
        .any(|needle| model.contains(needle));
    is_reasoning_local_model && pi_model_is_local(&model, base_url)
}

fn pi_model_is_local(model: &str, base_url: &Option<String>) -> bool {
    let model = model.to_ascii_lowercase();
    model.starts_with("local/")
        || model.starts_with("ollama/")
        || model.starts_with("llama-cpp/")
        || model.starts_with("vllm/")
        || model.starts_with("mlx-community/")
        || base_url.as_deref().is_some_and(is_loopback_url)
}

fn is_loopback_url(value: &str) -> bool {
    value.contains("127.0.0.1") || value.contains("localhost") || value.contains("[::1]")
}

fn live_task_spec(config: &HepaFakeRunConfig) -> HepaTaskSpec {
    let validation_commands = live_validation_commands(&config.task_text);
    HepaTaskSpec {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: config.task_id.clone(),
        project_id: "project-1".to_string(),
        goal: sanitized_task_text(config),
        non_goals: vec![
            "Adapter must not commit, push, create branches, or open pull requests.".to_string(),
        ],
        expected_areas: expected_areas_from_task(&config.task_text),
        acceptance_criteria: vec![
            "Live worker changes only task-relevant files.".to_string(),
            "Manager-owned validation passes.".to_string(),
            "Manager-owned staging, commit, and pull request creation execute.".to_string(),
        ],
        validation_commands,
        dependencies: Vec::new(),
        target_branch: Some("main".to_string()),
        risk_level: HepaRiskLevel::Low,
        max_total_rounds: 3,
        created_at: "2026-06-16T00:00:00Z".to_string(),
    }
}

fn live_task_spec_from_hermes_brief(
    config: &HepaFakeRunConfig,
    brief: &HepaHermesRunBrief,
) -> HepaTaskSpec {
    HepaTaskSpec {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: config.task_id.clone(),
        project_id: "project-1".to_string(),
        goal: sanitized_task_text(config),
        non_goals: vec![
            "Adapter must not commit, push, create branches, or open pull requests.".to_string(),
            "Hermes worker brief scope must not be expanded by the coding adapter.".to_string(),
        ],
        expected_areas: if brief.expected_areas.is_empty() {
            expected_areas_from_task(&brief.task_prompt)
        } else {
            brief.expected_areas.clone()
        },
        acceptance_criteria: brief.acceptance_criteria.clone(),
        validation_commands: brief.validation_commands.clone(),
        dependencies: Vec::new(),
        target_branch: Some("main".to_string()),
        risk_level: HepaRiskLevel::Low,
        max_total_rounds: brief.max_total_rounds,
        created_at: "2026-06-16T00:00:00Z".to_string(),
    }
}

fn expected_areas_from_task(task_text: &str) -> Vec<String> {
    let mut areas: Vec<String> = task_text
        .split(|character: char| character.is_whitespace() || matches!(character, ',' | ';'))
        .map(|token| token.trim_matches(|character: char| matches!(character, '`' | '\'' | '"')))
        .filter(|token| {
            token.contains('/')
                && [
                    ".tsx", ".ts", ".jsx", ".js", ".md", ".json", ".toml", ".yaml", ".yml",
                ]
                .iter()
                .any(|suffix| token.ends_with(suffix))
        })
        .map(str::to_string)
        .collect();
    areas.sort();
    areas.dedup();
    if areas.is_empty() {
        vec!["<task-relevant-files>".to_string()]
    } else {
        areas
    }
}

fn live_validation_commands(task_text: &str) -> Vec<String> {
    if task_text.contains("login-form.test.tsx") {
        return vec!["npx vitest run login-form.test.tsx".to_string()];
    }

    let mut commands = Vec::new();
    for command in [
        "pnpm install --frozen-lockfile --offline",
        "pnpm format:check",
        "yarn install --frozen-lockfile",
        "yarn test",
        "yarn test:e2e",
        "yarn build",
        "npx vitest run login-form.test.tsx",
        "pnpm --filter @todo/api-gateway test",
        "pnpm --filter @todo/services test",
        "git diff --check",
    ] {
        if task_text_contains_command(task_text, command) {
            commands.push(command.to_string());
        }
    }
    if !commands.is_empty() {
        if commands
            .iter()
            .any(|command| command == "yarn test:e2e" || command == "yarn build")
            && !commands
                .iter()
                .any(|command| command == "yarn install --frozen-lockfile")
        {
            commands.insert(0, "yarn install --frozen-lockfile".to_string());
        }
        commands.dedup();
        return commands;
    }

    if task_text.to_ascii_lowercase().contains("no-tests-detected") {
        Vec::new()
    } else {
        vec!["git diff --check".to_string()]
    }
}

fn task_text_contains_command(task_text: &str, command: &str) -> bool {
    if command == "yarn test" {
        task_text
            .split(['\n', ';', '.', ','])
            .any(|part| part.trim() == command)
    } else {
        task_text.contains(command)
    }
}

fn run_live_validation(worktree: &Path, task_spec: &HepaTaskSpec) -> HepaValidationSummary {
    if task_spec.validation_commands.is_empty() {
        return HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::NoTestsDetected,
            commands: Vec::new(),
            no_tests_detected: true,
            failure_type: None,
            summary: vec!["No validation command detected for this task.".to_string()],
        };
    }

    let mut commands = Vec::new();
    let mut summary = Vec::new();
    let mut all_passed = true;
    for command in &task_spec.validation_commands {
        let started = Instant::now();
        let output = run_safe_validation_command(worktree, command);
        let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        match output {
            Ok((exit_code, stdout, stderr)) => {
                if exit_code != 0 {
                    all_passed = false;
                }
                commands.push(HepaValidationCommandResult {
                    command: command.clone(),
                    exit_code,
                    duration_ms,
                });
                summary.push(format!(
                    "`{}` exited {}. stdout: {} stderr: {}",
                    command,
                    exit_code,
                    sanitize_validation_output(worktree, &stdout),
                    sanitize_validation_output(worktree, &stderr)
                ));
            }
            Err(error) => {
                all_passed = false;
                commands.push(HepaValidationCommandResult {
                    command: command.clone(),
                    exit_code: -1,
                    duration_ms,
                });
                summary.push(format!("`{command}` failed to launch: {error}"));
            }
        }
    }

    HepaValidationSummary {
        schema_version: CONTRACT_SCHEMA_VERSION,
        status: if all_passed {
            HepaValidationStatus::Passed
        } else {
            HepaValidationStatus::Failed
        },
        commands,
        no_tests_detected: false,
        failure_type: (!all_passed).then(|| "validation_failed".to_string()),
        summary,
    }
}

fn live_force_first_validation_failure() -> bool {
    matches!(
        std::env::var("HEPA_LIVE_FORCE_FIRST_VALIDATION_FAILURE")
            .ok()
            .as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

fn live_force_secret_path_change() -> bool {
    matches!(
        std::env::var("HEPA_LIVE_FORCE_SECRET_PATH_CHANGE")
            .ok()
            .as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

fn live_force_git_lifecycle_violation() -> bool {
    matches!(
        std::env::var("HEPA_LIVE_FORCE_GIT_LIFECYCLE_VIOLATION")
            .ok()
            .as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

fn live_force_review_block() -> bool {
    matches!(
        std::env::var("HEPA_LIVE_FORCE_REVIEW_BLOCK")
            .ok()
            .as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

fn controlled_git_lifecycle_block() -> Result<hepa_core::hard_blockers::HepaBlockedStatus, String> {
    let stop = HepaMonitorPolicy::default()
        .check_command("adapter && git commit -m rs-5-2-violation")
        .expect_err("controlled git lifecycle command must be blocked");
    Ok(block_from_monitor_stop(&stop))
}

fn first_secret_like_changed_path(changed_files: &[String]) -> Option<String> {
    changed_files
        .iter()
        .find(|path| is_live_secret_like_path(path))
        .cloned()
}

fn manager_changed_file_policy_blocker(
    changed_files: &[String],
    task_text: &str,
) -> Option<String> {
    let unexpected_lockfiles: Vec<&str> = changed_files
        .iter()
        .map(String::as_str)
        .filter(|path| is_dependency_lockfile_change(path))
        .collect();
    if !unexpected_lockfiles.is_empty() && !task_allows_dependency_lockfile_changes(task_text) {
        return Some(format!(
            "dependency lockfile changes require explicit dependency/package-manager intent: {}",
            unexpected_lockfiles.join(", ")
        ));
    }

    let unexpected_workspace_configs: Vec<&str> = changed_files
        .iter()
        .map(String::as_str)
        .filter(|path| is_workspace_config_change(path))
        .collect();
    if !unexpected_workspace_configs.is_empty() && !task_allows_workspace_config_changes(task_text)
    {
        return Some(format!(
            "workspace/package-manager config changes require explicit workspace/package-manager intent: {}",
            unexpected_workspace_configs.join(", ")
        ));
    }

    None
}

fn prepare_live_worktree_dependency_reuse(repo_path: &Path, worktree: &Path) -> Result<(), String> {
    for dependency_dir in ["node_modules"] {
        let source = repo_path.join(dependency_dir);
        let destination = worktree.join(dependency_dir);
        if !source.exists() || destination.exists() {
            continue;
        }
        symlink_dependency_dir(&source, &destination).map_err(|error| {
            format!(
                "failed to reuse dependency dir {} at {}: {error}",
                source.display(),
                destination.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(unix)]
fn symlink_dependency_dir(source: &Path, destination: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(source, destination)
}

#[cfg(not(unix))]
fn symlink_dependency_dir(_source: &Path, _destination: &Path) -> io::Result<()> {
    Ok(())
}

fn discard_unrequested_package_manager_churn(
    worktree: &Path,
    changed_files: &[String],
    task_text: &str,
) -> Result<Vec<String>, String> {
    let should_restore = |path: &str| {
        (is_dependency_lockfile_change(path) && !task_allows_dependency_lockfile_changes(task_text))
            || (is_workspace_config_change(path)
                && !task_allows_workspace_config_changes(task_text))
    };
    let restore_paths = changed_files
        .iter()
        .filter(|path| should_restore(path))
        .cloned()
        .collect::<Vec<_>>();
    if restore_paths.is_empty() {
        return Ok(changed_files.to_vec());
    }

    let mut command = Command::new("git");
    command.current_dir(worktree).args(["checkout", "--"]);
    for path in &restore_paths {
        command.arg(path);
    }
    let output = command.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "failed to restore unrequested package-manager changes: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    collect_changed_files(worktree)
}

fn is_dependency_lockfile_change(path: &str) -> bool {
    matches!(
        path.rsplit('/').next().unwrap_or(path),
        "pnpm-lock.yaml" | "package-lock.json" | "yarn.lock" | "bun.lock" | "bun.lockb"
    )
}

fn is_workspace_config_change(path: &str) -> bool {
    matches!(
        path.rsplit('/').next().unwrap_or(path),
        "pnpm-workspace.yaml" | "lerna.json" | "nx.json" | "turbo.json" | "rush.json"
    )
}

fn task_allows_dependency_lockfile_changes(task_text: &str) -> bool {
    let text = task_text.to_ascii_lowercase();
    if contains_negative_change_instruction(
        &text,
        &["lockfile", "lock file", "pnpm-lock.yaml", "yarn.lock"],
    ) {
        return false;
    }
    [
        "add a dependency",
        "add dependency",
        "add dependencies",
        "upgrade a dependency",
        "upgrade dependency",
        "upgrade dependencies",
        "update the lockfile",
        "update lockfile",
        "update the lock file",
        "update lock file",
        "edit the lockfile",
        "edit lockfile",
        "edit the lock file",
        "pnpm install",
        "npm install",
        "yarn install",
        "bun install",
        "add package",
        "upgrade package",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn task_allows_workspace_config_changes(task_text: &str) -> bool {
    let text = task_text.to_ascii_lowercase();
    if contains_negative_change_instruction(
        &text,
        &[
            "workspace config",
            "package-manager configuration",
            "pnpm-workspace.yaml",
        ],
    ) {
        return false;
    }
    [
        "update workspace config",
        "update the workspace config",
        "update monorepo config",
        "update the monorepo config",
        "update package-manager configuration",
        "update the package-manager configuration",
        "update the pnpm-workspace",
        "update pnpm-workspace",
        "edit pnpm-workspace",
        "update turbo config",
        "update nx config",
        "update lerna config",
        "update rush config",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn contains_negative_change_instruction(text: &str, protected_markers: &[&str]) -> bool {
    ["do not", "don't", "must not", "never"]
        .iter()
        .filter_map(|negative| text.find(negative).map(|index| &text[index..]))
        .any(|suffix| {
            let window = suffix.chars().take(180).collect::<String>();
            protected_markers
                .iter()
                .any(|marker| window.contains(marker))
        })
}

fn is_live_secret_like_path(path: &str) -> bool {
    let lower = path.trim().to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(lower.as_str());
    name == ".env"
        || name.starts_with(".env.")
        || name == "id_rsa"
        || name == "id_ed25519"
        || name.ends_with(".pem")
        || lower == ".ssh"
        || lower.starts_with(".ssh/")
        || lower.contains("/.ssh/")
        || lower.contains("/secrets/")
        || lower.ends_with("/credentials")
}

fn redact_secret_like_path_for_report(path: &str) -> String {
    if is_live_secret_like_path(path) {
        "<secret-like-path>".to_string()
    } else {
        redact_secrets(path)
    }
}

fn inject_secret_path_fixture(worktree: &Path) -> Result<(), String> {
    let secret_dir = worktree.join(".ssh");
    fs::create_dir_all(&secret_dir)
        .map_err(|error| format!("failed to create secret-path fixture dir: {error}"))?;
    fs::write(
        secret_dir.join("id_rsa"),
        "RS5_SECRET_PATH_FIXTURE_PLACEHOLDER\n",
    )
    .map_err(|error| format!("failed to inject secret-path fixture: {error}"))
}

fn force_validation_failure_for_repair_stress(
    mut validation: HepaValidationSummary,
) -> HepaValidationSummary {
    for command in &mut validation.commands {
        if command.exit_code == 0 {
            command.exit_code = 1;
            break;
        }
    }
    if validation.commands.is_empty() {
        validation.commands.push(HepaValidationCommandResult {
            command: "git diff --check".to_string(),
            exit_code: 1,
            duration_ms: 0,
        });
    }
    validation.status = HepaValidationStatus::Failed;
    validation.failure_type = Some("rs4_controlled_validation_failure".to_string());
    validation.summary.push(
        "RS-4 controlled repair trigger forced the first validation result after the real command completed, so Ralph-V2 can prove a bounded failure-aware repair round."
            .to_string(),
    );
    validation
}

fn run_safe_validation_command(
    worktree: &Path,
    command: &str,
) -> Result<(i32, String, String), String> {
    let argv = safe_validation_argv(command)?;
    let timeout_ms = live_validation_timeout_ms();
    let started = Instant::now();
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(worktree)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    loop {
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            let output = child
                .wait_with_output()
                .map_err(|error| error.to_string())?;
            return Ok((
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stdout).to_string(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        if elapsed_ms >= timeout_ms {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .map_err(|error| error.to_string())?;
            let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if !stderr.is_empty() && !stderr.ends_with('\n') {
                stderr.push('\n');
            }
            stderr.push_str(&format!(
                "HEPA validation timeout: `{command}` exceeded {timeout_ms}ms and was terminated."
            ));
            return Ok((
                -1,
                String::from_utf8_lossy(&output.stdout).to_string(),
                stderr,
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn safe_validation_argv(command: &str) -> Result<Vec<String>, String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let argv = match parts.as_slice() {
        ["git", "diff", "--check"] => parts,
        ["yarn", "install", "--frozen-lockfile"] => parts,
        ["yarn", script] if matches!(*script, "test" | "test:e2e" | "build" | "lint") => parts,
        ["yarn", "test", files @ ..]
            if !files.is_empty() && files.iter().all(|file| is_safe_validation_token(file)) =>
        {
            parts
        }
        ["npx", "vitest", "run", file] if is_safe_validation_token(file) => parts,
        ["bun", "test", files @ ..]
            if !files.is_empty() && files.iter().all(|file| is_safe_validation_token(file)) =>
        {
            parts
        }
        ["bunx", "tsc", "--noEmit", "-p", config] if is_safe_validation_token(config) => parts,
        ["pnpm", "install", "--frozen-lockfile", "--offline"] => parts,
        ["pnpm", "format:check"] => parts,
        ["pnpm", "--filter", package, script]
            if is_safe_package_filter(package)
                && matches!(*script, "test" | "lint" | "typecheck" | "build") =>
        {
            parts
        }
        ["pnpm", "--filter", package, script, "--", args @ ..]
            if is_safe_package_filter(package)
                && matches!(*script, "test" | "lint" | "typecheck" | "build")
                && !args.is_empty()
                && args.iter().all(|arg| is_safe_validation_token(arg)) =>
        {
            parts
        }
        _ => return Err(format!("unsupported live validation command: {command}")),
    };
    Ok(argv.into_iter().map(str::to_string).collect())
}

fn is_safe_package_filter(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '@' | '/' | '-' | '_' | '.'))
}

fn is_safe_validation_token(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.'))
}

fn live_validation_timeout_ms() -> u64 {
    const DEFAULT_LIVE_VALIDATION_TIMEOUT_MS: u64 = 300_000;
    const MAX_LIVE_VALIDATION_TIMEOUT_MS: u64 = 900_000;
    live_budget_ms(
        "HEPA_LIVE_VALIDATION_TIMEOUT_MS",
        DEFAULT_LIVE_VALIDATION_TIMEOUT_MS,
        MAX_LIVE_VALIDATION_TIMEOUT_MS,
    )
}

#[derive(Debug, Clone)]
struct LiveReviewOutcome {
    signals: Vec<HepaReviewSignal>,
    arbitration: HepaArbitrationSummary,
    staging_allowed: bool,
    blockers: Vec<String>,
    reviewer_passes: u32,
}

struct LiveReviewInput<'a> {
    config: &'a HepaFakeRunConfig,
    adapter_id: &'a str,
    spec: &'a hepa_adapters::spec::HepaAdapterSpec,
    environment: &'a std::collections::BTreeMap<String, String>,
    lane_paths: &'a hepa_core::artifacts::HepaLaneArtifactPaths,
    allocation: &'a HepaWorktreeAllocation,
    changed_files: &'a [String],
    validation: &'a HepaValidationSummary,
    diff_context: &'a str,
}

fn live_review_fanout(input: LiveReviewInput<'_>) -> Result<LiveReviewOutcome, String> {
    if let Some(artifact) = live_review_artifact_from_runtime(&input)? {
        write_json(
            &input
                .lane_paths
                .lane_dir
                .join("review/hermes-review-artifact.json"),
            &artifact,
        )
        .map_err(|error| error.to_string())?;
        return live_review_outcome_from_signals(
            input.config,
            input.lane_paths,
            artifact.signals,
            "Hermes reviewer runtime policy",
        );
    }
    if let Some(artifact) = live_review_artifact(input.config)? {
        write_json(
            &input
                .lane_paths
                .lane_dir
                .join("review/hermes-review-artifact.json"),
            &artifact,
        )
        .map_err(|error| error.to_string())?;
        return live_review_outcome_from_signals(
            input.config,
            input.lane_paths,
            artifact.signals,
            "Hermes review policy",
        );
    }
    if hermes_required() {
        return Err(
            "Hermes-present mode requires HEPA_HERMES_REVIEWER_COMMAND or HEPA_HERMES_REVIEW_ARTIFACT_FILE"
                .to_string(),
        );
    }
    if live_adapter_review_enabled() {
        return live_adapter_review(input);
    }
    live_deterministic_review_fanout(
        input.config,
        input.lane_paths,
        input.adapter_id,
        input.changed_files,
        input.validation,
        input.diff_context,
    )
}

fn live_review_artifact_from_runtime(
    input: &LiveReviewInput<'_>,
) -> Result<Option<HepaHermesReviewArtifact>, String> {
    let Ok(command) = std::env::var("HEPA_HERMES_REVIEWER_COMMAND") else {
        return Ok(None);
    };
    live_review_artifact_from_runtime_command(&command, input).map(Some)
}

fn live_review_artifact_from_runtime_command(
    command: &str,
    input: &LiveReviewInput<'_>,
) -> Result<HepaHermesReviewArtifact, String> {
    let review_dir = input.lane_paths.lane_dir.join("review");
    fs::create_dir_all(&review_dir).map_err(|error| error.to_string())?;
    let context_path = review_dir.join("hermes-review-context.json");
    let output_path = review_dir.join("hermes-review-artifact.runtime.json");
    let stdout_path = review_dir.join("hermes-reviewer-runtime.stdout.log");
    let stderr_path = review_dir.join("hermes-reviewer-runtime.stderr.log");
    let context = serde_json::json!({
        "schema_version": CONTRACT_SCHEMA_VERSION,
        "profile_id": "hepa-reviewer",
        "task_id": input.config.task_id,
        "lane_id": input.config.lane_id,
        "task_text": sanitized_task_text(input.config),
        "changed_files": input.changed_files,
        "validation": input.validation,
        "diff_context": input.diff_context,
        "artifact_output": output_path,
    });
    write_json(&context_path, &context).map_err(|error| error.to_string())?;
    run_hermes_profile_runtime_command(HermesProfileRuntimeCommand {
        command,
        profile_id: "hepa-reviewer",
        context_path: &context_path,
        output_path: &output_path,
        stdout_path: &stdout_path,
        stderr_path: &stderr_path,
    })?;
    let artifact = hermes_review_artifact_from_file(&output_path)?;
    if artifact.task_id != input.config.task_id {
        return Err(format!(
            "Hermes reviewer runtime artifact task_id mismatch: expected {} got {}",
            input.config.task_id, artifact.task_id
        ));
    }
    if artifact.lane_id != input.config.lane_id {
        return Err(format!(
            "Hermes reviewer runtime artifact lane_id mismatch: expected {} got {}",
            input.config.lane_id, artifact.lane_id
        ));
    }
    Ok(artifact)
}

fn live_review_artifact(
    config: &HepaFakeRunConfig,
) -> Result<Option<HepaHermesReviewArtifact>, String> {
    let default_artifact_path = default_hermes_review_artifact_path(config);
    if default_artifact_path.exists() {
        let artifact = hermes_review_artifact_from_file(&default_artifact_path)?;
        if artifact.task_id != config.task_id {
            return Err(format!(
                "Hermes review artifact task_id mismatch: expected {} got {}",
                config.task_id, artifact.task_id
            ));
        }
        if artifact.lane_id != config.lane_id {
            return Err(format!(
                "Hermes review artifact lane_id mismatch: expected {} got {}",
                config.lane_id, artifact.lane_id
            ));
        }
        return Ok(Some(artifact));
    }
    let Ok(artifact_path) = std::env::var("HEPA_HERMES_REVIEW_ARTIFACT_FILE") else {
        return Ok(None);
    };
    let artifact = hermes_review_artifact_from_file(Path::new(&artifact_path))?;
    if artifact.task_id != config.task_id {
        return Err(format!(
            "Hermes review artifact task_id mismatch: expected {} got {}",
            config.task_id, artifact.task_id
        ));
    }
    if artifact.lane_id != config.lane_id {
        return Err(format!(
            "Hermes review artifact lane_id mismatch: expected {} got {}",
            config.lane_id, artifact.lane_id
        ));
    }
    Ok(Some(artifact))
}

fn default_hermes_review_artifact_path(config: &HepaFakeRunConfig) -> PathBuf {
    config
        .control_root
        .join("hermes-review")
        .join(&config.run_id)
        .join(&config.lane_id)
        .join("hermes-review-artifact.runtime.json")
}

fn hermes_review_artifact_from_file(
    artifact_path: &Path,
) -> Result<HepaHermesReviewArtifact, String> {
    let raw = fs::read_to_string(artifact_path)
        .map_err(|error| format!("Hermes review artifact could not be read: {error}"))?;
    let artifact: HepaHermesReviewArtifact = serde_json::from_str(&raw)
        .map_err(|error| format!("Hermes review artifact JSON is invalid: {error}"))?;
    artifact
        .validate()
        .map_err(|error| format!("Hermes review artifact failed validation: {error}"))?;
    Ok(artifact)
}

fn live_review_manager_artifact(
    config: &HepaFakeRunConfig,
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    signals: &[HepaReviewSignal],
) -> Result<Option<HepaHermesReviewManagerArtifact>, String> {
    if let Some(artifact) = live_review_manager_artifact_from_runtime(config, lane_paths, signals)?
    {
        return Ok(Some(artifact));
    }
    let Ok(artifact_path) = std::env::var("HEPA_HERMES_REVIEW_MANAGER_ARTIFACT_FILE") else {
        return Ok(None);
    };
    let artifact = hermes_review_manager_artifact_from_file(Path::new(&artifact_path))?;
    if artifact.task_id != config.task_id {
        return Err(format!(
            "Hermes review-manager artifact task_id mismatch: expected {} got {}",
            config.task_id, artifact.task_id
        ));
    }
    if artifact.lane_id != config.lane_id {
        return Err(format!(
            "Hermes review-manager artifact lane_id mismatch: expected {} got {}",
            config.lane_id, artifact.lane_id
        ));
    }
    Ok(Some(artifact))
}

fn live_review_manager_artifact_from_runtime(
    config: &HepaFakeRunConfig,
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    signals: &[HepaReviewSignal],
) -> Result<Option<HepaHermesReviewManagerArtifact>, String> {
    let Ok(command) = std::env::var("HEPA_HERMES_REVIEW_MANAGER_COMMAND") else {
        return Ok(None);
    };
    live_review_manager_artifact_from_runtime_command(&command, config, lane_paths, signals)
        .map(Some)
}

fn live_review_manager_artifact_from_runtime_command(
    command: &str,
    config: &HepaFakeRunConfig,
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    signals: &[HepaReviewSignal],
) -> Result<HepaHermesReviewManagerArtifact, String> {
    let review_dir = lane_paths.lane_dir.join("review");
    fs::create_dir_all(&review_dir).map_err(|error| error.to_string())?;
    let context_path = review_dir.join("hermes-review-manager-context.json");
    let output_path = review_dir.join("hermes-review-manager-artifact.runtime.json");
    let stdout_path = review_dir.join("hermes-review-manager-runtime.stdout.log");
    let stderr_path = review_dir.join("hermes-review-manager-runtime.stderr.log");
    let context = serde_json::json!({
        "schema_version": CONTRACT_SCHEMA_VERSION,
        "profile_id": "hepa-review-manager",
        "task_id": config.task_id,
        "lane_id": config.lane_id,
        "signals": signals,
        "artifact_output": output_path,
    });
    write_json(&context_path, &context).map_err(|error| error.to_string())?;
    run_hermes_profile_runtime_command(HermesProfileRuntimeCommand {
        command,
        profile_id: "hepa-review-manager",
        context_path: &context_path,
        output_path: &output_path,
        stdout_path: &stdout_path,
        stderr_path: &stderr_path,
    })?;
    let artifact = hermes_review_manager_artifact_from_file(&output_path)?;
    if artifact.task_id != config.task_id {
        return Err(format!(
            "Hermes review-manager runtime artifact task_id mismatch: expected {} got {}",
            config.task_id, artifact.task_id
        ));
    }
    if artifact.lane_id != config.lane_id {
        return Err(format!(
            "Hermes review-manager runtime artifact lane_id mismatch: expected {} got {}",
            config.lane_id, artifact.lane_id
        ));
    }
    Ok(artifact)
}

struct HermesProfileRuntimeCommand<'a> {
    command: &'a str,
    profile_id: &'a str,
    context_path: &'a Path,
    output_path: &'a Path,
    stdout_path: &'a Path,
    stderr_path: &'a Path,
}

fn run_hermes_profile_runtime_command(
    input: HermesProfileRuntimeCommand<'_>,
) -> Result<(), String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(input.command)
        .env("HEPA_HERMES_PROFILE_ID", input.profile_id)
        .env("HEPA_HERMES_CONTEXT_FILE", input.context_path)
        .env("HEPA_HERMES_ARTIFACT_OUT", input.output_path)
        .output()
        .map_err(|error| {
            format!(
                "Hermes {} runtime could not start: {error}",
                input.profile_id
            )
        })?;
    fs::write(
        input.stdout_path,
        redact_secrets(&String::from_utf8_lossy(&output.stdout)),
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        input.stderr_path,
        redact_secrets(&String::from_utf8_lossy(&output.stderr)),
    )
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Hermes {} runtime exited {}",
            input.profile_id,
            output.status.code().unwrap_or(-1)
        ));
    }
    if !input.output_path.exists() {
        return Err(format!(
            "Hermes {} runtime did not write {}",
            input.profile_id,
            input.output_path.display()
        ));
    }
    Ok(())
}

fn hermes_review_manager_artifact_from_file(
    artifact_path: &Path,
) -> Result<HepaHermesReviewManagerArtifact, String> {
    let raw = fs::read_to_string(artifact_path)
        .map_err(|error| format!("Hermes review-manager artifact could not be read: {error}"))?;
    let artifact: HepaHermesReviewManagerArtifact = serde_json::from_str(&raw)
        .map_err(|error| format!("Hermes review-manager artifact JSON is invalid: {error}"))?;
    artifact
        .validate()
        .map_err(|error| format!("Hermes review-manager artifact failed validation: {error}"))?;
    Ok(artifact)
}

fn live_review_outcome_from_signals(
    config: &HepaFakeRunConfig,
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    signals: Vec<HepaReviewSignal>,
    pass_policy_label: &str,
) -> Result<LiveReviewOutcome, String> {
    let requires_review_manager = hermes_required() && review_signals_need_manager(&signals);
    let manager_artifact = live_review_manager_artifact(config, lane_paths, &signals)?;
    if let Some(artifact) = &manager_artifact {
        write_json(
            &lane_paths
                .lane_dir
                .join("review/hermes-review-manager-artifact.json"),
            artifact,
        )
        .map_err(|error| error.to_string())?;
    } else if requires_review_manager {
        return Err(
            "Hermes-present mode requires HEPA_HERMES_REVIEW_MANAGER_COMMAND or HEPA_HERMES_REVIEW_MANAGER_ARTIFACT_FILE when reviewer findings need manager arbitration"
                .to_string(),
        );
    }
    live_review_outcome_from_signals_and_manager(
        signals,
        manager_artifact.map(|artifact| artifact.arbitration),
        pass_policy_label,
    )
}

fn review_signals_need_manager(signals: &[HepaReviewSignal]) -> bool {
    signals
        .iter()
        .any(|signal| signal.status != HepaReviewStatus::Approved || !signal.findings.is_empty())
}

fn hermes_required() -> bool {
    env_flag("HEPA_HERMES_REQUIRED")
}

fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .as_deref()
        .map(is_truthy_env_value)
        .unwrap_or(false)
}

fn is_truthy_env_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "required"
    )
}

fn live_review_outcome_from_signals_and_manager(
    signals: Vec<HepaReviewSignal>,
    manager_arbitration: Option<HepaArbitrationSummary>,
    pass_policy_label: &str,
) -> Result<LiveReviewOutcome, String> {
    let policy = HepaReviewFanout {
        adapters: signals
            .iter()
            .map(|signal| signal.adapter_id.clone())
            .collect(),
        pass_policy: HepaReviewPassPolicy::All,
    };
    let pass_decision =
        apply_review_pass_policy(&policy, &signals).map_err(|error| error.to_string())?;
    let (arbitration, mut blockers) = if let Some(arbitration) = manager_arbitration {
        let blockers = blockers_from_arbitration_summary(&arbitration);
        (arbitration, blockers)
    } else {
        let findings = aggregate_review_findings(&signals).map_err(|error| error.to_string())?;
        let mut decisions = Vec::new();
        for finding in findings {
            decisions.push(arbitrate_live_finding(finding.finding)?);
        }
        let arbitration =
            summarize_arbitration_results(&decisions).map_err(format_arbitration_error)?;
        let staging_gate =
            evaluate_staging_after_arbitration(&decisions).map_err(format_arbitration_error)?;
        (arbitration, staging_gate.blockers)
    };
    if !pass_decision.passed {
        blockers.push(format!(
            "{pass_policy_label} required {} approval but received {}",
            pass_decision.required_approvals, pass_decision.approvals
        ));
    }
    blockers.sort();
    Ok(LiveReviewOutcome {
        reviewer_passes: signals.len() as u32,
        signals,
        arbitration,
        staging_allowed: blockers.is_empty(),
        blockers,
    })
}

fn blockers_from_arbitration_summary(arbitration: &HepaArbitrationSummary) -> Vec<String> {
    let mut blockers = Vec::new();
    for record in &arbitration.records {
        if record.disposition == "manager_required" {
            blockers.push(format!(
                "{}: review-manager arbitration is still required",
                record.finding_id
            ));
        }
        if record.accepted
            && matches!(
                record.severity_after,
                HepaFindingSeverity::High | HepaFindingSeverity::Critical
            )
        {
            blockers.push(format!(
                "{}: accepted high-risk release finding blocks staging",
                record.finding_id
            ));
        }
    }
    blockers.sort();
    blockers
}

fn live_adapter_review_enabled() -> bool {
    matches!(
        std::env::var("HEPA_LIVE_REVIEW_MODE").ok().as_deref(),
        Some("adapter" | "ADAPTER" | "live-adapter" | "LIVE_ADAPTER")
    )
}

fn live_adapter_review(input: LiveReviewInput<'_>) -> Result<LiveReviewOutcome, String> {
    if input.adapter_id == "pi" {
        return Err(
            "Pi is implementation-only in the Hermes-led workflow; review must use Hermes reviewer profiles"
                .to_string(),
        );
    }
    let review_id = "review-live-adapter";
    let mut prompt = live_review_prompt(
        input.config,
        input.changed_files,
        input.validation,
        input.diff_context,
    );
    if input.adapter_id == "pi" && pi_review_model_needs_no_think_suffix(input.environment) {
        prompt.push_str(
            "\nAdapter-local reviewer note: answer directly with only the requested JSON object and do not emit hidden reasoning. /no_think\n",
        );
    }
    let review_output = input
        .lane_paths
        .lane_dir
        .join("review/live-adapter-output.jsonl");
    let prompt_path = input
        .lane_paths
        .lane_dir
        .join("review/live-adapter-prompt.md");
    if let Some(parent) = prompt_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&prompt_path, &prompt).map_err(|error| error.to_string())?;
    let invocation = HepaOneshotAdapterInvocation {
        spec: input.spec.clone(),
        role: HepaAdapterRole::Reviewer,
        context: HepaAdapterTemplateContext {
            prompt_file: input
                .lane_paths
                .lane_dir
                .join("prompt.md")
                .display()
                .to_string(),
            worktree: input.allocation.worktree_path.display().to_string(),
            review_prompt_file: prompt_path.display().to_string(),
            output_file: input
                .lane_paths
                .lane_dir
                .join("attempts/attempt-1/attempt.json")
                .display()
                .to_string(),
            review_output_file: review_output.display().to_string(),
            artifact_dir: input.lane_paths.lane_dir.display().to_string(),
        },
        prompt,
        environment: input.environment.clone(),
        monitor_policy: live_monitor_policy(),
    };
    let _local_generation_permit = local_pi_generation_concurrency_permit(
        input.adapter_id,
        input.environment,
        HepaAdapterRole::Reviewer,
    )?;
    let result = HepaOneshotAdapterExecutor::new()
        .run(&invocation)
        .map_err(|error| error.to_string())?;
    let raw_review = adapter_review_payload(input.adapter_id, &result.stdout, &review_output)?;
    let normalization = normalize_reviewer_output_by_exception(
        HepaReviewerOutputInput {
            review_id: review_id.to_string(),
            lane_id: input.config.lane_id.clone(),
            adapter_id: format!("live-reviewer:{}", input.adapter_id),
            completed_at: "2026-06-16T00:00:06Z".to_string(),
            raw_output: raw_review,
        },
        |input, error| {
            Ok(HepaReviewSignal {
                schema_version: CONTRACT_SCHEMA_VERSION,
                review_id: input.review_id,
                lane_id: input.lane_id,
                adapter_id: input.adapter_id,
                status: HepaReviewStatus::Failed,
                findings: Vec::new(),
                summary: vec![format!(
                    "Live adapter reviewer output did not normalize: {}: {}",
                    error.field, error.message
                )],
                completed_at: input.completed_at,
            })
        },
    )
    .map_err(|error| error.to_string())?;
    live_review_outcome_from_signals(
        input.config,
        input.lane_paths,
        vec![normalization.signal],
        "live adapter reviewer",
    )
}

fn live_review_prompt(
    config: &HepaFakeRunConfig,
    changed_files: &[String],
    validation: &HepaValidationSummary,
    diff_context: &str,
) -> String {
    format!(
        "You are HEPA's live reviewer.\n\nTask:\n{}\n\nChanged files:\n{}\n\nValidation status: {:?}\n\nDiff context:\n{}\n\nReturn only a JSON object with this schema: {{\"status\":\"approved|changes_requested|blocked|failed\",\"summary\":[\"...\"],\"findings\":[{{\"severity\":\"low|medium|high|critical\",\"category\":\"...\",\"evidence\":\"...\",\"in_scope\":true,\"release_risk\":false,\"recommended_action\":\"...\",\"file_ref\":null,\"line\":null,\"message\":\"...\",\"accepted\":true}}]}}.\nIf there are no blocking issues, use status approved and an empty findings array. Do not create commits, branches, tags, pull requests, or Git remotes.",
        sanitized_task_text(config),
        if changed_files.is_empty() {
            "<none>".to_string()
        } else {
            changed_files.join(", ")
        },
        validation.status,
        diff_context
    )
}

fn pi_review_model_needs_no_think_suffix(
    environment: &std::collections::BTreeMap<String, String>,
) -> bool {
    let review_model = environment
        .get("HEPA_PI_REVIEW_MODEL")
        .or_else(|| environment.get("HEPA_PI_MODEL"))
        .cloned()
        .unwrap_or_default();
    let base_url = environment.get("HEPA_PI_BASE_URL").cloned();
    pi_model_needs_no_think_suffix(&review_model, &base_url)
}

fn pi_worker_model_needs_local_permit(
    environment: &std::collections::BTreeMap<String, String>,
) -> bool {
    let worker_model = environment
        .get("HEPA_PI_MODEL")
        .cloned()
        .unwrap_or_default();
    let base_url = environment.get("HEPA_PI_BASE_URL").cloned();
    pi_model_is_local(&worker_model, &base_url)
}

fn pi_local_worker_route_preflight_block_reason(
    adapter_id: &str,
    environment: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    if adapter_id != "pi" || !pi_worker_model_needs_local_permit(environment) {
        return None;
    }
    let diagnostic = pi_local_route_diagnostic(environment);
    if !diagnostic.is_blocking() {
        return None;
    }
    Some(format!(
        "{}; action={}",
        diagnostic.detail, diagnostic.action
    ))
}

fn local_pi_generation_concurrency_permit(
    adapter_id: &str,
    environment: &std::collections::BTreeMap<String, String>,
    role: HepaAdapterRole,
) -> Result<Option<MutexGuard<'static, ()>>, String> {
    if adapter_id != "pi" {
        return Ok(None);
    }
    let needs_permit = match role {
        HepaAdapterRole::Worker => pi_worker_model_needs_local_permit(environment),
        HepaAdapterRole::Reviewer => pi_review_model_needs_no_think_suffix(environment),
    };
    if !needs_permit {
        return Ok(None);
    }
    static LOCAL_PI_GENERATION_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    acquire_local_pi_generation_permit(
        LOCAL_PI_GENERATION_MUTEX.get_or_init(|| Mutex::new(())),
        live_local_generation_mutex_wait_ms(),
    )
    .map(Some)
}

fn live_local_generation_mutex_wait_ms() -> u64 {
    const DEFAULT_PI_LOCAL_MUTEX_WAIT_MS: u64 = 60_000;
    const MAX_PI_LOCAL_MUTEX_WAIT_MS: u64 = 600_000;
    live_budget_ms(
        "HEPA_PI_LOCAL_MUTEX_WAIT_MS",
        DEFAULT_PI_LOCAL_MUTEX_WAIT_MS,
        MAX_PI_LOCAL_MUTEX_WAIT_MS,
    )
}

fn acquire_local_pi_generation_permit(
    mutex: &'static Mutex<()>,
    wait_ms: u64,
) -> Result<MutexGuard<'static, ()>, String> {
    let started = Instant::now();
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => {
                let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                if elapsed_ms >= wait_ms {
                    return Err(format!(
                        "local_provider_concurrency_wait_timeout: waited {elapsed_ms}ms for local Pi generation permit"
                    ));
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn adapter_review_payload(
    adapter_id: &str,
    stdout: &str,
    output_file: &Path,
) -> Result<String, String> {
    let raw = if stdout.trim().is_empty() {
        fs::read_to_string(output_file).unwrap_or_default()
    } else {
        stdout.to_string()
    };
    if adapter_id == "pi" {
        let parsed = parse_pi_json_events(&raw).map_err(|error| error.to_string())?;
        return extract_json_object(&parsed.final_message)
            .ok_or_else(|| "Pi reviewer final message did not contain a JSON object".to_string());
    }
    Ok(extract_json_object(&raw).unwrap_or(raw))
}

fn extract_json_object(raw: &str) -> Option<String> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    (end >= start).then(|| raw[start..=end].to_string())
}

fn live_deterministic_review_fanout(
    config: &HepaFakeRunConfig,
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    adapter_id: &str,
    changed_files: &[String],
    validation: &HepaValidationSummary,
    diff_context: &str,
) -> Result<LiveReviewOutcome, String> {
    live_review_fanout_with_controlled_block(
        config,
        adapter_id,
        changed_files,
        validation,
        diff_context,
        lane_paths,
        live_force_review_block(),
    )
}

fn live_review_fanout_with_controlled_block(
    config: &HepaFakeRunConfig,
    adapter_id: &str,
    changed_files: &[String],
    validation: &HepaValidationSummary,
    diff_context: &str,
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    force_review_block: bool,
) -> Result<LiveReviewOutcome, String> {
    let primary_adapter = format!("hepa-reviewer:fallback-primary:{adapter_id}");
    let policy_adapter = format!("hepa-reviewer:fallback-policy:{adapter_id}");
    let reviewers = vec![
        configured_live_reviewer(
            primary_adapter.clone(),
            config.task_id.clone(),
            changed_files.to_vec(),
            force_review_block,
        ),
        configured_live_reviewer(
            policy_adapter.clone(),
            config.task_id.clone(),
            changed_files.to_vec(),
            force_review_block,
        ),
    ];
    let result = run_configured_reviewers_concurrently(
        HepaReviewFanoutInput {
            lane_id: config.lane_id.clone(),
            diff_context: diff_context.to_string(),
            validation_summary: validation_summary_name(validation),
            max_diff_bytes: 32_768,
        },
        reviewers,
    )
    .map_err(|error| error.to_string())?;
    live_review_outcome_from_signals(config, lane_paths, result.signals, "review fanout policy")
}

fn configured_live_reviewer(
    reviewer_adapter_id: String,
    task_id: String,
    changed_files: Vec<String>,
    force_review_block: bool,
) -> HepaConfiguredReviewer {
    HepaConfiguredReviewer::new(reviewer_adapter_id.clone(), move |request| {
        let signal = if force_review_block {
            live_controlled_review_block_signal(
                &request.lane_id,
                &request.adapter_id,
                &task_id,
                &changed_files,
            )
        } else if changed_files.is_empty() {
            live_no_diff_review_signal(&request.lane_id, &request.adapter_id, &task_id)
        } else if request.adapter_id.contains("primary") {
            live_primary_review_signal(
                &request.lane_id,
                &request.adapter_id,
                &task_id,
                &changed_files,
                &request.diff_context,
            )
        } else {
            live_policy_review_signal(
                &request.lane_id,
                &request.adapter_id,
                &task_id,
                &changed_files,
                &request.validation_summary,
            )
        };
        Ok(signal)
    })
}

fn live_controlled_review_block_signal(
    lane_id: &str,
    adapter_id: &str,
    task_id: &str,
    changed_files: &[String],
) -> HepaReviewSignal {
    let route = if adapter_id.contains("primary") {
        "primary"
    } else {
        "policy"
    };
    HepaReviewSignal {
        schema_version: CONTRACT_SCHEMA_VERSION,
        review_id: format!("review-rs5-block-{route}"),
        lane_id: lane_id.to_string(),
        adapter_id: adapter_id.to_string(),
        status: HepaReviewStatus::Blocked,
        findings: vec![HepaReviewFinding {
            finding_id: format!("rs5-review-blocked-escalation-{route}"),
            severity: HepaFindingSeverity::High,
            category: "review-blocked-escalation".to_string(),
            evidence: format!(
                "Controlled RS-5.3 reviewer verdict blocked task {task_id} after validation; changed_files={}.",
                if changed_files.is_empty() {
                    "<none>".to_string()
                } else {
                    changed_files.join(", ")
                }
            ),
            in_scope: true,
            release_risk: true,
            recommended_action:
                "Human manager must inspect the reviewer finding, decide whether to repair or override, then re-run HEPA before staging."
                    .to_string(),
            file_ref: changed_files.first().cloned(),
            line: None,
            message: "Review-blocked escalation requires human manager guidance before staging."
                .to_string(),
            accepted: true,
        }],
        summary: vec![
            "Controlled RS-5.3 reviewer verdict blocked the lane before staging.".to_string(),
            "Escalation: inspect reviewer evidence, choose repair or documented override, and re-run."
                .to_string(),
        ],
        completed_at: "2026-06-16T00:00:05Z".to_string(),
    }
}

fn live_primary_review_signal(
    lane_id: &str,
    adapter_id: &str,
    task_id: &str,
    changed_files: &[String],
    diff_context: &str,
) -> HepaReviewSignal {
    HepaReviewSignal {
        schema_version: CONTRACT_SCHEMA_VERSION,
        review_id: "review-primary".to_string(),
        lane_id: lane_id.to_string(),
        adapter_id: adapter_id.to_string(),
        status: HepaReviewStatus::Approved,
        findings: vec![HepaReviewFinding {
            finding_id: "rs3-manager-accept-low".to_string(),
            severity: HepaFindingSeverity::Low,
            category: "live-review-fanout".to_string(),
            evidence: format!(
                "Diff touches {}; review context bytes={}.",
                changed_files.join(", "),
                diff_context.len()
            ),
            in_scope: true,
            release_risk: false,
            recommended_action:
                "Manager may accept this low-risk observation after validation passes.".to_string(),
            file_ref: changed_files.first().cloned(),
            line: None,
            message: format!("Primary reviewer found the task {task_id} diff reviewable."),
            accepted: true,
        }],
        summary: vec![
            "Primary deterministic reviewer normalized the live diff successfully.".to_string(),
        ],
        completed_at: "2026-06-16T00:00:05Z".to_string(),
    }
}

fn live_policy_review_signal(
    lane_id: &str,
    adapter_id: &str,
    task_id: &str,
    changed_files: &[String],
    validation_summary: &str,
) -> HepaReviewSignal {
    HepaReviewSignal {
        schema_version: CONTRACT_SCHEMA_VERSION,
        review_id: "review-policy".to_string(),
        lane_id: lane_id.to_string(),
        adapter_id: adapter_id.to_string(),
        status: HepaReviewStatus::Approved,
        findings: vec![
            HepaReviewFinding {
                finding_id: "rs3-manager-reject-advisory".to_string(),
                severity: HepaFindingSeverity::Medium,
                category: "live-review-policy".to_string(),
                evidence: format!("Validation summary was `{validation_summary}`."),
                in_scope: true,
                release_risk: false,
                recommended_action:
                    "Manager should reject this advisory if validation evidence is sufficient."
                        .to_string(),
                file_ref: changed_files.first().cloned(),
                line: None,
                message: format!("Policy reviewer advisory for task {task_id}."),
                accepted: true,
            },
            HepaReviewFinding {
                finding_id: "rs3-downgrade-out-of-scope".to_string(),
                severity: HepaFindingSeverity::High,
                category: "live-review-policy".to_string(),
                evidence: "Out-of-scope non-release-risk observation from fanout route."
                    .to_string(),
                in_scope: false,
                release_risk: false,
                recommended_action: "Deterministically downgrade and exclude from repair scope."
                    .to_string(),
                file_ref: None,
                line: None,
                message: "Policy reviewer emitted an out-of-scope advisory.".to_string(),
                accepted: true,
            },
        ],
        summary: vec![
            "Policy deterministic reviewer normalized live validation and diff evidence."
                .to_string(),
        ],
        completed_at: "2026-06-16T00:00:05Z".to_string(),
    }
}

fn live_no_diff_review_signal(lane_id: &str, adapter_id: &str, task_id: &str) -> HepaReviewSignal {
    HepaReviewSignal {
        schema_version: CONTRACT_SCHEMA_VERSION,
        review_id: "review-no-diff".to_string(),
        lane_id: lane_id.to_string(),
        adapter_id: adapter_id.to_string(),
        status: HepaReviewStatus::Blocked,
        findings: vec![HepaReviewFinding {
            finding_id: "rs3-no-diff-blocker".to_string(),
            severity: HepaFindingSeverity::High,
            category: "live-review-fanout".to_string(),
            evidence: "No changed files were produced by the live worker.".to_string(),
            in_scope: true,
            release_risk: true,
            recommended_action: "Re-run with a task that produces a reviewable diff.".to_string(),
            file_ref: None,
            line: None,
            message: format!("Task {task_id} produced no reviewable diff."),
            accepted: true,
        }],
        summary: vec!["Review fanout blocked because no real diff was available.".to_string()],
        completed_at: "2026-06-16T00:00:05Z".to_string(),
    }
}

fn arbitrate_live_finding(finding: HepaReviewFinding) -> Result<HepaArbitratedFinding, String> {
    let arbitrated =
        apply_deterministic_downgrade_rules(finding).map_err(format_arbitration_error)?;
    if arbitrated.rule_id.as_deref() == Some("out-of-scope-non-release-risk") {
        return Ok(arbitrated);
    }
    match arbitrated.finding.finding_id.as_str() {
        id if id.starts_with("rs5-review-blocked-escalation-") => Ok(arbitrated),
        "rs3-manager-accept-low" => apply_manager_arbitration(
            arbitrated,
            HepaManagerArbitrationAction::Accept,
            "Manager accepts this low-risk review observation after validation passed.",
        ),
        "rs3-manager-reject-advisory" => apply_manager_arbitration(
            arbitrated,
            HepaManagerArbitrationAction::Reject,
            "Manager rejects the advisory because validation and diff scope are sufficient.",
        ),
        _ => apply_manager_arbitration(
            arbitrated,
            HepaManagerArbitrationAction::Reject,
            "Manager rejects unrecognized non-blocking live review finding by default.",
        ),
    }
    .map_err(format_arbitration_error)
}

fn validation_summary_name(validation: &HepaValidationSummary) -> String {
    match validation.status {
        HepaValidationStatus::Passed => "passed",
        HepaValidationStatus::Failed => "failed",
        HepaValidationStatus::Skipped => "skipped",
        HepaValidationStatus::NoTestsDetected => "no_tests_detected",
    }
    .to_string()
}

fn repair_findings_from_review_signals(signals: &[HepaReviewSignal]) -> Vec<HepaReviewFinding> {
    signals
        .iter()
        .flat_map(|signal| signal.findings.iter())
        .filter(|finding| finding.accepted)
        .cloned()
        .collect()
}

fn format_arbitration_error(error: hepa_review::arbitration::HepaArbitrationError) -> String {
    format!("{}: {}", error.field, error.message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LivePipelinePhase {
    WorkerBlocked,
    SafetyBlocked,
    ValidationFailed,
    ReviewFailed,
    PrFailed,
    PrCreated,
    Completed,
}

struct LiveTimingInput<'a> {
    config: &'a HepaFakeRunConfig,
    adapter_id: &'a str,
    worker_duration_seconds: f64,
    validation_duration_seconds: f64,
    review_duration_seconds: f64,
    reviewer_passes: u32,
    terminal_phase: LivePipelinePhase,
    repair_timing: Option<LiveRepairTiming>,
}

fn live_timing_record(input: LiveTimingInput<'_>) -> HepaTimingRecord {
    let LiveTimingInput {
        config,
        adapter_id,
        worker_duration_seconds,
        validation_duration_seconds,
        review_duration_seconds,
        reviewer_passes,
        terminal_phase,
        repair_timing,
    } = input;
    let mut phases = vec![
        HepaTimingPhase {
            name: "live_worker".to_string(),
            status: if terminal_phase == LivePipelinePhase::WorkerBlocked {
                HepaPhaseStatus::Blocked
            } else {
                HepaPhaseStatus::Completed
            },
            duration_seconds: worker_duration_seconds,
            round: Some(1),
            role: Some(HepaAgentRole::Worker),
            adapter_id: Some(adapter_id.to_string()),
            routing_reason: Some("explicit live adapter".to_string()),
            sandbox_posture: Some(run_sandbox_posture()),
        },
        HepaTimingPhase {
            name: "live_validation".to_string(),
            status: if terminal_phase == LivePipelinePhase::WorkerBlocked {
                HepaPhaseStatus::Skipped
            } else if terminal_phase == LivePipelinePhase::ValidationFailed
                || repair_timing.is_some()
            {
                HepaPhaseStatus::Failed
            } else {
                HepaPhaseStatus::Completed
            },
            duration_seconds: validation_duration_seconds,
            round: Some(1),
            role: Some(HepaAgentRole::Manager),
            adapter_id: None,
            routing_reason: Some("manager-owned validation".to_string()),
            sandbox_posture: Some(run_sandbox_posture()),
        },
    ];
    if let Some(repair_timing) = repair_timing {
        phases.push(HepaTimingPhase {
            name: "live_repair_brief".to_string(),
            status: HepaPhaseStatus::Completed,
            duration_seconds: repair_timing.brief_duration_seconds,
            round: Some(2),
            role: Some(HepaAgentRole::Manager),
            adapter_id: Some("hepa-manager-ralph-v2".to_string()),
            routing_reason: Some(
                "failure-aware repair prompt rewritten from validation evidence".to_string(),
            ),
            sandbox_posture: Some(run_sandbox_posture()),
        });
        phases.push(HepaTimingPhase {
            name: "live_repair_worker".to_string(),
            status: HepaPhaseStatus::Completed,
            duration_seconds: repair_timing.worker_duration_seconds,
            round: Some(2),
            role: Some(HepaAgentRole::Worker),
            adapter_id: Some(adapter_id.to_string()),
            routing_reason: Some(
                "bounded repair attempt through the same adapter contract".to_string(),
            ),
            sandbox_posture: Some(run_sandbox_posture()),
        });
        phases.push(HepaTimingPhase {
            name: "live_repair_validation".to_string(),
            status: if repair_timing.completed {
                HepaPhaseStatus::Completed
            } else {
                HepaPhaseStatus::Failed
            },
            duration_seconds: repair_timing.validation_duration_seconds,
            round: Some(2),
            role: Some(HepaAgentRole::Manager),
            adapter_id: None,
            routing_reason: Some("manager-owned validation after repair".to_string()),
            sandbox_posture: Some(run_sandbox_posture()),
        });
    }
    if !matches!(
        terminal_phase,
        LivePipelinePhase::WorkerBlocked
            | LivePipelinePhase::ValidationFailed
            | LivePipelinePhase::SafetyBlocked
    ) {
        phases.push(HepaTimingPhase {
            name: "live_review_fanout".to_string(),
            status: if terminal_phase == LivePipelinePhase::ReviewFailed {
                HepaPhaseStatus::Blocked
            } else {
                HepaPhaseStatus::Completed
            },
            duration_seconds: review_duration_seconds,
            round: Some(1),
            role: Some(HepaAgentRole::Reviewer),
            adapter_id: Some(format!("hepa-reviewer:fallback-fanout:{adapter_id}")),
            routing_reason: Some("parallel Hermes reviewer fallback fanout".to_string()),
            sandbox_posture: Some(run_sandbox_posture()),
        });
        phases.push(HepaTimingPhase {
            name: "live_arbitration".to_string(),
            status: if terminal_phase == LivePipelinePhase::ReviewFailed {
                HepaPhaseStatus::Blocked
            } else {
                HepaPhaseStatus::Completed
            },
            duration_seconds: 0.0,
            round: Some(1),
            role: Some(HepaAgentRole::Manager),
            adapter_id: Some("hepa-manager-arbitration".to_string()),
            routing_reason: Some("deterministic arbitration with recorded reasoning".to_string()),
            sandbox_posture: Some(run_sandbox_posture()),
        });
    }
    if matches!(
        terminal_phase,
        LivePipelinePhase::PrFailed | LivePipelinePhase::PrCreated | LivePipelinePhase::Completed
    ) {
        phases.push(HepaTimingPhase {
            name: "live_staging_commit_pr".to_string(),
            status: if terminal_phase == LivePipelinePhase::PrFailed {
                HepaPhaseStatus::Blocked
            } else {
                HepaPhaseStatus::Completed
            },
            duration_seconds: 0.0,
            round: Some(1),
            role: Some(HepaAgentRole::Manager),
            adapter_id: None,
            routing_reason: Some("manager-owned git lifecycle".to_string()),
            sandbox_posture: Some(run_sandbox_posture()),
        });
    }
    HepaTimingRecord {
        schema_version: CONTRACT_SCHEMA_VERSION,
        run_id: config.run_id.clone(),
        phases,
        counters: HepaTimingCounters {
            agent_loops: if repair_timing.is_some() { 2 } else { 1 },
            manager_passes: if repair_timing.is_some() { 2 } else { 1 },
            worker_profile_llm_calls: 0,
            reviewer_passes,
            install_events: 0,
            container_count: 0,
        },
    }
}

fn live_terminal_report(
    config: &HepaFakeRunConfig,
    validation: HepaValidationSummary,
    review_signals: Vec<HepaReviewSignal>,
    timing: HepaTimingRecord,
    arbitration: HepaArbitrationSummary,
    pr_url: Option<String>,
    summary: Vec<String>,
) -> HepaTerminalTaskReport {
    HepaTerminalTaskReport {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: config.task_id.clone(),
        lane_id: config.lane_id.clone(),
        status: HepaTerminalStatus::Completed,
        pr_url,
        validation: Some(validation),
        review_signals,
        arbitration: Some(arbitration),
        timing: Some(timing),
        summary,
        human_attention_required: false,
        completed_at: "2026-06-16T00:00:07Z".to_string(),
    }
}

struct FinishBlockedInput<'a> {
    config: &'a HepaFakeRunConfig,
    task: HepaFleetTask,
    run_paths: &'a hepa_core::artifacts::HepaRunArtifactPaths,
    lane_paths: &'a hepa_core::artifacts::HepaLaneArtifactPaths,
    allocator: &'a HepaWorktreeAllocator,
    lane: &'a mut HepaLane,
    validation: HepaValidationSummary,
    review_signals: Vec<HepaReviewSignal>,
    arbitration: Option<HepaArbitrationSummary>,
    timing: HepaTimingRecord,
    reason: String,
}

fn finish_blocked_live_run(input: FinishBlockedInput<'_>) -> Result<HepaFakeRunResult, String> {
    let FinishBlockedInput {
        config,
        mut task,
        run_paths,
        lane_paths,
        allocator,
        lane,
        validation,
        review_signals,
        arbitration,
        timing,
        reason,
    } = input;
    write_json(&lane_paths.lane_state, lane).map_err(|error| error.to_string())?;
    let readiness = HepaReadinessResult {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: config.task_id.clone(),
        status: HepaReadinessStatus::Blocked,
        blockers: vec![reason.clone()],
        questions: Vec::new(),
        checked_at: "2026-06-16T00:00:04Z".to_string(),
    };
    task.status = HepaTaskStatus::Blocked;
    task.readiness = HepaReadinessState::Blocked;
    write_json(&run_paths.task_state, &task).map_err(|error| error.to_string())?;
    lane_paths
        .write_timing_record(&timing)
        .map_err(|error| error.to_string())?;
    let failure_pattern = reason.clone();
    let terminal_report = HepaTerminalTaskReport {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: config.task_id.clone(),
        lane_id: config.lane_id.clone(),
        status: HepaTerminalStatus::Blocked,
        pr_url: None,
        validation: Some(validation),
        review_signals,
        arbitration,
        timing: Some(timing.clone()),
        summary: vec![reason],
        human_attention_required: true,
        completed_at: "2026-06-16T00:00:04Z".to_string(),
    };
    write_json(&lane_paths.final_report, &terminal_report).map_err(|error| error.to_string())?;
    write_json(&run_paths.run_state, &readiness).map_err(|error| error.to_string())?;
    record_terminal_memory(TerminalMemoryInput {
        control_root: &config.control_root,
        project_id: &task.project_id,
        lane_id: &config.lane_id,
        lane_state: &HepaLaneState::Blocked,
        adapter_id: "live",
        prompt_pattern: &config.task_text,
        failure_pattern: Some(&failure_pattern),
        validation_pass: false,
        reviewer_pass: false,
        pr_readiness: false,
        repair_convergence: false,
    });
    run_paths
        .archive_on_exit("2026-06-16T00:00:08Z", HepaArchiveOutcome::Blocked)
        .map_err(|error| error.to_string())?;

    let cleanup = allocator
        .cleanup_lane(&config.lane_id, "2026-06-16T00:00:09Z")
        .map_err(|error| error.to_string())?;
    Ok(HepaFakeRunResult {
        run_id: config.run_id.clone(),
        lane_id: config.lane_id.clone(),
        status: "blocked".to_string(),
        timing,
        terminal_report,
        cleanup_performed: matches!(
            cleanup.status,
            hepa_git::worktree::HepaWorktreeCleanupStatus::Cleaned
        ),
    })
}

struct TerminalMemoryInput<'a> {
    control_root: &'a Path,
    project_id: &'a str,
    lane_id: &'a str,
    lane_state: &'a HepaLaneState,
    adapter_id: &'a str,
    prompt_pattern: &'a str,
    failure_pattern: Option<&'a str>,
    validation_pass: bool,
    reviewer_pass: bool,
    pr_readiness: bool,
    repair_convergence: bool,
}

fn record_terminal_memory(input: TerminalMemoryInput<'_>) {
    let Ok(memory) = project_memory(input.control_root, input.project_id) else {
        return;
    };
    let _ = memory.ensure_context_packs();
    let _ = memory.append_prompt_pattern(input.lane_state, input.prompt_pattern);
    if let Some(failure_pattern) = input.failure_pattern {
        let _ = memory.append_failure_pattern(input.lane_state, failure_pattern);
    }
    let _ = memory.append_adapter_lesson(
        input.lane_state,
        &format!(
            "adapter={} terminal_state={:?}",
            input.adapter_id, input.lane_state
        ),
    );
    let _ = memory.record_reward(&HepaRewardSignal {
        project_id: safe_memory_project_id(input.project_id),
        lane_id: safe_memory_project_id(input.lane_id),
        validation_pass: input.validation_pass,
        reviewer_pass: input.reviewer_pass,
        pr_readiness: input.pr_readiness,
        ci_pass: input.pr_readiness,
        human_merge: false,
        repair_convergence: input.repair_convergence,
        created_at: unix_timestamp_label(),
    });
}

fn memory_failure_context(control_root: &Path, project_id: &str) -> Vec<String> {
    project_memory(control_root, project_id)
        .map(|memory| memory.retry_brief_failure_context())
        .unwrap_or_default()
}

fn project_memory(control_root: &Path, project_id: &str) -> Result<HepaProjectMemory, String> {
    HepaProjectMemory::new(
        control_root.join("memory"),
        safe_memory_project_id(project_id),
    )
    .map_err(|error| error.to_string())
}

fn safe_memory_project_id(value: &str) -> String {
    let mut safe = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    while safe.contains("..") {
        safe = safe.replace("..", ".");
    }
    let safe = safe.trim_matches('.').trim_matches('_').to_string();
    if safe.is_empty() {
        "default-project".to_string()
    } else {
        safe
    }
}

fn unix_timestamp_label() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("t{seconds}")
}

fn sanitize_validation_output(worktree: &Path, text: &str) -> String {
    let mut sanitized = redact_secrets(text);
    for path in [
        Some(worktree),
        worktree.parent(),
        worktree.parent().and_then(Path::parent),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(path) = path.to_str() {
            sanitized = sanitized.replace(path, "<VALIDATION_RUNTIME>");
        }
    }
    if let Some(home) = std::env::var_os("HOME").and_then(|value| value.into_string().ok()) {
        sanitized = sanitized.replace(&home, "<HOME>");
    }
    let mut line = sanitized
        .lines()
        .take(4)
        .collect::<Vec<_>>()
        .join(" | ")
        .trim()
        .to_string();
    if line.len() > 500 {
        line.truncate(500);
        line.push_str("...");
    }
    line
}

fn live_attempt_summary(worktree: &Path, stdout: &str, stderr: &str) -> Vec<String> {
    vec![
        format!(
            "worker stdout: {}",
            sanitize_validation_output(worktree, stdout)
        ),
        format!(
            "worker stderr: {}",
            sanitize_validation_output(worktree, stderr)
        ),
    ]
}

fn commit_title(task_text: &str) -> String {
    let mut title = task_text
        .split('.')
        .next()
        .unwrap_or(task_text)
        .trim()
        .to_string();
    if title.len() > 72 {
        title.truncate(72);
    }
    if title.is_empty() {
        "live validation task".to_string()
    } else {
        title
    }
}

fn sanitized_task_text(config: &HepaFakeRunConfig) -> String {
    let mut text = redact_secrets(&config.task_text);
    for path in [
        config.repo_path.as_path(),
        config.control_root.as_path(),
        config.worktree_root.as_path(),
        config.archive_root.as_path(),
    ] {
        if let Some(path) = path.to_str() {
            text = text.replace(path, "<TARGET_REPO>");
        }
    }
    if let Some(home) = std::env::var_os("HOME").and_then(|value| value.into_string().ok()) {
        text = text.replace(&home, "<HOME>");
    }
    text
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
        steering_records: Vec::new(),
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

fn blocked_worker_validation_summary(reason: &str) -> HepaValidationSummary {
    HepaValidationSummary {
        schema_version: CONTRACT_SCHEMA_VERSION,
        status: HepaValidationStatus::Skipped,
        commands: Vec::new(),
        no_tests_detected: true,
        failure_type: Some("live_worker_blocked_before_validation".to_string()),
        summary: vec![format!(
            "Validation was not run because the live worker did not produce a usable terminal attempt: {reason}"
        )],
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

/// Resolve the active sandbox posture recorded in every run. The fake adapter
/// declares no native sandbox and a trusted project resolves to host-worktree.
fn run_sandbox_posture() -> String {
    use hepa_adapters::container::{HepaProjectTrust, resolve_sandbox_posture};
    use hepa_adapters::spec::HepaAdapterSandbox;
    resolve_sandbox_posture(HepaAdapterSandbox::None, HepaProjectTrust::Trusted)
        .as_str()
        .to_string()
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
                sandbox_posture: Some(run_sandbox_posture()),
            },
            HepaTimingPhase {
                name: "fake_review".to_string(),
                status: HepaPhaseStatus::Completed,
                duration_seconds: 1.0,
                round: Some(1),
                role: Some(HepaAgentRole::Reviewer),
                adapter_id: Some("fake".to_string()),
                routing_reason: Some("fake review fanout".to_string()),
                sandbox_posture: Some(run_sandbox_posture()),
            },
        ],
        counters: HepaTimingCounters {
            agent_loops: 1,
            manager_passes: 1,
            worker_profile_llm_calls: 0,
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
        arbitration: None,
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

fn collect_changed_files(worktree: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .map_err(|error| format!("failed to inspect worktree diff: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let mut changed = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let path = line
            .get(3..)
            .ok_or_else(|| format!("unexpected git status line: {line}"))?
            .trim()
            .to_string();
        if !path.is_empty() && path != ".hepa-worktree.json" && !path.starts_with(".hepa/") {
            changed.push(path);
        }
    }
    Ok(changed)
}

fn summarize_changed_files(changed_files: &[String]) -> String {
    if changed_files.is_empty() {
        return "none".to_string();
    }
    let shown: Vec<&str> = changed_files.iter().take(8).map(String::as_str).collect();
    if changed_files.len() > shown.len() {
        format!(
            "{} ({} more)",
            shown.join(", "),
            changed_files.len() - shown.len()
        )
    } else {
        shown.join(", ")
    }
}

fn collect_live_diff(worktree: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", "--"])
        .output()
        .map_err(|error| format!("failed to inspect worktree diff: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let diff = sanitize_validation_output(worktree, &String::from_utf8_lossy(&output.stdout));
    if !diff.trim().is_empty() {
        return Ok(diff);
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["status", "--short"])
        .output()
        .map_err(|error| format!("failed to inspect worktree status: {error}"))?;
    if !status.status.success() {
        return Err(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&status.stderr)
        ));
    }
    let status = sanitize_validation_output(worktree, &String::from_utf8_lossy(&status.stdout));
    if status.trim().is_empty() {
        Ok("No tracked diff or status changes were captured.".to_string())
    } else {
        Ok(format!("No tracked diff captured. Git status:\n{status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hepa_adapters::spec::{
        ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterMode,
        HepaAdapterPromptTransport, HepaAdapterSandbox, HepaAdapterSpec,
    };
    use hepa_kanban::{
        card_mapping::{
            HepaHermesCardMappingInput, HepaHermesCommentKind, HepaHermesFieldValue,
            map_task_to_hermes_card,
        },
        sync::HepaMemoryHermesCardStore,
    };
    use std::{
        os::unix::fs::PermissionsExt,
        process::Command,
        sync::{Mutex, MutexGuard, OnceLock},
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
        assert_eq!(result.timing.counters.worker_profile_llm_calls, 0);
        assert_eq!(result.timing.counters.container_count, 0);
        // The active sandbox posture is recorded in every run's timing phases.
        assert!(
            result
                .timing
                .phases
                .iter()
                .all(|phase| phase.sandbox_posture.as_deref() == Some("host-worktree"))
        );
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
    fn live_pi_budget_clamps_operator_env_to_release_target() {
        assert_eq!(clamp_live_budget_ms(None, 300_000, 600_000), 300_000);
        assert_eq!(
            clamp_live_budget_ms(Some("5400000"), 300_000, 600_000),
            600_000
        );
        assert_eq!(
            clamp_live_budget_ms(Some("120000"), 300_000, 600_000),
            120_000
        );
        assert_eq!(clamp_live_budget_ms(Some("0"), 300_000, 600_000), 1);
        assert_eq!(
            clamp_live_budget_ms(Some("not-a-number"), 300_000, 600_000),
            300_000
        );
    }

    #[test]
    fn local_pi_generation_permit_wait_is_bounded() {
        let mutex: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
        let held = mutex.lock().expect("hold local generation permit");

        let started = Instant::now();
        let error =
            acquire_local_pi_generation_permit(mutex, 25).expect_err("permit wait should time out");

        assert!(error.contains("local_provider_concurrency_wait_timeout"));
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(held);
        assert!(acquire_local_pi_generation_permit(mutex, 25).is_ok());
    }

    #[test]
    fn live_pi_empty_output_writes_blocked_attempt_artifacts() {
        let root = unique_test_dir("live-pi-empty-output");
        let repo = root.join("repo");
        init_repo(&repo);
        let config = HepaFakeRunConfig {
            repo_path: repo.clone(),
            control_root: root.join("control"),
            worktree_root: root.join("worktrees"),
            archive_root: root.join("archive"),
            run_id: "run-empty".to_string(),
            task_id: "task-empty".to_string(),
            lane_id: "lane-empty".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        };
        let layout =
            HepaArtifactLayout::new(&config.control_root, &config.archive_root).expect("layout");
        let lane_paths = layout
            .run(&config.run_id, &config.task_id)
            .expect("run paths")
            .lane(&config.lane_id)
            .expect("lane paths");
        fs::create_dir_all(&lane_paths.lane_dir).expect("lane dir");
        let allocator = HepaWorktreeAllocator::new(&config.repo_path, &config.worktree_root);
        let allocation = allocator
            .allocate_lane_with_metadata(&config.lane_id, "2026-06-16T00:00:00Z")
            .expect("allocation");
        let fake_pi = root.join("pi-empty");
        write_fake_pi_empty_output(&fake_pi);
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "pi".to_string(),
            display_name: "Pi Coding Agent".to_string(),
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: format!("{} --mode json", fake_pi.display()),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: vec![fake_pi.to_string_lossy().to_string()],
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::None,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["local-only".to_string()],
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 1,
            prompt_transport: HepaAdapterPromptTransport::Stdin,
            output_capture: HepaAdapterOutputCapture::Stdout,
        };

        let result = execute_live_worker_attempt(ExecuteLiveAttemptInput {
            config: &config,
            lane_paths: &lane_paths,
            allocation: &allocation,
            spec,
            adapter_id: "pi",
            environment: std::collections::BTreeMap::new(),
            attempt_id: "attempt-1",
            round: 1,
            prompt: "fixture prompt".to_string(),
            started_at: "2026-06-16T00:00:02Z",
            completed_at: "2026-06-16T00:00:03Z",
        });
        let error = match result {
            Ok(_) => panic!("empty Pi output must block"),
            Err(error) => error,
        };

        assert!(error.contains("local_provider_empty_or_malformed_response"));
        let attempt_dir = lane_paths
            .attempt("attempt-1")
            .expect("attempt paths")
            .attempt_dir;
        let attempt_json =
            fs::read_to_string(attempt_dir.join("attempt.json")).expect("attempt report");
        assert!(attempt_json.contains("\"status\": \"blocked\""));
        assert!(attempt_json.contains("local_provider_empty_or_malformed_response"));
        let stdout = fs::read_to_string(attempt_dir.join("stdout.log")).expect("stdout log");
        assert!(stdout.contains("\"type\":\"agent_end\""));
        assert!(attempt_dir.join("stderr.log").exists());

        allocator
            .cleanup_lane(&config.lane_id, "2026-06-16T00:00:09Z")
            .expect("cleanup");
        remove_test_dir(root);
    }

    #[test]
    fn live_pi_truncated_eof_with_changed_files_continues_to_validation_path() {
        let root = unique_test_dir("live-pi-truncated-with-changes");
        let repo = root.join("repo");
        init_repo(&repo);
        let config = HepaFakeRunConfig {
            repo_path: repo.clone(),
            control_root: root.join("control"),
            worktree_root: root.join("worktrees"),
            archive_root: root.join("archive"),
            run_id: "run-truncated".to_string(),
            task_id: "task-truncated".to_string(),
            lane_id: "lane-truncated".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        };
        let layout =
            HepaArtifactLayout::new(&config.control_root, &config.archive_root).expect("layout");
        let lane_paths = layout
            .run(&config.run_id, &config.task_id)
            .expect("run paths")
            .lane(&config.lane_id)
            .expect("lane paths");
        fs::create_dir_all(&lane_paths.lane_dir).expect("lane dir");
        let allocator = HepaWorktreeAllocator::new(&config.repo_path, &config.worktree_root);
        let allocation = allocator
            .allocate_lane_with_metadata(&config.lane_id, "2026-06-16T00:00:00Z")
            .expect("allocation");
        let fake_pi = root.join("pi-truncated");
        write_executable(
            &fake_pi,
            "#!/usr/bin/env sh\ncat >/dev/null\nprintf 'changed by pi\\n' >> README.md\nprintf '%s\\n' '{\"type\":\"agent_start\"}' '{\"type\":\"tool_call\",\"name\":\"edit\"}'\nprintf '%s' '{\"type\":\"message_update\",\"delta\":\"unterminated'\n",
        );
        let mut spec = dummy_pi_spec();
        spec.command = format!("{} --mode json", fake_pi.display());
        spec.required_commands = vec![fake_pi.to_string_lossy().to_string()];

        let result = execute_live_worker_attempt(ExecuteLiveAttemptInput {
            config: &config,
            lane_paths: &lane_paths,
            allocation: &allocation,
            spec,
            adapter_id: "pi",
            environment: std::collections::BTreeMap::new(),
            attempt_id: "attempt-1",
            round: 1,
            prompt: "fixture prompt".to_string(),
            started_at: "2026-06-16T00:00:02Z",
            completed_at: "2026-06-16T00:00:03Z",
        })
        .expect("changed files should continue after EOF-truncated Pi output");

        assert_eq!(result.changed_files, vec!["README.md".to_string()]);
        let attempt_json = fs::read_to_string(
            lane_paths
                .attempt("attempt-1")
                .expect("attempt paths")
                .attempt_dir
                .join("attempt.json"),
        )
        .expect("attempt report");
        assert!(attempt_json.contains("\"status\": \"completed\""));
        let stream = fs::read_to_string(
            lane_paths
                .lane_dir
                .join("streams/manager-tool-summary-stream.jsonl"),
        )
        .expect("stream event");
        assert!(stream.contains("pi_truncated_stream_continued"));

        allocator
            .cleanup_lane(&config.lane_id, "2026-06-16T00:00:09Z")
            .expect("cleanup");
        remove_test_dir(root);
    }

    #[test]
    fn fake_terminal_run_records_memory_signals() {
        let root = unique_test_dir("fake-memory");
        let repo = root.join("repo");
        init_repo(&repo);
        let config = HepaFakeRunConfig {
            repo_path: repo.clone(),
            control_root: root.join("control"),
            worktree_root: root.join("worktrees"),
            archive_root: root.join("archive"),
            run_id: "run-memory".to_string(),
            task_id: "task-memory".to_string(),
            lane_id: "lane-memory".to_string(),
            task_text: "Update docs without touching secrets".to_string(),
            timing: true,
        };

        run_fake_task(&config).expect("fake run should complete");
        let memory =
            project_memory(&config.control_root, "single-repo").expect("memory should load");

        let prompt_pack = memory
            .read_pack(hepa_memory::HepaContextPack::PromptPatterns)
            .expect("prompt pack exists");
        assert!(prompt_pack.contains("Update docs without touching secrets"));
        let adapter_pack = memory
            .read_pack(hepa_memory::HepaContextPack::AdapterLessons)
            .expect("adapter lessons exist");
        assert!(adapter_pack.contains("adapter=fake"));
        let rewards = memory.list_rewards();
        assert_eq!(rewards.len(), 1);
        assert!(rewards[0].validation_pass);
        assert!(rewards[0].reviewer_pass);
        assert!(rewards[0].pr_readiness);
        assert!(rewards[0].repair_convergence);

        remove_test_dir(root);
    }

    #[test]
    fn repair_context_consults_project_failure_memory() {
        let root = unique_test_dir("repair-memory");
        let memory = project_memory(&root.join("control"), "project/with path")
            .expect("sanitized project memory");
        memory
            .append_failure_pattern(&HepaLaneState::Failed, "rerun lint after generated docs")
            .expect("append failure memory");

        let context = memory_failure_context(&root.join("control"), "project/with path");

        assert_eq!(context, vec!["rerun lint after generated docs".to_string()]);
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

    #[test]
    fn live_worker_prompt_uses_requested_task_without_smoke_edit() {
        let prompt =
            live_worker_prompt("Add a focused reset-password form test and run yarn test.");

        assert!(prompt.contains("Add a focused reset-password form test"));
        assert!(prompt.contains("Repository worktree: current directory"));
        assert!(prompt.contains("HEPA owns the Git lifecycle"));
        assert!(prompt.contains("Run the smallest relevant validation command"));
        assert!(prompt.contains("Repository privacy rules"));
        assert!(prompt.contains("Do not write absolute local filesystem paths"));
        assert!(prompt.contains("neutral placeholder fixture"));
        assert!(!prompt.contains("/tmp/hepa-lane"));
        assert!(!prompt.contains("Added by Pi smoke test"));
        assert!(!prompt.contains("Make exactly one change"));
    }

    #[test]
    fn live_worker_prompt_adds_no_think_only_for_local_qwen_pi() {
        let config = hepa_core::config::HepaConfig::load(
            None,
            &std::collections::BTreeMap::new(),
            HepaConfigOverrides {
                pi_model: Some("local/mlx-community/Qwen3-30B-A3B-4bit".to_string()),
                pi_base_url: Some(Some("http://127.0.0.1:52415/v1".to_string())),
                ..HepaConfigOverrides::default()
            },
        )
        .expect("config should load");

        let prompt = live_worker_prompt_for_adapter("Update README.md", "pi", &config);

        assert!(prompt.contains("Update README.md"));
        assert!(prompt.contains("Pi tool path rules"));
        assert!(prompt.contains("read or edit `src/app/file.tsx`"));
        assert!(prompt.contains("Local-provider bounded task rules"));
        assert!(prompt.contains("edit the smallest existing file set"));
        assert!(prompt.contains("Do not run validation commands yourself"));
        assert!(prompt.contains("/no_think"));
    }

    #[test]
    fn live_worker_prompt_adds_bounded_rules_without_no_think_for_non_reasoning_local_pi() {
        let config = hepa_core::config::HepaConfig::load(
            None,
            &std::collections::BTreeMap::new(),
            HepaConfigOverrides {
                pi_model: Some("llama-cpp/devstral-small-24b".to_string()),
                pi_base_url: Some(Some("http://127.0.0.1:8080/v1".to_string())),
                ..HepaConfigOverrides::default()
            },
        )
        .expect("config should load");

        let prompt = live_worker_prompt_for_adapter("Update README.md", "pi", &config);

        assert!(prompt.contains("Pi tool path rules"));
        assert!(prompt.contains("Local-provider bounded task rules"));
        assert!(prompt.contains("targeted reads"));
        assert!(prompt.contains("Do not run validation commands yourself"));
        assert!(!prompt.contains("/no_think"));
    }

    #[test]
    fn live_worker_prompt_adds_no_think_for_local_gpt_oss_pi() {
        let config = hepa_core::config::HepaConfig::load(
            None,
            &std::collections::BTreeMap::new(),
            HepaConfigOverrides {
                pi_model: Some("llama-cpp/gpt-oss-20b".to_string()),
                pi_base_url: Some(Some("http://127.0.0.1:8080/v1".to_string())),
                ..HepaConfigOverrides::default()
            },
        )
        .expect("config should load");

        let prompt = live_worker_prompt_for_adapter("Update README.md", "pi", &config);

        assert!(prompt.contains("Local-provider bounded task rules"));
        assert!(prompt.contains("Do not include reasoning traces"));
        assert!(prompt.contains("/no_think"));
    }

    #[test]
    fn live_worker_prompt_does_not_add_local_rules_for_cloud_or_non_pi() {
        let cloud_config = hepa_core::config::HepaConfig::load(
            None,
            &std::collections::BTreeMap::new(),
            HepaConfigOverrides {
                pi_model: Some("deepseek/deepseek-chat".to_string()),
                ..HepaConfigOverrides::default()
            },
        )
        .expect("config should load");
        let local_config = hepa_core::config::HepaConfig::load(
            None,
            &std::collections::BTreeMap::new(),
            HepaConfigOverrides {
                pi_model: Some("local/mlx-community/Qwen3-30B-A3B-4bit".to_string()),
                pi_base_url: Some(Some("http://127.0.0.1:52415/v1".to_string())),
                ..HepaConfigOverrides::default()
            },
        )
        .expect("config should load");

        assert!(
            !live_worker_prompt_for_adapter("Update README.md", "pi", &cloud_config)
                .contains("/no_think")
        );
        assert!(
            live_worker_prompt_for_adapter("Update README.md", "pi", &cloud_config)
                .contains("Pi tool path rules")
        );
        assert!(
            !live_worker_prompt_for_adapter("Update README.md", "pi", &cloud_config)
                .contains("Local-provider bounded task rules")
        );
        assert!(
            !live_worker_prompt_for_adapter("Update README.md", "custom", &local_config)
                .contains("/no_think")
        );
        assert!(
            !live_worker_prompt_for_adapter("Update README.md", "custom", &local_config)
                .contains("Pi tool path rules")
        );
        assert!(
            !live_worker_prompt_for_adapter("Update README.md", "custom", &local_config)
                .contains("Local-provider bounded task rules")
        );
    }

    #[test]
    fn pi_local_detection_covers_llama_cpp_reasoning_models() {
        let base_url = Some("http://127.0.0.1:8080/v1".to_string());

        assert!(pi_model_is_local("llama-cpp/devstral-small-24b", &None));
        assert!(pi_model_is_local("deepseek/deepseek-chat", &base_url));
        assert!(pi_worker_model_needs_local_permit(
            &std::collections::BTreeMap::from([(
                "HEPA_PI_MODEL".to_string(),
                "llama-cpp/devstral-small-24b".to_string(),
            )])
        ));
        assert!(!pi_model_needs_no_think_suffix(
            "llama-cpp/devstral-small-24b",
            &base_url
        ));
        assert!(pi_model_needs_no_think_suffix(
            "llama-cpp/gpt-oss-20b",
            &base_url
        ));
        assert!(pi_model_needs_no_think_suffix(
            "local/mlx-community/Qwen3-30B-A3B-4bit",
            &base_url
        ));
    }

    #[test]
    fn pi_local_provider_context_overflow_is_classified_distinctly() {
        let reason = pi_local_provider_output_failure_reason(
            "agent_end missing final assistant message",
            r#"{"type":"agent_start"}"#,
            "request (16862 tokens) exceeds the available context size (16384)",
        );

        assert!(reason.contains("local_provider_context_window_exceeded"));
        assert!(!reason.contains("local_provider_empty_or_malformed_response"));
    }

    #[test]
    fn pi_local_provider_tool_protocol_failure_is_classified_distinctly() {
        let reason = pi_local_provider_output_failure_reason(
            "agent_end missing final assistant message",
            r#"{"type":"message","errorMessage":"The model produced output that does not match the expected peg-native format"}"#,
            "",
        );

        assert!(reason.contains("local_provider_tool_call_protocol_error"));
        assert!(!reason.contains("local_provider_empty_or_malformed_response"));
    }

    #[test]
    fn hermes_required_mode_requires_worker_brief_source() {
        let _env = ScopedEnv::set_many(&[
            ("HEPA_HERMES_REQUIRED", Some("true")),
            ("HEPA_HERMES_RUN_BRIEF_COMMAND", None),
            ("HEPA_HERMES_RUN_BRIEF_FILE", None),
        ]);
        let root = unique_test_dir("hepa-hermes-required-worker");
        let config = runtime_review_config(&root);

        let error = live_run_brief(&config).expect_err("Hermes-required mode must not fall back");

        assert!(error.contains("HEPA_HERMES_RUN_BRIEF_COMMAND"));
        assert!(error.contains("HEPA_HERMES_RUN_BRIEF_FILE"));

        remove_test_dir(root);
    }

    #[test]
    fn hermes_required_mode_requires_reviewer_source_before_fallback_review() {
        let _env = ScopedEnv::set_many(&[
            ("HEPA_HERMES_REQUIRED", Some("required")),
            ("HEPA_HERMES_REVIEWER_COMMAND", None),
            ("HEPA_HERMES_REVIEW_ARTIFACT_FILE", None),
        ]);
        let root = unique_test_dir("hermes-required-reviewer");
        let config = runtime_review_config(&root);
        let (lane_paths, allocation) = runtime_review_paths(&config);
        let spec = dummy_pi_spec();
        let environment = std::collections::BTreeMap::new();
        let validation = passed_validation();
        let changed_files = vec!["README.md".to_string()];

        let error = live_review_fanout(LiveReviewInput {
            config: &config,
            adapter_id: "pi",
            spec: &spec,
            environment: &environment,
            lane_paths: &lane_paths,
            allocation: &allocation,
            changed_files: &changed_files,
            validation: &validation,
            diff_context: "diff --git a/README.md b/README.md",
        })
        .expect_err("Hermes-required mode must not use deterministic or adapter review fallback");

        assert!(error.contains("HEPA_HERMES_REVIEWER_COMMAND"));
        assert!(error.contains("HEPA_HERMES_REVIEW_ARTIFACT_FILE"));

        remove_test_dir(root);
    }

    #[test]
    fn hermes_required_mode_requires_pr_intent_source_before_fallback_body() {
        let _env = ScopedEnv::set_many(&[
            ("HEPA_HERMES_REQUIRED", Some("1")),
            ("HEPA_HERMES_PR_INTENT_COMMAND", None),
            ("HEPA_HERMES_PR_INTENT_FILE", None),
        ]);
        let root = unique_test_dir("hermes-required-pr-intent");
        let config = runtime_review_config(&root);
        let task_spec = live_task_spec(&config);
        let validation = passed_validation();
        let review = HepaReviewSignal {
            schema_version: CONTRACT_SCHEMA_VERSION,
            review_id: "review-1".to_string(),
            lane_id: config.lane_id.clone(),
            adapter_id: "hepa-reviewer:primary".to_string(),
            status: HepaReviewStatus::Approved,
            findings: Vec::new(),
            summary: vec!["No blocking findings.".to_string()],
            completed_at: "2026-06-16T00:00:06Z".to_string(),
        };
        let timing = live_timing_record(LiveTimingInput {
            config: &config,
            adapter_id: "pi",
            worker_duration_seconds: 1.0,
            validation_duration_seconds: 0.1,
            review_duration_seconds: 0.1,
            reviewer_passes: 1,
            terminal_phase: LivePipelinePhase::Completed,
            repair_timing: None,
        });
        let terminal_report = terminal_report(&config, validation, review, timing);
        let lane = completed_lane(&config);

        let error = live_pr_request(
            &config,
            &task_spec,
            &terminal_report,
            &lane,
            &["README.md".to_string()],
            "main",
            "hepa/manager/lane-live",
        )
        .expect_err("Hermes-required mode must not build fallback PR bodies");

        assert!(error.contains("HEPA_HERMES_PR_INTENT_COMMAND"));
        assert!(error.contains("HEPA_HERMES_PR_INTENT_FILE"));

        remove_test_dir(root);
    }

    #[test]
    fn hermes_pr_intent_file_builds_live_pr_request() {
        let root = unique_test_dir("hermes-pr-intent");
        let intent_path = root.join("intent.json");
        let intent = HepaHermesPrIntent {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            author_profile_id: "hepa-manager".to_string(),
            title: "Add starter template readiness badge".to_string(),
            body: "## Summary\nAdds the requested app readiness badge.\n\n## Task\nAcceptance criteria:\n- Show the app readiness badge in the README.\n\n## Changes\n- Updated the README badge guidance.\n\n## Validation\n- yarn test:e2e passed\n\n## Review\n- Reviewer approved the scoped change.\n\n## Risk\n- Low risk; documentation-only change.\n".to_string(),
            audit_summary: vec![
                "HEPA validated staging before publishing.".to_string(),
                "Human review remains required.".to_string(),
            ],
            human_review_required: true,
        };
        fs::create_dir_all(&root).expect("root");
        fs::write(
            &intent_path,
            serde_json::to_string_pretty(&intent).expect("intent json"),
        )
        .expect("write intent");
        let config = runtime_review_config(&root);
        let task_spec = live_task_spec(&config);
        let validation = passed_validation();
        let review = HepaReviewSignal {
            schema_version: CONTRACT_SCHEMA_VERSION,
            review_id: "review-1".to_string(),
            lane_id: config.lane_id.clone(),
            adapter_id: "hepa-reviewer:primary".to_string(),
            status: HepaReviewStatus::Approved,
            findings: Vec::new(),
            summary: vec!["No blocking findings.".to_string()],
            completed_at: "2026-06-16T00:00:06Z".to_string(),
        };
        let timing = live_timing_record(LiveTimingInput {
            config: &config,
            adapter_id: "pi",
            worker_duration_seconds: 1.0,
            validation_duration_seconds: 0.1,
            review_duration_seconds: 0.1,
            reviewer_passes: 1,
            terminal_phase: LivePipelinePhase::Completed,
            repair_timing: None,
        });
        let terminal_report = terminal_report(&config, validation, review, timing);
        let lane = completed_lane(&config);

        let request = pr_request_from_hermes_intent_file(
            &intent_path,
            &task_spec,
            &terminal_report,
            &lane,
            "main".to_string(),
            "hepa/manager/lane-1".to_string(),
        )
        .expect("valid Hermes intent should build live PR request");

        assert_eq!(request.title, intent.title);
        assert_eq!(request.base_branch, "main");
        assert_eq!(request.head_branch, "hepa/manager/lane-1");
        assert!(
            request
                .body
                .contains("Adds the requested app readiness badge")
        );
        assert_eq!(request.body, intent.body.trim_end());
        assert!(!request.body.contains("## HEPA audit"));
        assert!(!request.body.contains("## HEPA run evidence"));
        assert!(!request.body.contains("Agent loops: 1"));
        assert!(!request.body.contains("PR intent author: hepa-manager"));

        remove_test_dir(root);
    }

    #[test]
    fn hermes_pr_intent_runtime_command_builds_live_pr_request() {
        let root = unique_test_dir("hermes-pr-intent-runtime");
        let script = root.join("fake-hermes-manager-pr");
        write_executable(
            &script,
            r###"#!/usr/bin/env sh
printf '%s\n' "manager pr-intent runtime invoked"
cat > "$HEPA_HERMES_ARTIFACT_OUT" <<'JSON'
{
  "schema_version": 1,
  "task_id": "task-live",
  "lane_id": "lane-live",
  "author_profile_id": "hepa-manager",
  "title": "Update README with project-specific guidance",
  "body": "## Summary\nAdds the requested project-specific README guidance.\n\n## Task\nAcceptance criteria:\n- Explain the project-specific usage clearly.\n\n## Changes\n- Updated README guidance for the requested project task.\n\n## Validation\n- git diff --check passed\n\n## Review\n- Reviewer approved the scoped change.\n\n## Risk\n- Low risk; documentation-only change.\n",
  "audit_summary": [
    "Hermes manager authored this PR intent.",
    "HEPA validated staging before publishing."
  ],
  "human_review_required": true
}
JSON
"###,
        );
        let config = runtime_review_config(&root);
        let task_spec = live_task_spec(&config);
        let validation = passed_validation();
        let review = HepaReviewSignal {
            schema_version: CONTRACT_SCHEMA_VERSION,
            review_id: "review-1".to_string(),
            lane_id: config.lane_id.clone(),
            adapter_id: "hepa-reviewer:primary".to_string(),
            status: HepaReviewStatus::Approved,
            findings: Vec::new(),
            summary: vec!["No blocking findings.".to_string()],
            completed_at: "2026-06-16T00:00:06Z".to_string(),
        };
        let timing = live_timing_record(LiveTimingInput {
            config: &config,
            adapter_id: "pi",
            worker_duration_seconds: 1.0,
            validation_duration_seconds: 0.1,
            review_duration_seconds: 0.1,
            reviewer_passes: 1,
            terminal_phase: LivePipelinePhase::Completed,
            repair_timing: None,
        });
        let terminal_report = terminal_report(&config, validation, review, timing);
        let lane = completed_lane(&config);

        let request = pr_request_from_hermes_intent_runtime_command(
            &script.display().to_string(),
            &config,
            &task_spec,
            &terminal_report,
            &lane,
            &["README.md".to_string()],
            "main".to_string(),
            "hepa/manager/lane-live".to_string(),
        )
        .expect("Hermes manager runtime should build PR request");

        assert_eq!(
            request.title,
            "Update README with project-specific guidance"
        );
        assert!(
            request
                .body
                .contains("Adds the requested project-specific README guidance")
        );
        assert!(
            !request
                .body
                .contains("Hermes manager authored this PR intent")
        );
        assert!(!request.body.contains("## HEPA audit"));
        assert!(
            fs::read_to_string(
                config.control_root.join(
                    "hermes-pr-intent/run-live/lane-live/hermes-pr-intent-runtime.stdout.log"
                )
            )
            .expect("PR intent runtime stdout")
            .contains("manager pr-intent runtime invoked")
        );
        assert!(
            fs::read_to_string(
                config
                    .control_root
                    .join("hermes-pr-intent/run-live/lane-live/hermes-pr-intent-context.json")
            )
            .expect("PR intent runtime context")
            .contains("\"changed_files\"")
        );

        remove_test_dir(root);
    }

    #[test]
    fn hermes_pr_intent_file_rejects_generic_live_pr_body() {
        let root = unique_test_dir("hermes-pr-intent-generic");
        let intent_path = root.join("intent.json");
        let intent = HepaHermesPrIntent {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            author_profile_id: "hepa-manager".to_string(),
            title: "HEPA validation: Update app".to_string(),
            body: "## Summary\n- Task: HEPA validation: update app\n\n## Changes\n- changed files\n\n## Validation\n- passed\n\n## Review\n- approved\n\n## Risk\n- low\n"
                .to_string(),
            audit_summary: vec!["validation passed".to_string()],
            human_review_required: true,
        };
        fs::create_dir_all(&root).expect("root");
        fs::write(
            &intent_path,
            serde_json::to_string_pretty(&intent).expect("intent json"),
        )
        .expect("write intent");
        let config = runtime_review_config(&root);
        let task_spec = live_task_spec(&config);
        let validation = passed_validation();
        let review = HepaReviewSignal {
            schema_version: CONTRACT_SCHEMA_VERSION,
            review_id: "review-1".to_string(),
            lane_id: config.lane_id.clone(),
            adapter_id: "hepa-reviewer:primary".to_string(),
            status: HepaReviewStatus::Approved,
            findings: Vec::new(),
            summary: vec!["No blocking findings.".to_string()],
            completed_at: "2026-06-16T00:00:06Z".to_string(),
        };
        let timing = live_timing_record(LiveTimingInput {
            config: &config,
            adapter_id: "pi",
            worker_duration_seconds: 1.0,
            validation_duration_seconds: 0.1,
            review_duration_seconds: 0.1,
            reviewer_passes: 1,
            terminal_phase: LivePipelinePhase::Completed,
            repair_timing: None,
        });
        let terminal_report = terminal_report(&config, validation, review, timing);
        let lane = completed_lane(&config);

        let error = pr_request_from_hermes_intent_file(
            &intent_path,
            &task_spec,
            &terminal_report,
            &lane,
            "main".to_string(),
            "hepa/manager/lane-1".to_string(),
        )
        .expect_err("generic Hermes PR body should be rejected");

        assert!(error.contains("Hermes PR intent failed validation"));
        assert!(error.contains("generic HEPA validation"));

        remove_test_dir(root);
    }

    #[test]
    fn hermes_run_brief_file_builds_live_task_spec() {
        let root = unique_test_dir("hermes-run-brief");
        let brief_path = root.join("brief.json");
        let brief = HepaHermesRunBrief {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            author_profile_id: "hepa-worker".to_string(),
            task_prompt: "Update src/app/login.tsx to show the requested copy.".to_string(),
            expected_areas: vec!["src/app/login.tsx".to_string()],
            acceptance_criteria: vec!["Login copy matches the task request.".to_string()],
            validation_commands: vec!["yarn test:e2e".to_string()],
            max_total_rounds: 3,
        };
        fs::create_dir_all(&root).expect("root");
        fs::write(
            &brief_path,
            serde_json::to_string_pretty(&brief).expect("brief json"),
        )
        .expect("write brief");

        let loaded =
            hermes_run_brief_from_file(&brief_path).expect("valid Hermes brief should load");
        let config = HepaFakeRunConfig {
            repo_path: PathBuf::from("repo"),
            control_root: PathBuf::from("control"),
            worktree_root: PathBuf::from("worktrees"),
            archive_root: PathBuf::from("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update the login page copy for returning users.".to_string(),
            timing: true,
        };
        let task_spec = live_task_spec_from_hermes_brief(&config, &loaded);

        assert_eq!(
            task_spec.goal,
            "Update the login page copy for returning users."
        );
        assert_ne!(task_spec.goal, brief.task_prompt);
        assert_eq!(task_spec.expected_areas, vec!["src/app/login.tsx"]);
        assert_eq!(task_spec.acceptance_criteria, brief.acceptance_criteria);
        assert_eq!(task_spec.validation_commands, vec!["yarn test:e2e"]);
        assert_eq!(task_spec.max_total_rounds, 3);
        assert!(
            task_spec
                .non_goals
                .iter()
                .any(|line| line.contains("Hermes worker brief scope"))
        );

        remove_test_dir(root);
    }

    #[test]
    fn hermes_run_brief_runtime_command_builds_live_task_spec() {
        let root = unique_test_dir("hermes-run-brief-runtime");
        let script = root.join("fake-hermes-worker");
        write_executable(
            &script,
            r#"#!/usr/bin/env sh
printf '%s\n' "worker runtime invoked"
cat > "$HEPA_HERMES_ARTIFACT_OUT" <<'JSON'
{
  "schema_version": 1,
  "task_id": "task-live",
  "lane_id": "lane-live",
  "author_profile_id": "hepa-worker",
  "task_prompt": "Update src/app/login.tsx to show the requested copy.",
  "expected_areas": ["src/app/login.tsx"],
  "acceptance_criteria": ["Login copy matches the task request."],
  "validation_commands": ["yarn test:e2e"],
  "max_total_rounds": 3
}
JSON
"#,
        );
        let config = runtime_review_config(&root);

        let brief = live_run_brief_from_runtime_command(&script.display().to_string(), &config)
            .expect("Hermes worker runtime should produce run brief");
        let task_spec = live_task_spec_from_hermes_brief(&config, &brief);

        assert_eq!(task_spec.goal, "Update README.md");
        assert_ne!(task_spec.goal, brief.task_prompt);
        assert_eq!(task_spec.expected_areas, vec!["src/app/login.tsx"]);
        assert_eq!(task_spec.validation_commands, vec!["yarn test:e2e"]);
        assert!(
            fs::read_to_string(
                config.control_root.join(
                    "hermes-run-brief/run-live/lane-live/hermes-run-brief-runtime.stdout.log"
                )
            )
            .expect("worker runtime stdout")
            .contains("worker runtime invoked")
        );

        remove_test_dir(root);
    }

    #[test]
    fn hermes_run_brief_file_rejects_non_worker_author() {
        let root = unique_test_dir("hermes-run-brief-author");
        let brief_path = root.join("brief.json");
        let brief = HepaHermesRunBrief {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            author_profile_id: "hepa-manager".to_string(),
            task_prompt: "Update README.md.".to_string(),
            expected_areas: vec!["README.md".to_string()],
            acceptance_criteria: vec!["README updated.".to_string()],
            validation_commands: vec!["git diff --check".to_string()],
            max_total_rounds: 1,
        };
        fs::create_dir_all(&root).expect("root");
        fs::write(
            &brief_path,
            serde_json::to_string_pretty(&brief).expect("brief json"),
        )
        .expect("write brief");

        let error = hermes_run_brief_from_file(&brief_path)
            .expect_err("non-worker Hermes brief should be rejected");

        assert!(error.contains("Hermes run brief failed validation"));
        assert!(error.contains("hepa-worker"));

        remove_test_dir(root);
    }

    #[test]
    fn hermes_review_artifact_file_builds_live_review_outcome() {
        let root = unique_test_dir("hermes-review-artifact");
        let artifact_path = root.join("review.json");
        let artifact = HepaHermesReviewArtifact {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            author_profile_id: "hepa-reviewer".to_string(),
            signals: vec![HepaReviewSignal {
                schema_version: CONTRACT_SCHEMA_VERSION,
                review_id: "review-1".to_string(),
                lane_id: "lane-live".to_string(),
                adapter_id: "hepa-reviewer:primary".to_string(),
                status: HepaReviewStatus::Approved,
                findings: Vec::new(),
                summary: vec!["No blocking findings.".to_string()],
                completed_at: "2026-06-16T00:00:06Z".to_string(),
            }],
            arbitration_required: false,
        };
        fs::create_dir_all(&root).expect("root");
        fs::write(
            &artifact_path,
            serde_json::to_string_pretty(&artifact).expect("artifact json"),
        )
        .expect("write artifact");

        let loaded = hermes_review_artifact_from_file(&artifact_path)
            .expect("valid Hermes review artifact should load");
        let outcome = live_review_outcome_from_signals_and_manager(
            loaded.signals,
            None,
            "Hermes review policy",
        )
        .expect("approved Hermes review should build outcome");

        assert_eq!(outcome.reviewer_passes, 1);
        assert!(outcome.staging_allowed);
        assert!(outcome.blockers.is_empty());
        assert_eq!(outcome.signals[0].adapter_id, "hepa-reviewer:primary");

        remove_test_dir(root);
    }

    #[test]
    fn hermes_review_manager_artifact_file_sets_live_arbitration() {
        let root = unique_test_dir("hermes-review-manager-artifact");
        let artifact_path = root.join("manager.json");
        let artifact = HepaHermesReviewManagerArtifact {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            author_profile_id: "hepa-review-manager".to_string(),
            arbitration: HepaArbitrationSummary {
                schema_version: CONTRACT_SCHEMA_VERSION,
                status: "settled".to_string(),
                records: vec![hepa_core::contracts::HepaArbitrationFindingRecord {
                    schema_version: CONTRACT_SCHEMA_VERSION,
                    finding_id: "finding-1".to_string(),
                    disposition: "manager_rejected".to_string(),
                    rule_id: Some("manager-judgment".to_string()),
                    reason: "Review manager rejected the advisory after validation passed."
                        .to_string(),
                    severity_before: HepaFindingSeverity::Medium,
                    severity_after: HepaFindingSeverity::Medium,
                    accepted: false,
                }],
                pr_body_lines: vec![
                    "- finding-1: manager_rejected, Medium -> Medium, accepted=false, reason=Review manager rejected the advisory after validation passed.".to_string(),
                ],
                card_status: "arbitration=settled records=1 accepted=0".to_string(),
            },
        };
        fs::create_dir_all(&root).expect("root");
        fs::write(
            &artifact_path,
            serde_json::to_string_pretty(&artifact).expect("manager artifact json"),
        )
        .expect("write manager artifact");

        let loaded = hermes_review_manager_artifact_from_file(&artifact_path)
            .expect("valid review-manager artifact should load");
        let blockers = blockers_from_arbitration_summary(&loaded.arbitration);
        let outcome = live_review_outcome_from_signals_and_manager(
            vec![HepaReviewSignal {
                schema_version: CONTRACT_SCHEMA_VERSION,
                review_id: "review-1".to_string(),
                lane_id: "lane-live".to_string(),
                adapter_id: "hepa-reviewer:primary".to_string(),
                status: HepaReviewStatus::Approved,
                findings: Vec::new(),
                summary: vec!["No blocking findings.".to_string()],
                completed_at: "2026-06-16T00:00:06Z".to_string(),
            }],
            Some(loaded.arbitration),
            "Hermes review policy",
        )
        .expect("settled review-manager arbitration should build outcome");

        assert!(blockers.is_empty());
        assert!(outcome.staging_allowed);
        assert_eq!(outcome.arbitration.status, "settled");
        assert_eq!(
            outcome.arbitration.records[0].disposition,
            "manager_rejected"
        );

        remove_test_dir(root);
    }

    #[test]
    fn hermes_review_manager_artifact_file_rejects_unresolved_arbitration() {
        let root = unique_test_dir("hermes-review-manager-unresolved");
        let artifact_path = root.join("manager.json");
        let artifact = HepaHermesReviewManagerArtifact {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            author_profile_id: "hepa-review-manager".to_string(),
            arbitration: HepaArbitrationSummary {
                schema_version: CONTRACT_SCHEMA_VERSION,
                status: "manager_required".to_string(),
                records: vec![hepa_core::contracts::HepaArbitrationFindingRecord {
                    schema_version: CONTRACT_SCHEMA_VERSION,
                    finding_id: "finding-1".to_string(),
                    disposition: "manager_required".to_string(),
                    rule_id: None,
                    reason: "Needs review-manager judgment.".to_string(),
                    severity_before: HepaFindingSeverity::High,
                    severity_after: HepaFindingSeverity::High,
                    accepted: true,
                }],
                pr_body_lines: vec!["- finding-1 needs judgment.".to_string()],
                card_status: "arbitration=manager_required records=1 accepted=1".to_string(),
            },
        };
        fs::create_dir_all(&root).expect("root");
        fs::write(
            &artifact_path,
            serde_json::to_string_pretty(&artifact).expect("manager artifact json"),
        )
        .expect("write manager artifact");

        let error = hermes_review_manager_artifact_from_file(&artifact_path)
            .expect_err("unresolved review-manager artifact should be rejected");

        assert!(error.contains("Hermes review-manager artifact failed validation"));

        remove_test_dir(root);
    }

    #[test]
    fn hermes_required_mode_requires_review_manager_when_review_has_findings() {
        let _env = ScopedEnv::set_many(&[
            ("HEPA_HERMES_REQUIRED", Some("yes")),
            ("HEPA_HERMES_REVIEW_MANAGER_COMMAND", None),
            ("HEPA_HERMES_REVIEW_MANAGER_ARTIFACT_FILE", None),
        ]);
        let root = unique_test_dir("hermes-required-review-manager");
        let config = runtime_review_config(&root);
        let (lane_paths, _) = runtime_review_paths(&config);
        let signal = HepaReviewSignal {
            schema_version: CONTRACT_SCHEMA_VERSION,
            review_id: "review-1".to_string(),
            lane_id: config.lane_id.clone(),
            adapter_id: "hepa-reviewer:primary".to_string(),
            status: HepaReviewStatus::Approved,
            findings: vec![HepaReviewFinding {
                finding_id: "finding-1".to_string(),
                severity: HepaFindingSeverity::Medium,
                category: "maintainability".to_string(),
                evidence: "Reviewer requested manager judgment.".to_string(),
                in_scope: true,
                release_risk: false,
                recommended_action: "Manager should arbitrate.".to_string(),
                file_ref: None,
                line: None,
                message: "Needs arbitration.".to_string(),
                accepted: true,
            }],
            summary: vec!["Finding requires manager arbitration.".to_string()],
            completed_at: "2026-06-16T00:00:06Z".to_string(),
        };

        let error = live_review_outcome_from_signals(
            &config,
            &lane_paths,
            vec![signal],
            "Hermes review policy",
        )
        .expect_err("Hermes-required findings must not use deterministic arbitration");

        assert!(error.contains("HEPA_HERMES_REVIEW_MANAGER_COMMAND"));
        assert!(error.contains("HEPA_HERMES_REVIEW_MANAGER_ARTIFACT_FILE"));

        remove_test_dir(root);
    }

    #[test]
    fn hermes_required_mode_allows_clean_reviewer_signal_without_review_manager() {
        let _env = ScopedEnv::set_many(&[
            ("HEPA_HERMES_REQUIRED", Some("on")),
            ("HEPA_HERMES_REVIEW_MANAGER_COMMAND", None),
            ("HEPA_HERMES_REVIEW_MANAGER_ARTIFACT_FILE", None),
        ]);
        let root = unique_test_dir("hermes-required-clean-review");
        let config = runtime_review_config(&root);
        let (lane_paths, _) = runtime_review_paths(&config);

        let outcome = live_review_outcome_from_signals(
            &config,
            &lane_paths,
            vec![HepaReviewSignal {
                schema_version: CONTRACT_SCHEMA_VERSION,
                review_id: "review-1".to_string(),
                lane_id: config.lane_id.clone(),
                adapter_id: "hepa-reviewer:primary".to_string(),
                status: HepaReviewStatus::Approved,
                findings: Vec::new(),
                summary: vec!["No blocking findings.".to_string()],
                completed_at: "2026-06-16T00:00:06Z".to_string(),
            }],
            "Hermes review policy",
        )
        .expect("clean Hermes review can proceed without a review-manager artifact");

        assert!(outcome.staging_allowed);
        assert_eq!(outcome.reviewer_passes, 1);

        remove_test_dir(root);
    }

    #[test]
    fn hermes_review_artifact_file_rejects_pi_review_signal() {
        let root = unique_test_dir("hermes-review-artifact-pi");
        let artifact_path = root.join("review.json");
        let artifact = HepaHermesReviewArtifact {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            author_profile_id: "hepa-reviewer".to_string(),
            signals: vec![HepaReviewSignal {
                schema_version: CONTRACT_SCHEMA_VERSION,
                review_id: "review-1".to_string(),
                lane_id: "lane-live".to_string(),
                adapter_id: "pi".to_string(),
                status: HepaReviewStatus::Approved,
                findings: Vec::new(),
                summary: vec!["No blocking findings.".to_string()],
                completed_at: "2026-06-16T00:00:06Z".to_string(),
            }],
            arbitration_required: false,
        };
        fs::create_dir_all(&root).expect("root");
        fs::write(
            &artifact_path,
            serde_json::to_string_pretty(&artifact).expect("artifact json"),
        )
        .expect("write artifact");

        let error = hermes_review_artifact_from_file(&artifact_path)
            .expect_err("Pi-labeled review signal should be rejected");

        assert!(error.contains("Hermes review artifact failed validation"));
        assert!(error.contains("Hermes reviewer profile"));

        remove_test_dir(root);
    }

    #[test]
    fn hermes_reviewer_runtime_command_builds_live_review_outcome() {
        let root = unique_test_dir("hermes-reviewer-runtime");
        let script = root.join("fake-hermes-reviewer");
        write_executable(
            &script,
            r#"#!/usr/bin/env sh
printf '%s\n' "reviewer runtime invoked"
cat > "$HEPA_HERMES_ARTIFACT_OUT" <<'JSON'
{
  "schema_version": 1,
  "task_id": "task-live",
  "lane_id": "lane-live",
  "author_profile_id": "hepa-reviewer",
  "signals": [
    {
      "schema_version": 1,
      "review_id": "review-runtime-1",
      "lane_id": "lane-live",
      "adapter_id": "hepa-reviewer:runtime-primary",
      "status": "approved",
      "findings": [],
      "summary": ["Runtime reviewer approved the diff."],
      "completed_at": "2026-06-16T00:00:06Z"
    }
  ],
  "arbitration_required": false
}
JSON
"#,
        );
        let config = runtime_review_config(&root);
        let (lane_paths, allocation) = runtime_review_paths(&config);
        let spec = dummy_pi_spec();
        let environment = std::collections::BTreeMap::new();
        let validation = passed_validation();
        let changed_files = vec!["README.md".to_string()];

        let input = LiveReviewInput {
            config: &config,
            adapter_id: "pi",
            spec: &spec,
            environment: &environment,
            lane_paths: &lane_paths,
            allocation: &allocation,
            changed_files: &changed_files,
            validation: &validation,
            diff_context: "diff --git a/README.md b/README.md",
        };
        let artifact =
            live_review_artifact_from_runtime_command(&script.display().to_string(), &input)
                .expect("Hermes reviewer runtime should produce artifact");
        write_json(
            &lane_paths
                .lane_dir
                .join("review/hermes-review-artifact.json"),
            &artifact,
        )
        .expect("persist review artifact");
        let outcome = live_review_outcome_from_signals(
            &config,
            &lane_paths,
            artifact.signals,
            "Hermes reviewer runtime policy",
        )
        .expect("Hermes reviewer runtime should produce review outcome");

        assert!(outcome.staging_allowed);
        assert_eq!(outcome.reviewer_passes, 1);
        assert_eq!(
            outcome.signals[0].adapter_id,
            "hepa-reviewer:runtime-primary"
        );
        assert!(
            fs::read_to_string(
                lane_paths
                    .lane_dir
                    .join("review/hermes-reviewer-runtime.stdout.log")
            )
            .expect("reviewer runtime stdout")
            .contains("reviewer runtime invoked")
        );
        assert!(
            lane_paths
                .lane_dir
                .join("review/hermes-review-artifact.json")
                .exists()
        );

        remove_test_dir(root);
    }

    #[test]
    fn hermes_review_manager_runtime_command_sets_live_arbitration() {
        let root = unique_test_dir("hermes-review-manager-runtime");
        let script = root.join("fake-hermes-review-manager");
        write_executable(
            &script,
            r#"#!/usr/bin/env sh
printf '%s\n' "review-manager runtime invoked"
cat > "$HEPA_HERMES_ARTIFACT_OUT" <<'JSON'
{
  "schema_version": 1,
  "task_id": "task-live",
  "lane_id": "lane-live",
  "author_profile_id": "hepa-review-manager",
  "arbitration": {
    "schema_version": 1,
    "status": "settled",
    "records": [
      {
        "schema_version": 1,
        "finding_id": "finding-runtime-1",
        "disposition": "manager_rejected",
        "rule_id": "manager-judgment",
        "reason": "Runtime review-manager rejected the advisory.",
        "severity_before": "medium",
        "severity_after": "medium",
        "accepted": false
      }
    ],
    "pr_body_lines": ["- finding-runtime-1 rejected by review manager."],
    "card_status": "arbitration=settled records=1 accepted=0"
  }
}
JSON
"#,
        );
        let config = runtime_review_config(&root);
        let (lane_paths, _) = runtime_review_paths(&config);
        let signal = HepaReviewSignal {
            schema_version: CONTRACT_SCHEMA_VERSION,
            review_id: "review-runtime-1".to_string(),
            lane_id: config.lane_id.clone(),
            adapter_id: "hepa-reviewer:runtime-primary".to_string(),
            status: HepaReviewStatus::Approved,
            findings: vec![HepaReviewFinding {
                finding_id: "finding-runtime-1".to_string(),
                severity: HepaFindingSeverity::Medium,
                category: "advisory".to_string(),
                evidence: "Runtime reviewer advisory.".to_string(),
                in_scope: true,
                release_risk: false,
                recommended_action: "Review-manager should arbitrate.".to_string(),
                file_ref: None,
                line: None,
                message: "Advisory finding.".to_string(),
                accepted: true,
            }],
            summary: vec!["Runtime reviewer requested arbitration.".to_string()],
            completed_at: "2026-06-16T00:00:06Z".to_string(),
        };

        let artifact = live_review_manager_artifact_from_runtime_command(
            &script.display().to_string(),
            &config,
            &lane_paths,
            std::slice::from_ref(&signal),
        )
        .expect("Hermes review-manager runtime should produce artifact");
        write_json(
            &lane_paths
                .lane_dir
                .join("review/hermes-review-manager-artifact.json"),
            &artifact,
        )
        .expect("persist review-manager artifact");
        let outcome = live_review_outcome_from_signals_and_manager(
            vec![signal],
            Some(artifact.arbitration),
            "Hermes reviewer runtime policy",
        )
        .expect("Hermes review-manager runtime should settle arbitration");

        assert!(outcome.staging_allowed);
        assert_eq!(outcome.arbitration.status, "settled");
        assert_eq!(
            outcome.arbitration.records[0].disposition,
            "manager_rejected"
        );
        assert!(
            fs::read_to_string(
                lane_paths
                    .lane_dir
                    .join("review/hermes-review-manager-runtime.stdout.log")
            )
            .expect("review-manager runtime stdout")
            .contains("review-manager runtime invoked")
        );

        remove_test_dir(root);
    }

    #[test]
    fn pi_adapter_cannot_run_live_review_in_hermes_led_workflow() {
        let root = unique_test_dir("pi-review-blocked");
        let config = HepaFakeRunConfig {
            repo_path: root.join("repo"),
            control_root: root.join("control"),
            worktree_root: root.join("worktrees"),
            archive_root: root.join("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        };
        let layout =
            HepaArtifactLayout::new(&config.control_root, &config.archive_root).expect("layout");
        let lane_paths = layout
            .run(&config.run_id, &config.task_id)
            .expect("run paths")
            .lane(&config.lane_id)
            .expect("lane paths");
        let allocation = HepaWorktreeAllocation {
            lane_id: config.lane_id.clone(),
            branch: "hepa/lane-live".to_string(),
            worktree_path: root.join("worktree"),
            base_commit: "base".to_string(),
            metadata_path: root.join("worktree/.hepa-worktree.json"),
        };
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "pi".to_string(),
            display_name: "Pi Coding Agent".to_string(),
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: "unused".to_string(),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: Vec::new(),
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::None,
            supports_resume: false,
            supports_json_output: true,
            capabilities: Vec::new(),
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 1,
            prompt_transport: HepaAdapterPromptTransport::Stdin,
            output_capture: HepaAdapterOutputCapture::Stdout,
        };
        let environment = std::collections::BTreeMap::new();
        let validation = HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Passed,
            commands: Vec::new(),
            no_tests_detected: false,
            failure_type: None,
            summary: vec!["Validation passed.".to_string()],
        };
        let changed_files = vec!["README.md".to_string()];
        let error = live_adapter_review(LiveReviewInput {
            config: &config,
            adapter_id: "pi",
            spec: &spec,
            environment: &environment,
            lane_paths: &lane_paths,
            allocation: &allocation,
            changed_files: &changed_files,
            validation: &validation,
            diff_context: "diff --git a/README.md b/README.md",
        })
        .expect_err("Pi must not be allowed to run review");

        assert!(error.contains("implementation-only"));
        assert!(error.contains("Hermes reviewer profiles"));

        remove_test_dir(root);
    }

    #[test]
    fn live_task_spec_derives_validation_and_expected_areas() {
        let config = HepaFakeRunConfig {
            repo_path: PathBuf::from("repo"),
            control_root: PathBuf::from("control"),
            worktree_root: PathBuf::from("worktrees"),
            archive_root: PathBuf::from("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update src/app/views/login-and-registration/login-form.test.tsx and run yarn test login-form.test.tsx".to_string(),
            timing: true,
        };
        let task_spec = live_task_spec(&config);

        assert_eq!(
            task_spec.expected_areas,
            vec!["src/app/views/login-and-registration/login-form.test.tsx"]
        );
        assert_eq!(
            task_spec.validation_commands,
            vec!["npx vitest run login-form.test.tsx"]
        );
        assert!(
            task_spec
                .acceptance_criteria
                .iter()
                .any(|criterion| criterion.contains("pull request creation"))
        );
    }

    #[test]
    fn live_task_spec_derives_pnpm_monorepo_validation_commands() {
        let config = HepaFakeRunConfig {
            repo_path: PathBuf::from("repo"),
            control_root: PathBuf::from("control"),
            worktree_root: PathBuf::from("worktrees"),
            archive_root: PathBuf::from("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update AGENTS.md from the roadmap. Run pnpm install --frozen-lockfile --offline and pnpm format:check.".to_string(),
            timing: true,
        };
        let task_spec = live_task_spec(&config);

        assert_eq!(
            task_spec.validation_commands,
            vec![
                "pnpm install --frozen-lockfile --offline",
                "pnpm format:check"
            ]
        );
    }

    #[test]
    fn live_task_spec_derives_yarn_app_validation_commands() {
        let config = HepaFakeRunConfig {
            repo_path: PathBuf::from("repo"),
            control_root: PathBuf::from("control"),
            worktree_root: PathBuf::from("worktrees"),
            archive_root: PathBuf::from("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Add a status badge. Validation commands: yarn test:e2e; yarn build."
                .to_string(),
            timing: true,
        };
        let task_spec = live_task_spec(&config);

        assert_eq!(
            task_spec.validation_commands,
            vec![
                "yarn install --frozen-lockfile",
                "yarn test:e2e",
                "yarn build"
            ]
        );
    }

    #[test]
    fn live_controlled_repair_trigger_rewrites_validation_summary() {
        let validation = HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Passed,
            commands: vec![HepaValidationCommandResult {
                command: "git diff --check".to_string(),
                exit_code: 0,
                duration_ms: 4,
            }],
            no_tests_detected: false,
            failure_type: None,
            summary: vec!["`git diff --check` exited 0.".to_string()],
        };

        let forced = force_validation_failure_for_repair_stress(validation);

        assert_eq!(forced.status, HepaValidationStatus::Failed);
        assert_eq!(
            forced.failure_type.as_deref(),
            Some("rs4_controlled_validation_failure")
        );
        assert_eq!(forced.commands[0].exit_code, 1);
        assert!(
            forced
                .summary
                .iter()
                .any(|line| line.contains("RS-4 controlled repair trigger"))
        );
    }

    #[test]
    fn live_secret_path_fixture_is_collected_and_rejected_by_staging() {
        let root = unique_test_dir("live-secret-path-fixture");
        let repo = root.join("repo");
        init_repo(&repo);

        inject_secret_path_fixture(&repo).expect("fixture should write");
        let changed = collect_changed_files(&repo).expect("changed files should collect fixture");

        assert_eq!(changed, vec![".ssh/id_rsa".to_string()]);
        let error = HepaSafeStaging::new(&repo)
            .stage_approved_files(&changed)
            .expect_err("secret-like path must be rejected");
        assert!(error.to_string().contains("secret-like paths"));
        remove_test_dir(root);
    }

    #[test]
    fn live_secret_path_block_redacts_report_path() {
        let changed = vec!["docs/README.md".to_string(), ".ssh/id_rsa".to_string()];

        let secret_path = first_secret_like_changed_path(&changed).expect("secret path");

        assert_eq!(secret_path, ".ssh/id_rsa");
        assert_eq!(
            redact_secret_like_path_for_report(&secret_path),
            "<secret-like-path>"
        );
    }

    #[test]
    fn manager_changed_file_policy_blocks_unrequested_lockfile_churn() {
        let changed = vec![
            "apps/web/README.md".to_string(),
            "apps/web/package.json".to_string(),
            "apps/web/src/__tests__/health.test.ts".to_string(),
            "apps/web/src/helpers/validators.ts".to_string(),
            "pnpm-lock.yaml".to_string(),
        ];

        let blocker = manager_changed_file_policy_blocker(
            &changed,
            "Improve the apps/web Jest health-check surface with a script, utility, tests, and docs.",
        )
        .expect("unrequested lockfile churn should block");

        assert!(blocker.contains("pnpm-lock.yaml"));
        assert!(blocker.contains("dependency lockfile changes"));
    }

    #[test]
    fn manager_changed_file_policy_treats_negative_dependency_text_as_no_permission() {
        let changed = vec![
            "apps/api-gateway/src/__tests__/route-parity.test.ts".to_string(),
            "pnpm-lock.yaml".to_string(),
            "pnpm-workspace.yaml".to_string(),
        ];

        let blocker = manager_changed_file_policy_blocker(
            &changed,
            "Do not edit pnpm-lock.yaml, pnpm-workspace.yaml, package.json dependency lists, generated dist files, or node_modules.",
        )
        .expect("negative dependency instructions must not authorize package-manager churn");

        assert!(blocker.contains("pnpm-lock.yaml"));
    }

    #[test]
    fn manager_changed_file_policy_blocks_unrequested_workspace_config_churn() {
        let changed = vec![
            "apps/web/package.json".to_string(),
            "pnpm-workspace.yaml".to_string(),
        ];

        let blocker = manager_changed_file_policy_blocker(
            &changed,
            "Add a deterministic test script to apps/web/package.json.",
        )
        .expect("unrequested workspace config churn should block");

        assert!(blocker.contains("pnpm-workspace.yaml"));
        assert!(blocker.contains("workspace/package-manager config"));
    }

    #[test]
    fn manager_changed_file_policy_allows_explicit_dependency_and_workspace_tasks() {
        let dependency_changed = vec![
            "apps/web/package.json".to_string(),
            "pnpm-lock.yaml".to_string(),
        ];
        assert!(
            manager_changed_file_policy_blocker(
                &dependency_changed,
                "Add a dependency and update the lockfile."
            )
            .is_none()
        );

        let workspace_changed = vec![
            "package.json".to_string(),
            "pnpm-workspace.yaml".to_string(),
        ];
        assert!(
            manager_changed_file_policy_blocker(
                &workspace_changed,
                "Update the pnpm-workspace package-manager configuration."
            )
            .is_none()
        );
    }

    #[test]
    fn dependency_reuse_links_root_node_modules_into_live_worktree() {
        let root = unique_test_dir("dependency-reuse");
        let repo = root.join("repo");
        let worktree = root.join("worktree");
        fs::create_dir_all(repo.join("node_modules")).expect("repo node_modules");
        fs::create_dir_all(&worktree).expect("worktree");

        prepare_live_worktree_dependency_reuse(&repo, &worktree).expect("dependency reuse");

        assert!(worktree.join("node_modules").exists());
        remove_test_dir(root);
    }

    #[test]
    fn unrequested_package_manager_churn_is_discarded_before_staging() {
        let root = unique_test_dir("discard-lockfile-churn");
        let repo = root.join("repo");
        init_repo(&repo);
        fs::write(repo.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").expect("lockfile");
        fs::write(repo.join("src.ts"), "export const value = 1;\n").expect("source");
        git(&repo, ["add", "pnpm-lock.yaml", "src.ts"]);
        git(&repo, ["commit", "-m", "add app files"]);

        fs::write(
            repo.join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\nchanged: true\n",
        )
        .expect("lockfile change");
        fs::write(repo.join("src.ts"), "export const value = 2;\n").expect("source change");

        let changed = collect_changed_files(&repo).expect("changed files");
        let cleaned = discard_unrequested_package_manager_churn(
            &repo,
            &changed,
            "Improve source behavior without dependency changes.",
        )
        .expect("discard churn");

        assert_eq!(cleaned, vec!["src.ts".to_string()]);
        assert_eq!(
            fs::read_to_string(repo.join("pnpm-lock.yaml")).expect("lockfile restored"),
            "lockfileVersion: '9.0'\n"
        );
        remove_test_dir(root);
    }

    #[test]
    fn safe_validation_argv_allows_dashboard_package_scripts() {
        assert_eq!(
            safe_validation_argv("pnpm --filter @todo/api-gateway test").expect("pnpm filter"),
            vec!["pnpm", "--filter", "@todo/api-gateway", "test"]
        );
        assert_eq!(
            safe_validation_argv("pnpm --filter @todo/services test").expect("pnpm filter"),
            vec!["pnpm", "--filter", "@todo/services", "test"]
        );
        assert_eq!(
            safe_validation_argv("yarn test").expect("yarn test"),
            vec!["yarn", "test"]
        );
        assert_eq!(
            safe_validation_argv("yarn test src/app/util/util.test.ts")
                .expect("targeted yarn test"),
            vec!["yarn", "test", "src/app/util/util.test.ts"]
        );
        assert_eq!(
            safe_validation_argv("pnpm --filter @todo/web test -- TodoFilters TodoList")
                .expect("pnpm filter test args"),
            vec![
                "pnpm",
                "--filter",
                "@todo/web",
                "test",
                "--",
                "TodoFilters",
                "TodoList"
            ]
        );
        assert_eq!(
            safe_validation_argv(
                "bun test apps/api-gateway/src/__tests__/app.test.ts apps/api-gateway/src/__tests__/health.test.ts"
            )
            .expect("bun test files"),
            vec![
                "bun",
                "test",
                "apps/api-gateway/src/__tests__/app.test.ts",
                "apps/api-gateway/src/__tests__/health.test.ts"
            ]
        );
        assert_eq!(
            safe_validation_argv("bunx tsc --noEmit -p apps/api-gateway/tsconfig.json")
                .expect("bunx tsc project"),
            vec![
                "bunx",
                "tsc",
                "--noEmit",
                "-p",
                "apps/api-gateway/tsconfig.json"
            ]
        );
    }

    #[test]
    fn safe_validation_argv_rejects_shell_commands() {
        let error = safe_validation_argv("pnpm --filter @todo/api-gateway test; rm -rf .")
            .expect_err("shell metachar command must not be accepted");

        assert!(error.contains("unsupported live validation command"));
    }

    #[test]
    fn summarize_changed_files_lists_concrete_paths_for_pr_evidence() {
        let changed = vec![
            "apps/web/README.md".to_string(),
            "apps/web/package.json".to_string(),
            "apps/web/src/lib/health.ts".to_string(),
        ];

        assert_eq!(
            summarize_changed_files(&changed),
            "apps/web/README.md, apps/web/package.json, apps/web/src/lib/health.ts"
        );
        assert_eq!(summarize_changed_files(&[]), "none");
    }

    #[test]
    fn live_controlled_git_lifecycle_block_uses_monitor_policy() {
        let blocked = controlled_git_lifecycle_block().expect("controlled block");

        assert_eq!(blocked.reason, "command_policy");
        assert_eq!(blocked.card_status, "blocked");
        assert!(blocked.human_attention_required);
        assert!(blocked.evidence.contains("git commit"));
    }

    #[test]
    fn live_timing_safety_block_stops_before_review() {
        let config = HepaFakeRunConfig {
            repo_path: PathBuf::from("repo"),
            control_root: PathBuf::from("control"),
            worktree_root: PathBuf::from("worktrees"),
            archive_root: PathBuf::from("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Create a secret-like path fixture.".to_string(),
            timing: true,
        };

        let timing = live_timing_record(LiveTimingInput {
            config: &config,
            adapter_id: "pi",
            worker_duration_seconds: 1.0,
            validation_duration_seconds: 0.1,
            review_duration_seconds: 0.0,
            reviewer_passes: 0,
            terminal_phase: LivePipelinePhase::SafetyBlocked,
            repair_timing: None,
        });

        assert_eq!(timing.counters.reviewer_passes, 0);
        assert!(
            timing
                .phases
                .iter()
                .all(|phase| phase.name != "live_review_fanout")
        );
        assert!(
            timing
                .phases
                .iter()
                .any(|phase| phase.name == "live_validation"
                    && phase.status == HepaPhaseStatus::Completed)
        );
    }

    #[test]
    fn live_review_and_timing_records_are_not_fake() {
        let config = HepaFakeRunConfig {
            repo_path: PathBuf::from("repo"),
            control_root: PathBuf::from("control"),
            worktree_root: PathBuf::from("worktrees"),
            archive_root: PathBuf::from("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Run a real frontend task".to_string(),
            timing: true,
        };
        let timing = live_timing_record(LiveTimingInput {
            config: &config,
            adapter_id: "pi",
            worker_duration_seconds: 2.0,
            validation_duration_seconds: 1.0,
            review_duration_seconds: 0.5,
            reviewer_passes: 2,
            terminal_phase: LivePipelinePhase::Completed,
            repair_timing: None,
        });

        assert_eq!(timing.counters.agent_loops, 1);
        assert_eq!(timing.counters.manager_passes, 1);
        assert_eq!(timing.counters.reviewer_passes, 2);
        assert_eq!(timing.counters.container_count, 0);
        assert!(
            timing
                .phases
                .iter()
                .any(|phase| phase.name == "live_worker")
        );
        assert!(
            timing
                .phases
                .iter()
                .any(|phase| phase.name == "live_review_fanout")
        );
        assert!(
            timing
                .phases
                .iter()
                .any(|phase| phase.name == "live_arbitration")
        );
        assert!(
            timing
                .phases
                .iter()
                .any(|phase| phase.name == "live_staging_commit_pr")
        );
        assert!(
            timing
                .phases
                .iter()
                .all(|phase| !phase.name.starts_with("fake_"))
        );
    }

    #[test]
    fn live_repair_prompt_preserves_failure_evidence_and_lifecycle_boundary() {
        let config = hepa_core::config::HepaConfig::load(
            None,
            &std::collections::BTreeMap::new(),
            HepaConfigOverrides {
                pi_model: Some("deepseek/deepseek-chat".to_string()),
                ..HepaConfigOverrides::default()
            },
        )
        .expect("config should load");

        let prompt = live_repair_worker_prompt_for_adapter(
            "Ralph-V2 repair brief for lane lane-1\nRound: 2\n\nEvidence to address:\n- command `git diff --check` failed with exit code 2 after 10 ms.",
            "pi",
            &config,
        );

        assert!(prompt.contains("Round: 2"));
        assert!(prompt.contains("git diff --check"));
        assert!(prompt.contains("Fix only the evidenced failures"));
        assert!(prompt.contains("HEPA owns the Git lifecycle"));
        assert!(prompt.contains("Repository privacy rules"));
        assert!(prompt.contains("Do not write absolute local filesystem paths"));
        assert!(prompt.contains("Pi tool path rules"));
        assert!(prompt.contains("find call uses a search root"));
        assert!(!prompt.contains("/no_think"));
    }

    #[test]
    fn live_repair_respects_one_round_hermes_budget_before_worker_launch() {
        let root = unique_test_dir("repair-budget-one-round");
        let layout = HepaArtifactLayout::new(root.join("control"), root.join("archive"))
            .expect("artifact layout");
        let run_paths = layout
            .run("run-live", "task-live")
            .expect("run artifact paths");
        let lane_paths = run_paths.lane("lane-live").expect("lane artifact paths");
        let config = HepaFakeRunConfig {
            repo_path: root.join("repo"),
            control_root: root.join("control"),
            worktree_root: root.join("worktrees"),
            archive_root: root.join("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        };
        let mut task_spec = live_task_spec(&config);
        task_spec.max_total_rounds = 1;
        let allocation = HepaWorktreeAllocation {
            lane_id: "lane-live".to_string(),
            branch: "hepa/lane-live".to_string(),
            worktree_path: root.join("worktree"),
            base_commit: "base".to_string(),
            metadata_path: root.join("worktree/.hepa-worktree.json"),
        };
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "pi".to_string(),
            display_name: "Pi Coding Agent".to_string(),
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: "unused".to_string(),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: Vec::new(),
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::None,
            supports_resume: false,
            supports_json_output: true,
            capabilities: Vec::new(),
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 1,
            prompt_transport: HepaAdapterPromptTransport::Stdin,
            output_capture: HepaAdapterOutputCapture::Stdout,
        };
        let failed_validation = HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Failed,
            commands: vec![HepaValidationCommandResult {
                command: "git diff --check".to_string(),
                exit_code: 1,
                duration_ms: 10,
            }],
            no_tests_detected: false,
            failure_type: Some("validation_failed".to_string()),
            summary: vec!["`git diff --check` exited 1.".to_string()],
        };

        let error = run_live_repair_round(RunLiveRepairInput {
            config: &config,
            lane_paths: &lane_paths,
            allocation: &allocation,
            task_spec: &task_spec,
            spec,
            adapter_id: "pi",
            environment: std::collections::BTreeMap::new(),
            prior_prompt: "Update README.md".to_string(),
            failed_validation,
            review_findings: Vec::new(),
            repair_round: 2,
            first_changed_files: vec!["README.md".to_string()],
        })
        .expect_err("one-round budget should block repair before worker launch");

        assert!(error.contains("repair budget blocked round 2"));
        let budget = fs::read_to_string(lane_paths.lane_dir.join("repair/round-2-budget.json"))
            .expect("budget artifact");
        assert!(budget.contains("\"max_total_attempts\": 1"));
        assert!(budget.contains("\"allowed\": false"));
        assert!(
            !lane_paths
                .lane_dir
                .join("repair/round-2-prompt.md")
                .exists()
        );

        remove_test_dir(root);
    }

    #[test]
    fn live_repair_allows_third_total_round_before_human_cap() {
        let root = unique_test_dir("repair-budget-third-round");
        let layout = HepaArtifactLayout::new(root.join("control"), root.join("archive"))
            .expect("artifact layout");
        let run_paths = layout
            .run("run-live", "task-live")
            .expect("run artifact paths");
        let lane_paths = run_paths.lane("lane-live").expect("lane artifact paths");
        let config = HepaFakeRunConfig {
            repo_path: root.join("repo"),
            control_root: root.join("control"),
            worktree_root: root.join("worktrees"),
            archive_root: root.join("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        };
        let mut task_spec = live_task_spec(&config);
        task_spec.max_total_rounds = 3;
        let allocation = HepaWorktreeAllocation {
            lane_id: "lane-live".to_string(),
            branch: "hepa/lane-live".to_string(),
            worktree_path: root.join("worktree"),
            base_commit: "base".to_string(),
            metadata_path: root.join("worktree/.hepa-worktree.json"),
        };
        init_repo(&allocation.worktree_path);
        let spec = dummy_pi_spec();
        let failed_validation = HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Failed,
            commands: vec![HepaValidationCommandResult {
                command: "git diff --check".to_string(),
                exit_code: 1,
                duration_ms: 10,
            }],
            no_tests_detected: false,
            failure_type: Some("validation_failed".to_string()),
            summary: vec!["`git diff --check` exited 1.".to_string()],
        };

        let error = run_live_repair_round(RunLiveRepairInput {
            config: &config,
            lane_paths: &lane_paths,
            allocation: &allocation,
            task_spec: &task_spec,
            spec,
            adapter_id: "pi",
            environment: std::collections::BTreeMap::new(),
            prior_prompt: "Update README.md".to_string(),
            failed_validation,
            review_findings: vec![HepaReviewFinding {
                finding_id: "finding-review-1".to_string(),
                severity: HepaFindingSeverity::High,
                category: "correctness".to_string(),
                evidence: "Reviewer found the acceptance criterion still unmet.".to_string(),
                in_scope: true,
                release_risk: true,
                recommended_action: "Repair the implementation and rerun validation.".to_string(),
                file_ref: Some("README.md".to_string()),
                line: None,
                message: "Review requires a worker repair.".to_string(),
                accepted: true,
            }],
            repair_round: 3,
            first_changed_files: vec!["README.md".to_string()],
        })
        .expect_err("dummy adapter should fail after budget allows round 3");

        assert!(!error.contains("repair budget blocked"));
        let budget = fs::read_to_string(lane_paths.lane_dir.join("repair/round-3-budget.json"))
            .expect("round 3 budget artifact");
        assert!(budget.contains("\"max_total_attempts\": 3"));
        assert!(budget.contains("\"allowed\": true"));
        let brief = fs::read_to_string(lane_paths.lane_dir.join("repair/round-3-brief.json"))
            .expect("round 3 brief artifact");
        assert!(brief.contains("finding-review-1"));
        assert!(
            lane_paths
                .lane_dir
                .join("repair/round-3-prompt.md")
                .exists()
        );

        remove_test_dir(root);
    }

    #[test]
    fn live_pi_noop_text_response_blocks_before_validation() {
        let root = unique_test_dir("live-pi-noop-output");
        let repo = root.join("repo");
        init_repo(&repo);
        let config = HepaFakeRunConfig {
            repo_path: repo.clone(),
            control_root: root.join("control"),
            worktree_root: root.join("worktrees"),
            archive_root: root.join("archive"),
            run_id: "run-noop".to_string(),
            task_id: "task-noop".to_string(),
            lane_id: "lane-noop".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        };
        let layout =
            HepaArtifactLayout::new(&config.control_root, &config.archive_root).expect("layout");
        let lane_paths = layout
            .run(&config.run_id, &config.task_id)
            .expect("run paths")
            .lane(&config.lane_id)
            .expect("lane paths");
        fs::create_dir_all(&lane_paths.lane_dir).expect("lane dir");
        let allocator = HepaWorktreeAllocator::new(&config.repo_path, &config.worktree_root);
        let allocation = allocator
            .allocate_lane_with_metadata(&config.lane_id, "2026-06-16T00:00:00Z")
            .expect("allocation");
        let fake_pi = root.join("pi-noop");
        write_executable(
            &fake_pi,
            "#!/usr/bin/env sh\ncat >/dev/null\nprintf '%s\n' '{\"type\":\"agent_start\"}' '{\"type\":\"agent_end\",\"messages\":[{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"I will update README.md.\"}]}]}'\n",
        );
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "pi".to_string(),
            display_name: "Pi Coding Agent".to_string(),
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: format!("{} --mode json", fake_pi.display()),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: vec![fake_pi.to_string_lossy().to_string()],
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::None,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["local-only".to_string()],
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 1,
            prompt_transport: HepaAdapterPromptTransport::Stdin,
            output_capture: HepaAdapterOutputCapture::Stdout,
        };

        let result = execute_live_worker_attempt(ExecuteLiveAttemptInput {
            config: &config,
            lane_paths: &lane_paths,
            allocation: &allocation,
            spec,
            adapter_id: "pi",
            environment: std::collections::BTreeMap::new(),
            attempt_id: "attempt-1",
            round: 1,
            prompt: "fixture prompt".to_string(),
            started_at: "2026-06-16T00:00:02Z",
            completed_at: "2026-06-16T00:00:03Z",
        });
        let error = match result {
            Ok(_) => panic!("no-op Pi response must block"),
            Err(error) => error,
        };

        assert!(error.contains("local_provider_no_tool_activity_or_changes"));
        let attempt_json = fs::read_to_string(
            lane_paths
                .attempt("attempt-1")
                .expect("attempt paths")
                .attempt_dir
                .join("attempt.json"),
        )
        .expect("attempt report");
        assert!(attempt_json.contains("\"status\": \"blocked\""));
        assert!(attempt_json.contains("local_provider_no_tool_activity_or_changes"));

        allocator
            .cleanup_lane(&config.lane_id, "2026-06-16T00:00:09Z")
            .expect("cleanup");
        remove_test_dir(root);
    }

    #[test]
    fn live_repair_blocks_fourth_round_for_human_intervention() {
        let root = unique_test_dir("repair-budget-fourth-round");
        let layout = HepaArtifactLayout::new(root.join("control"), root.join("archive"))
            .expect("artifact layout");
        let run_paths = layout
            .run("run-live", "task-live")
            .expect("run artifact paths");
        let lane_paths = run_paths.lane("lane-live").expect("lane artifact paths");
        let config = runtime_review_config(&root);
        let mut task_spec = live_task_spec(&config);
        task_spec.max_total_rounds = 3;
        let allocation = HepaWorktreeAllocation {
            lane_id: "lane-live".to_string(),
            branch: "hepa/lane-live".to_string(),
            worktree_path: root.join("worktree"),
            base_commit: "base".to_string(),
            metadata_path: root.join("worktree/.hepa-worktree.json"),
        };
        let failed_validation = HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Failed,
            commands: vec![HepaValidationCommandResult {
                command: "git diff --check".to_string(),
                exit_code: 1,
                duration_ms: 10,
            }],
            no_tests_detected: false,
            failure_type: Some("validation_failed".to_string()),
            summary: vec!["`git diff --check` exited 1.".to_string()],
        };

        let error = run_live_repair_round(RunLiveRepairInput {
            config: &config,
            lane_paths: &lane_paths,
            allocation: &allocation,
            task_spec: &task_spec,
            spec: dummy_pi_spec(),
            adapter_id: "pi",
            environment: std::collections::BTreeMap::new(),
            prior_prompt: "Update README.md".to_string(),
            failed_validation,
            review_findings: Vec::new(),
            repair_round: 4,
            first_changed_files: vec!["README.md".to_string()],
        })
        .expect_err("fourth total round should block before worker launch");

        assert!(error.contains("repair budget blocked round 4"));
        let budget = fs::read_to_string(lane_paths.lane_dir.join("repair/round-4-budget.json"))
            .expect("round 4 budget artifact");
        assert!(budget.contains("\"max_total_attempts\": 3"));
        assert!(budget.contains("\"allowed\": false"));
        assert!(
            !lane_paths
                .lane_dir
                .join("repair/round-4-prompt.md")
                .exists()
        );

        remove_test_dir(root);
    }

    #[test]
    fn live_timing_records_bounded_repair_round() {
        let config = HepaFakeRunConfig {
            repo_path: PathBuf::from("repo"),
            control_root: PathBuf::from("control"),
            worktree_root: PathBuf::from("worktrees"),
            archive_root: PathBuf::from("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update AGENTS.md and run git diff --check.".to_string(),
            timing: true,
        };

        let timing = live_timing_record(LiveTimingInput {
            config: &config,
            adapter_id: "pi",
            worker_duration_seconds: 8.0,
            validation_duration_seconds: 0.2,
            review_duration_seconds: 0.3,
            reviewer_passes: 2,
            terminal_phase: LivePipelinePhase::Completed,
            repair_timing: Some(LiveRepairTiming {
                brief_duration_seconds: 0.1,
                worker_duration_seconds: 5.0,
                validation_duration_seconds: 0.2,
                completed: true,
            }),
        });

        assert_eq!(timing.counters.agent_loops, 2);
        assert_eq!(timing.counters.manager_passes, 2);
        assert!(
            timing
                .phases
                .iter()
                .any(|phase| phase.name == "live_repair_brief" && phase.round == Some(2))
        );
        assert!(
            timing
                .phases
                .iter()
                .any(|phase| phase.name == "live_repair_worker" && phase.round == Some(2))
        );
        assert!(
            timing
                .phases
                .iter()
                .any(|phase| phase.name == "live_repair_validation" && phase.round == Some(2))
        );
        let first_validation = timing
            .phases
            .iter()
            .find(|phase| phase.name == "live_validation")
            .expect("round-1 validation phase");
        assert_eq!(first_validation.status, HepaPhaseStatus::Failed);
    }

    #[test]
    fn live_review_fanout_records_arbitration_for_real_diff() {
        let root = unique_test_dir("review-fanout-arbitration");
        let layout = HepaArtifactLayout::new(root.join("control"), root.join("archive"))
            .expect("artifact layout");
        let run_paths = layout
            .run("run-live", "task-live")
            .expect("run artifact paths");
        let lane_paths = run_paths.lane("lane-live").expect("lane artifact paths");
        let config = HepaFakeRunConfig {
            repo_path: PathBuf::from("repo"),
            control_root: PathBuf::from("control"),
            worktree_root: PathBuf::from("worktrees"),
            archive_root: PathBuf::from("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        };
        let validation = HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Passed,
            commands: vec![HepaValidationCommandResult {
                command: "git diff --check".to_string(),
                exit_code: 0,
                duration_ms: 7,
            }],
            no_tests_detected: false,
            failure_type: None,
            summary: vec!["`git diff --check` exited 0.".to_string()],
        };

        let outcome = live_deterministic_review_fanout(
            &config,
            &lane_paths,
            "pi",
            &["README.md".to_string()],
            &validation,
            "diff --git a/README.md b/README.md",
        )
        .expect("fanout should pass");

        assert!(outcome.staging_allowed);
        assert_eq!(outcome.reviewer_passes, 2);
        assert_eq!(outcome.signals.len(), 2);
        assert!(
            outcome
                .signals
                .iter()
                .all(|signal| signal.status == HepaReviewStatus::Approved)
        );
        assert!(
            outcome
                .signals
                .iter()
                .all(|signal| signal.adapter_id.starts_with("hepa-reviewer:"))
        );
        assert!(outcome.arbitration.records.len() >= 3);
        let dispositions = outcome
            .arbitration
            .records
            .iter()
            .map(|record| record.disposition.as_str())
            .collect::<Vec<_>>();
        assert!(dispositions.contains(&"manager_accepted"));
        assert!(dispositions.contains(&"manager_rejected"));
        assert!(dispositions.contains(&"downgraded"));
        assert!(outcome.arbitration.card_status.contains("arbitration="));
        assert!(outcome.blockers.is_empty());
        remove_test_dir(root);
    }

    #[test]
    fn live_review_prompt_requires_json_and_preserves_git_lifecycle() {
        let config = HepaFakeRunConfig {
            repo_path: PathBuf::from("repo"),
            control_root: PathBuf::from("control"),
            worktree_root: PathBuf::from("worktrees"),
            archive_root: PathBuf::from("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        };
        let validation = HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Passed,
            commands: vec![HepaValidationCommandResult {
                command: "git diff --check".to_string(),
                exit_code: 0,
                duration_ms: 7,
            }],
            no_tests_detected: false,
            failure_type: None,
            summary: vec!["`git diff --check` exited 0.".to_string()],
        };

        let prompt = live_review_prompt(
            &config,
            &["README.md".to_string()],
            &validation,
            "diff --git a/README.md b/README.md",
        );

        assert!(prompt.contains("Return only a JSON object"));
        assert!(prompt.contains("\"status\":\"approved|changes_requested|blocked|failed\""));
        assert!(prompt.contains("Do not create commits"));
        assert!(prompt.contains("pull requests"));
    }

    #[test]
    fn live_review_prompt_adds_no_think_for_local_qwen_review_model() {
        let environment = std::collections::BTreeMap::from([
            (
                "HEPA_PI_MODEL".to_string(),
                "deepseek/deepseek-chat".to_string(),
            ),
            (
                "HEPA_PI_REVIEW_MODEL".to_string(),
                "local/mlx-community/Qwen3-30B-A3B-4bit".to_string(),
            ),
            (
                "HEPA_PI_BASE_URL".to_string(),
                "http://127.0.0.1:52415/v1".to_string(),
            ),
        ]);

        assert!(pi_review_model_needs_no_think_suffix(&environment));
    }

    #[test]
    fn local_pi_generation_permit_applies_to_local_worker_and_reviewer_models() {
        let local_environment = std::collections::BTreeMap::from([
            (
                "HEPA_PI_MODEL".to_string(),
                "local/mlx-community/Qwen3-30B-A3B-4bit".to_string(),
            ),
            (
                "HEPA_PI_REVIEW_MODEL".to_string(),
                "local/mlx-community/Qwen3-30B-A3B-4bit".to_string(),
            ),
            (
                "HEPA_PI_BASE_URL".to_string(),
                "http://127.0.0.1:52415/v1".to_string(),
            ),
        ]);
        let cloud_environment = std::collections::BTreeMap::from([(
            "HEPA_PI_REVIEW_MODEL".to_string(),
            "deepseek/deepseek-chat".to_string(),
        )]);
        let hybrid_environment = std::collections::BTreeMap::from([
            (
                "HEPA_PI_MODEL".to_string(),
                "local/mlx-community/Qwen3-30B-A3B-4bit".to_string(),
            ),
            (
                "HEPA_PI_REVIEW_MODEL".to_string(),
                "deepseek/deepseek-chat".to_string(),
            ),
            (
                "HEPA_PI_BASE_URL".to_string(),
                "http://127.0.0.1:52415/v1".to_string(),
            ),
        ]);

        let permit = local_pi_generation_concurrency_permit(
            "pi",
            &local_environment,
            HepaAdapterRole::Worker,
        )
        .expect("permit decision");
        assert!(permit.is_some());
        drop(permit);
        let permit = local_pi_generation_concurrency_permit(
            "pi",
            &local_environment,
            HepaAdapterRole::Reviewer,
        )
        .expect("permit decision");
        assert!(permit.is_some());
        drop(permit);
        let permit = local_pi_generation_concurrency_permit(
            "pi",
            &hybrid_environment,
            HepaAdapterRole::Worker,
        )
        .expect("permit decision");
        assert!(permit.is_some());
        drop(permit);
        assert!(
            local_pi_generation_concurrency_permit(
                "pi",
                &hybrid_environment,
                HepaAdapterRole::Reviewer
            )
            .expect("permit decision")
            .is_none()
        );
        assert!(
            local_pi_generation_concurrency_permit(
                "pi",
                &cloud_environment,
                HepaAdapterRole::Worker
            )
            .expect("permit decision")
            .is_none()
        );
        assert!(
            local_pi_generation_concurrency_permit(
                "custom",
                &local_environment,
                HepaAdapterRole::Worker
            )
            .expect("permit decision")
            .is_none()
        );
    }

    #[test]
    fn local_pi_monitor_stop_with_changes_can_continue_only_for_local_pi() {
        let local_environment = std::collections::BTreeMap::from([(
            "HEPA_PI_MODEL".to_string(),
            "llama-cpp/devstral-small-24b".to_string(),
        )]);
        let cloud_environment = std::collections::BTreeMap::from([(
            "HEPA_PI_MODEL".to_string(),
            "deepseek/deepseek-chat".to_string(),
        )]);
        let stall_error = hepa_adapters::engine::HepaAdapterExecutionError {
            field: "monitor".to_string(),
            message: "stall: stall budget exceeded".to_string(),
            status: Some("blocked".to_string()),
            stdout: "partial local output".to_string(),
            stderr: String::new(),
        };
        let command_error = hepa_adapters::engine::HepaAdapterExecutionError {
            field: "command".to_string(),
            message: "failed".to_string(),
            status: None,
            stdout: String::new(),
            stderr: String::new(),
        };

        assert!(pi_local_monitor_stop_with_changed_files_can_continue(
            "pi",
            &local_environment,
            &stall_error,
            &["src/example.ts".to_string()]
        ));
        assert!(!pi_local_monitor_stop_with_changed_files_can_continue(
            "pi",
            &local_environment,
            &stall_error,
            &[]
        ));
        assert!(!pi_local_monitor_stop_with_changed_files_can_continue(
            "pi",
            &cloud_environment,
            &stall_error,
            &["src/example.ts".to_string()]
        ));
        assert!(!pi_local_monitor_stop_with_changed_files_can_continue(
            "custom",
            &local_environment,
            &stall_error,
            &["src/example.ts".to_string()]
        ));
        assert!(!pi_local_monitor_stop_with_changed_files_can_continue(
            "pi",
            &local_environment,
            &command_error,
            &["src/example.ts".to_string()]
        ));
    }

    #[test]
    fn live_pi_exo_mlx_route_blocks_before_adapter_invocation() {
        let root = unique_test_dir("live-pi-exo-mlx-preflight");
        let repo = root.join("repo");
        init_repo(&repo);
        let config = HepaFakeRunConfig {
            repo_path: repo.clone(),
            control_root: root.join("control"),
            worktree_root: root.join("worktrees"),
            archive_root: root.join("archive"),
            run_id: "run-local-preflight".to_string(),
            task_id: "task-local-preflight".to_string(),
            lane_id: "lane-local-preflight".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        };
        let layout =
            HepaArtifactLayout::new(&config.control_root, &config.archive_root).expect("layout");
        let lane_paths = layout
            .run(&config.run_id, &config.task_id)
            .expect("run paths")
            .lane(&config.lane_id)
            .expect("lane paths");
        fs::create_dir_all(&lane_paths.lane_dir).expect("lane dir");
        let allocator = HepaWorktreeAllocator::new(&config.repo_path, &config.worktree_root);
        let allocation = allocator
            .allocate_lane_with_metadata(&config.lane_id, "2026-06-16T00:00:00Z")
            .expect("allocation");
        let fake_pi = root.join("pi-should-not-run");
        write_executable(
            &fake_pi,
            "#!/usr/bin/env sh\necho adapter-invoked >&2\nexit 42\n",
        );
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "pi".to_string(),
            display_name: "Pi Coding Agent".to_string(),
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: format!("{} --mode json", fake_pi.display()),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: vec![fake_pi.to_string_lossy().to_string()],
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::None,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["local-only".to_string()],
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 1,
            prompt_transport: HepaAdapterPromptTransport::Stdin,
            output_capture: HepaAdapterOutputCapture::Stdout,
        };

        let error = match execute_live_worker_attempt(ExecuteLiveAttemptInput {
            config: &config,
            lane_paths: &lane_paths,
            allocation: &allocation,
            spec,
            adapter_id: "pi",
            environment: std::collections::BTreeMap::from([
                (
                    "HEPA_PI_MODEL".to_string(),
                    "local/mlx-community/Qwen3-30B-A3B-4bit".to_string(),
                ),
                (
                    "HEPA_PI_BASE_URL".to_string(),
                    "http://127.0.0.1:52415/v1".to_string(),
                ),
            ]),
            attempt_id: "attempt-1",
            round: 1,
            prompt: "fixture prompt".to_string(),
            started_at: "2026-06-16T00:00:02Z",
            completed_at: "2026-06-16T00:00:03Z",
        }) {
            Ok(_) => panic!("unsupported local route must fail before invoking Pi"),
            Err(error) => error,
        };

        assert!(error.contains("local_tool_calling_unsupported"));
        assert!(error.contains("llama.cpp"));
        assert!(error.contains("--jinja"));
        let stderr_log = fs::read_to_string(
            lane_paths
                .attempt("attempt-1")
                .expect("attempt paths")
                .attempt_dir
                .join("stderr.log"),
        )
        .expect("stderr log");
        assert!(!stderr_log.contains("adapter-invoked"));
        let attempt_json = fs::read_to_string(
            lane_paths
                .attempt("attempt-1")
                .expect("attempt paths")
                .attempt_dir
                .join("attempt.json"),
        )
        .expect("attempt report");
        assert!(attempt_json.contains("\"status\": \"blocked\""));
        assert!(attempt_json.contains("local_tool_calling_unsupported"));

        allocator
            .cleanup_lane(&config.lane_id, "2026-06-16T00:00:09Z")
            .expect("cleanup");
        remove_test_dir(root);
    }

    #[test]
    fn adapter_review_payload_extracts_json_from_pi_final_message() {
        let root = unique_test_dir("adapter-review-payload");
        let output_file = root.join("review.jsonl");
        let stdout = r#"{"type":"agent_start"}
{"type":"agent_end","message":{"content":"Here is the review:\n{\"status\":\"approved\",\"summary\":[\"ok\"],\"findings\":[]}"}}"#;

        let payload = adapter_review_payload("pi", stdout, &output_file)
            .expect("Pi final JSON should extract");

        assert_eq!(
            payload,
            "{\"status\":\"approved\",\"summary\":[\"ok\"],\"findings\":[]}"
        );
        remove_test_dir(root);
    }

    #[test]
    fn live_controlled_review_block_requires_human_escalation_before_staging() {
        let root = unique_test_dir("controlled-review-block");
        let layout = HepaArtifactLayout::new(root.join("control"), root.join("archive"))
            .expect("artifact layout");
        let run_paths = layout
            .run("run-live", "task-live")
            .expect("run artifact paths");
        let lane_paths = run_paths.lane("lane-live").expect("lane artifact paths");
        let config = HepaFakeRunConfig {
            repo_path: PathBuf::from("repo"),
            control_root: PathBuf::from("control"),
            worktree_root: PathBuf::from("worktrees"),
            archive_root: PathBuf::from("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        };
        let validation = HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Passed,
            commands: vec![HepaValidationCommandResult {
                command: "git diff --check".to_string(),
                exit_code: 0,
                duration_ms: 7,
            }],
            no_tests_detected: false,
            failure_type: None,
            summary: vec!["`git diff --check` exited 0.".to_string()],
        };
        let outcome = live_review_fanout_with_controlled_block(
            &config,
            "pi",
            &["README.md".to_string()],
            &validation,
            "diff --git a/README.md b/README.md",
            &lane_paths,
            true,
        )
        .expect("fanout should produce a controlled block");

        assert!(!outcome.staging_allowed);
        assert_eq!(outcome.reviewer_passes, 2);
        assert!(
            outcome
                .signals
                .iter()
                .all(|signal| signal.status == HepaReviewStatus::Blocked)
        );
        assert_eq!(outcome.arbitration.status, "manager_required");
        assert!(
            outcome.blockers.iter().any(|blocker| {
                blocker.contains("manager arbitration is required before staging")
            })
        );
        assert!(outcome.signals.iter().any(|signal| {
            signal
                .summary
                .iter()
                .any(|line| line.contains("inspect reviewer evidence"))
        }));
        remove_test_dir(root);
    }

    #[test]
    fn review_blocked_run_projects_blocked_reason_to_hermes_card_payload() {
        let config = HepaFakeRunConfig {
            repo_path: PathBuf::from("repo"),
            control_root: PathBuf::from("control"),
            worktree_root: PathBuf::from("worktrees"),
            archive_root: PathBuf::from("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        };
        let reason = "Live review fanout blocked the lane; staging and PR were not attempted: rs5-review-blocked-escalation-primary: manager arbitration is required before staging; review fanout policy required 2 approvals but received 0";
        let validation = HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Passed,
            commands: vec![HepaValidationCommandResult {
                command: "git diff --check".to_string(),
                exit_code: 0,
                duration_ms: 7,
            }],
            no_tests_detected: false,
            failure_type: None,
            summary: vec!["`git diff --check` exited 0.".to_string()],
        };
        let signal = live_controlled_review_block_signal(
            "lane-live",
            "hepa-reviewer:fallback-primary:pi",
            "task-live",
            &["README.md".to_string()],
        );
        let decision = arbitrate_live_finding(signal.findings[0].clone())
            .expect("controlled finding should require manager arbitration");
        let arbitration = summarize_arbitration_results(&[decision])
            .expect("arbitration summary should serialize to card");
        let timing = live_timing_record(LiveTimingInput {
            config: &config,
            adapter_id: "pi",
            worker_duration_seconds: 1.0,
            validation_duration_seconds: 0.1,
            review_duration_seconds: 0.1,
            reviewer_passes: 1,
            terminal_phase: LivePipelinePhase::ReviewFailed,
            repair_timing: None,
        });
        let task = HepaFleetTask {
            status: HepaTaskStatus::Blocked,
            readiness: HepaReadinessState::Blocked,
            external_card_id: Some("hermes-card-rs5-3".to_string()),
            ..fleet_task(&config)
        };
        let lane = HepaLane {
            state: HepaLaneState::Blocked,
            adapter_id: "pi".to_string(),
            attempt_count: 1,
            updated_at: "2026-06-16T00:00:05Z".to_string(),
            ..completed_lane(&config)
        };
        let readiness = HepaReadinessResult {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: config.task_id.clone(),
            status: HepaReadinessStatus::Blocked,
            blockers: vec![reason.to_string()],
            questions: Vec::new(),
            checked_at: "2026-06-16T00:00:05Z".to_string(),
        };
        let terminal_report = HepaTerminalTaskReport {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: config.task_id.clone(),
            lane_id: config.lane_id.clone(),
            status: HepaTerminalStatus::Blocked,
            pr_url: None,
            validation: Some(validation.clone()),
            review_signals: vec![signal.clone()],
            arbitration: Some(arbitration),
            timing: Some(timing.clone()),
            summary: vec![reason.to_string()],
            human_attention_required: true,
            completed_at: "2026-06-16T00:00:05Z".to_string(),
        };

        let payload = map_task_to_hermes_card(&HepaHermesCardMappingInput {
            project: HepaProject {
                schema_version: CONTRACT_SCHEMA_VERSION,
                project_id: "project-1".to_string(),
                display_name: "HEPA Fixture Project".to_string(),
                repo_ref: "<TARGET_REPO>".to_string(),
                default_branch: "main".to_string(),
                routing_policy_ref: None,
                is_active: true,
                created_at: "2026-06-16T00:00:00Z".to_string(),
                updated_at: "2026-06-16T00:00:05Z".to_string(),
            },
            task_spec: live_task_spec(&config),
            task,
            lanes: vec![lane],
            readiness: Some(readiness),
            validation: Some(validation),
            review_signals: vec![signal],
            terminal_report: Some(terminal_report),
            timing: Some(timing),
            steering_records: Vec::new(),
            blocked_questions: Vec::new(),
        })
        .expect("blocked review should project to card");

        assert_eq!(
            payload.fields.get("task_status"),
            Some(&HepaHermesFieldValue::Text("blocked".to_string()))
        );
        assert_eq!(
            payload.fields.get("readiness_state"),
            Some(&HepaHermesFieldValue::Text("blocked".to_string()))
        );
        assert_eq!(
            payload.fields.get("terminal_status"),
            Some(&HepaHermesFieldValue::Text("blocked".to_string()))
        );
        assert!(payload.comments.iter().any(|comment| {
            comment.kind == HepaHermesCommentKind::Readiness
                && comment
                    .body
                    .contains("manager arbitration is required before staging")
        }));
        assert!(payload.comments.iter().any(|comment| comment.kind
            == HepaHermesCommentKind::TerminalReport
            && comment.body.contains("Human attention required: true")));
    }

    #[test]
    fn live_validation_output_redacts_lane_runtime_paths() {
        let worktree = PathBuf::from("/tmp/hepa-validation/.hepa/worktrees/lane-cli-fake");
        let output = format!(
            "RUN v4.1.8 {}\nok src/app/views/login-and-registration/login-form.test.tsx",
            worktree.display()
        );
        let sanitized = sanitize_validation_output(&worktree, &output);

        assert!(sanitized.contains("<VALIDATION_RUNTIME>"));
        assert!(!sanitized.contains("/tmp/hepa-validation"));
        assert!(!sanitized.contains(".hepa/worktrees/lane-cli-fake"));
    }

    #[test]
    fn validation_stream_events_record_manager_summaries() {
        let root = unique_test_dir("validation-stream");
        let layout = HepaArtifactLayout::new(root.join("control"), root.join("archive"))
            .expect("artifact layout");
        let run_paths = layout
            .run("run-live", "task-live")
            .expect("run artifact paths");
        let lane_paths = run_paths.lane("lane-live").expect("lane artifact paths");
        let passed = HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Passed,
            commands: vec![HepaValidationCommandResult {
                command: "git diff --check".to_string(),
                exit_code: 0,
                duration_ms: 7,
            }],
            no_tests_detected: false,
            failure_type: None,
            summary: vec!["`git diff --check` exited 0.".to_string()],
        };
        let mut failed = passed.clone();
        failed.status = HepaValidationStatus::Failed;
        failed.failure_type = Some("validation_failed".to_string());
        failed.summary = vec!["`cargo test` exited 1.".to_string()];

        append_validation_stream_event(&lane_paths, 1, &passed).expect("round 1 stream");
        append_validation_stream_event(&lane_paths, 2, &failed).expect("round 2 stream");

        let stream = fs::read_to_string(
            lane_paths
                .lane_dir
                .join("streams/manager-validation-stream.jsonl"),
        )
        .expect("validation stream");
        assert!(stream.contains("\"source\":\"hepa-manager\""));
        assert!(stream.contains("\"event\":\"validation_summary\""));
        assert!(stream.contains("\"round\":1"));
        assert!(stream.contains("\"round\":2"));
        assert!(stream.contains("\"failure_type\":\"validation_failed\""));

        remove_test_dir(root);
    }

    #[test]
    fn tool_summary_stream_events_redact_and_bound_model_visible_summary() {
        let root = unique_test_dir("tool-summary-stream");
        let layout = HepaArtifactLayout::new(root.join("control"), root.join("archive"))
            .expect("artifact layout");
        let run_paths = layout
            .run("run-live", "task-live")
            .expect("run artifact paths");
        let lane_paths = run_paths.lane("lane-live").expect("lane artifact paths");
        let long_tail = " keep-going".repeat(60);
        let parsed = HepaPiParsedOutput {
            final_message: format!(
                "Edited README.md, used Bearer hepa-test-token-12345, and ran validation.{long_tail} tail-marker"
            ),
            tool_activity: vec![
                "tool_call".to_string(),
                "tool_result".to_string(),
                "tool_call".to_string(),
            ],
        };

        append_tool_summary_stream_event(&lane_paths, 1, &parsed).expect("tool summary stream");

        let stream = fs::read_to_string(
            lane_paths
                .lane_dir
                .join("streams/manager-tool-summary-stream.jsonl"),
        )
        .expect("tool summary stream");
        assert!(stream.contains("\"event\":\"tool_activity_summary\""));
        assert!(stream.contains("\"tool_event_count\":3"));
        assert!(stream.contains("\"tool_call\""));
        assert!(stream.contains("\"tool_result\""));
        assert!(stream.contains("\"final_message_bytes\":"));
        assert!(stream.contains("\"final_message_preview\""));
        assert!(stream.contains("Edited README"));
        assert!(!stream.contains("hepa-test-token-12345"));
        assert!(!stream.contains("tail-marker"));

        remove_test_dir(root);
    }

    #[test]
    fn live_diff_falls_back_to_status_for_untracked_repair_evidence() {
        let root = unique_test_dir("live-diff-untracked");
        let repo = root.join("repo");
        init_repo(&repo);
        fs::write(repo.join("new-status-matrix.md"), "# Status\n").expect("new file");

        let diff = collect_live_diff(&repo).expect("diff fallback should be available");

        assert!(diff.contains("No tracked diff captured. Git status:"));
        assert!(diff.contains("?? new-status-matrix.md"));
        remove_test_dir(root);
    }

    #[test]
    fn live_attempt_summary_redacts_pi_event_paths() {
        let worktree = PathBuf::from("/tmp/hepa-validation/.hepa/worktrees/lane-cli-fake");
        let stdout = format!(
            "{{\"type\":\"session\",\"cwd\":\"{}\"}}\n{{\"type\":\"agent_start\"}}",
            worktree.display()
        );
        let summary = live_attempt_summary(&worktree, &stdout, "");

        assert_eq!(summary.len(), 2);
        assert!(summary[0].contains("<VALIDATION_RUNTIME>"));
        assert!(!summary[0].contains("/tmp/hepa-validation"));
        assert!(!summary[0].contains(".hepa/worktrees/lane-cli-fake"));
    }

    #[test]
    fn live_usage_entries_write_lane_cost_report() {
        let root = unique_test_dir("live-cost");
        let control = root.join("control");
        let archive = root.join("archive");
        let layout = HepaArtifactLayout::new(&control, &archive).expect("layout");
        let lane_paths = layout
            .run("run-1", "task-1")
            .expect("run")
            .lane("lane-1")
            .expect("lane");
        let config = HepaFakeRunConfig {
            repo_path: root.join("repo"),
            control_root: control,
            worktree_root: root.join("worktrees"),
            archive_root: archive,
            run_id: "run-1".to_string(),
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            task_text: "Update docs".to_string(),
            timing: true,
        };
        let output_file = root.join("adapter-output.json");
        fs::create_dir_all(&root).expect("root dir");
        fs::write(
            &output_file,
            r#"{"usage":{"input_tokens":12,"output_tokens":3,"total_tokens":15,"cost_micros":99,"currency":"USD"}}"#,
        )
        .expect("adapter output");
        let entries = extract_live_usage_entries(
            "pi",
            "attempt-1",
            &hepa_adapters::spec::HepaAdapterCostClass::PaidCloud,
            &HepaAdapterOutputCapture::AdapterFile,
            &output_file,
            "",
        )
        .expect("usage should extract");

        write_lane_cost_report_if_present(&config, &lane_paths, &entries)
            .expect("cost report should write");

        let cost_json = fs::read_to_string(lane_paths.cost_report).expect("cost artifact");
        assert!(cost_json.contains("\"total_tokens\": 15"));
        assert!(cost_json.contains("\"total_cost_micros\": 99"));
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

    fn runtime_review_config(root: &Path) -> HepaFakeRunConfig {
        HepaFakeRunConfig {
            repo_path: root.join("repo"),
            control_root: root.join("control"),
            worktree_root: root.join("worktrees"),
            archive_root: root.join("archive"),
            run_id: "run-live".to_string(),
            task_id: "task-live".to_string(),
            lane_id: "lane-live".to_string(),
            task_text: "Update README.md".to_string(),
            timing: true,
        }
    }

    fn runtime_review_paths(
        config: &HepaFakeRunConfig,
    ) -> (
        hepa_core::artifacts::HepaLaneArtifactPaths,
        HepaWorktreeAllocation,
    ) {
        let layout =
            HepaArtifactLayout::new(&config.control_root, &config.archive_root).expect("layout");
        let lane_paths = layout
            .run(&config.run_id, &config.task_id)
            .expect("run paths")
            .lane(&config.lane_id)
            .expect("lane paths");
        let allocation = HepaWorktreeAllocation {
            lane_id: config.lane_id.clone(),
            branch: "hepa/lane-live".to_string(),
            worktree_path: config.worktree_root.join("lane-live"),
            base_commit: "base".to_string(),
            metadata_path: config.worktree_root.join("lane-live/.hepa-worktree.json"),
        };
        (lane_paths, allocation)
    }

    fn dummy_pi_spec() -> HepaAdapterSpec {
        HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "pi".to_string(),
            display_name: "Pi Coding Agent".to_string(),
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: "unused".to_string(),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: Vec::new(),
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::None,
            supports_resume: false,
            supports_json_output: true,
            capabilities: Vec::new(),
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 1,
            prompt_transport: HepaAdapterPromptTransport::Stdin,
            output_capture: HepaAdapterOutputCapture::Stdout,
        }
    }

    fn passed_validation() -> HepaValidationSummary {
        HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Passed,
            commands: Vec::new(),
            no_tests_detected: false,
            failure_type: None,
            summary: vec!["Validation passed.".to_string()],
        }
    }

    fn write_executable(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("script dir");
        }
        fs::write(path, contents).expect("script write");
        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod");
    }

    fn write_fake_pi_empty_output(path: &Path) {
        write_executable(
            path,
            "#!/usr/bin/env sh\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"agent_start\"}' '{\"type\":\"agent_end\",\"messages\":[{\"role\":\"assistant\",\"content\":[]}]}'\n",
        );
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

    struct ScopedEnv {
        _guard: MutexGuard<'static, ()>,
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl ScopedEnv {
        fn set_many(changes: &[(&'static str, Option<&str>)]) -> Self {
            static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let guard = ENV_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("env lock");
            let mut previous = Vec::new();
            for (key, value) in changes {
                previous.push((*key, std::env::var(key).ok()));
                // Tests serialize environment mutation with ENV_LOCK.
                unsafe {
                    if let Some(value) = value {
                        std::env::set_var(key, value);
                    } else {
                        std::env::remove_var(key);
                    }
                }
            }
            Self {
                _guard: guard,
                previous,
            }
        }
    }

    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            for (key, previous) in self.previous.iter().rev() {
                if let Some(previous) = previous {
                    // Tests serialize environment mutation with ENV_LOCK.
                    unsafe { std::env::set_var(key, previous) };
                } else {
                    // Tests serialize environment mutation with ENV_LOCK.
                    unsafe { std::env::remove_var(key) };
                }
            }
        }
    }
}
