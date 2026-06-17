use hepa_adapters::{
    engine::{HepaOneshotAdapterExecutor, HepaOneshotAdapterInvocation},
    fake::{HepaFakeAdapter, HepaFakeReviewerInput, HepaFakeWorkerInput},
    pi::parse_pi_json_events,
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
        HepaFindingSeverity, HepaFleetTask, HepaLane, HepaLaneState, HepaPhaseStatus,
        HepaReadinessResult, HepaReadinessState, HepaReadinessStatus, HepaReviewFinding,
        HepaReviewSignal, HepaReviewStatus, HepaRiskLevel, HepaTaskSpec, HepaTaskStatus,
        HepaTerminalStatus, HepaTerminalTaskReport, HepaTimingCounters, HepaTimingPhase,
        HepaTimingRecord, HepaValidate, HepaValidationCommandResult, HepaValidationStatus,
        HepaValidationSummary,
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
        HepaSystemProcessRunner, build_pr_body,
    },
    staging::HepaSafeStaging,
    worktree::{HepaWorktreeAllocation, HepaWorktreeAllocator},
};
#[cfg(test)]
use hepa_kanban::{
    card_mapping::HepaHermesCardMappingInput,
    sync::{HepaHermesCardStore, HepaKanbanSyncEngine, HepaKanbanSyncSummary},
};
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
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
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

pub fn run_live_task(
    config: &HepaFakeRunConfig,
    adapter_id: &str,
) -> Result<HepaFakeRunResult, String> {
    validate_config(config)?;
    let task_spec = live_task_spec(config);
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
    let prompt =
        live_worker_prompt_for_adapter(&sanitized_task_text(config), adapter_id, &live_config);
    let mut repair_timing = None;
    let mut attempt_outcome = execute_live_worker_attempt(ExecuteLiveAttemptInput {
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
    })?;
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
    let review_outcome = live_review_fanout(
        config,
        adapter_id,
        &spec,
        &environment,
        &lane_paths,
        &allocation,
        &attempt_outcome.changed_files,
        &validation,
        &diff_context,
    )?;
    let review_duration_seconds = review_started.elapsed().as_secs_f64();
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
    if !review_outcome.staging_allowed {
        transition_and_record(
            &lane_paths,
            &mut lane,
            7,
            HepaLaneState::Blocked,
            "live review fanout blocked",
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
                "Live review fanout blocked the lane; staging and PR were not attempted: {}",
                review_outcome.blockers.join("; ")
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
        &HepaCommitMessage::new(format!(
            "hepa: {}",
            commit_title(&sanitized_task_text(config))
        ))
        .with_body(vec![
            format!("Task: {}", sanitized_task_text(config)),
            format!("Run: {}", config.run_id),
            format!("Lane: {}", config.lane_id),
            "Manager-owned commit created by HEPA live pipeline.".to_string(),
        ]),
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
    let pr_body = build_pr_body(&HepaPrBodyInput {
        task_spec: &task_spec,
        terminal_report: &terminal_report,
        lane: &lane,
        external_card_id: None,
    });
    let branch = lane.branch.clone();
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
    let pr_request = HepaPrRequest {
        title: format!(
            "HEPA validation: {}",
            commit_title(&sanitized_task_text(config))
        ),
        body: pr_body,
        base_branch: "main".to_string(),
        head_branch: branch.clone(),
    };
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
        environment,
        monitor_policy: live_monitor_policy(),
    };
    let worker_started = Instant::now();
    let result = HepaOneshotAdapterExecutor::new()
        .run(&invocation)
        .map_err(|error| error.to_string())?;
    let duration_seconds = worker_started.elapsed().as_secs_f64();
    let changed_files = collect_changed_files(&allocation.worktree_path)?;
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
    first_changed_files: Vec<String>,
}

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
        first_changed_files,
    } = input;
    let policy = HepaRepairRoundPolicy {
        max_repair_rounds: 2,
        max_total_attempts: 2,
    };
    let repair_round = 2;
    let decision = enforce_repair_round_budget(
        policy.clone(),
        HepaRepairRoundState {
            next_repair_round: repair_round,
            total_attempts_after_next: 2,
        },
    )
    .map_err(|error| format!("repair budget invalid: {}: {}", error.field, error.message))?;
    let repair_dir = lane_paths.lane_dir.join("repair");
    fs::create_dir_all(&repair_dir).map_err(|error| error.to_string())?;
    write_json(
        &repair_dir.join("round-2-budget.json"),
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
            "repair budget blocked round 2: {}",
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
    let brief = rewrite_repair_prompt_from_evidence(HepaRepairBriefInput {
        lane_id: config.lane_id.clone(),
        repair_round,
        prior_prompt,
        failing_commands,
        review_findings: Vec::new(),
        diff_state,
        files_touched: first_changed_files,
    })
    .map_err(|error| format!("repair brief invalid: {}: {}", error.field, error.message))?;
    write_json(
        &repair_dir.join("round-2-brief.json"),
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
    fs::write(repair_dir.join("round-2-prompt.md"), &repair_prompt)
        .map_err(|error| error.to_string())?;
    let attempt = execute_live_worker_attempt(ExecuteLiveAttemptInput {
        config,
        lane_paths,
        allocation,
        spec,
        adapter_id,
        environment,
        attempt_id: "attempt-2",
        round: repair_round,
        prompt: repair_prompt,
        started_at: "2026-06-16T00:00:04Z",
        completed_at: "2026-06-16T00:00:05Z",
    })?;
    let validation_started = Instant::now();
    let validation = run_live_validation(&allocation.worktree_path, task_spec);
    let validation_duration_seconds = validation_started.elapsed().as_secs_f64();
    write_json(&repair_dir.join("round-2-validation.json"), &validation)
        .map_err(|error| error.to_string())?;
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

fn live_monitor_policy() -> HepaMonitorPolicy {
    HepaMonitorPolicy {
        timeout_ms: std::env::var("HEPA_PI_LIVE_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .or(Some(300_000)),
        stall_ms: std::env::var("HEPA_PI_LIVE_STALL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .or(Some(240_000)),
        ..HepaMonitorPolicy::default()
    }
}

fn live_worker_prompt(task_text: &str) -> String {
    format!(
        "You are HEPA's live stress-test worker.\n\nTask:\n{task_text}\n\nRepository worktree: current directory.\n\nExecution rules:\n- You are already running inside the lane worktree.\n- Make only the changes needed to satisfy the task.\n- Use relative paths when reading or editing files.\n- Do not create commits, branches, tags, pull requests, or Git remotes; HEPA owns the Git lifecycle.\n- Do not read or print provider keys, credentials, or unrelated local files.\n- Run the smallest relevant validation command requested by the task when practical.\n- Finish by reporting changed files, validation results, and any blockers.\n",
    )
}

fn live_worker_prompt_for_adapter(
    task_text: &str,
    adapter_id: &str,
    config: &hepa_core::config::HepaConfig,
) -> String {
    let mut prompt = live_worker_prompt(task_text);
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
    if adapter_id == "pi" && pi_model_needs_no_think_suffix(&config.pi.model, &config.pi.base_url) {
        prompt.push_str(
            "\nAdapter-local model note: answer directly and do not emit hidden reasoning. /no_think\n",
        );
    }
    prompt
}

fn pi_model_needs_no_think_suffix(model: &str, base_url: &Option<String>) -> bool {
    let model = model.to_ascii_lowercase();
    let is_qwen = model.contains("qwen");
    let is_local = model.starts_with("local/")
        || model.starts_with("ollama/")
        || model.starts_with("lmstudio/")
        || model.starts_with("vllm/")
        || model.starts_with("mlx-community/")
        || base_url.as_deref().is_some_and(is_loopback_url);
    is_qwen && is_local
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
        max_total_rounds: 1,
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
    let mut commands = Vec::new();
    for command in [
        "pnpm install --frozen-lockfile --offline",
        "pnpm format:check",
        "npx vitest run login-form.test.tsx",
        "git diff --check",
    ] {
        if task_text.contains(command) {
            commands.push(command.to_string());
        }
    }
    if !commands.is_empty() {
        commands.dedup();
        return commands;
    }

    if task_text.contains("login-form.test.tsx") {
        vec!["npx vitest run login-form.test.tsx".to_string()]
    } else if task_text.to_ascii_lowercase().contains("no-tests-detected") {
        Vec::new()
    } else {
        vec!["git diff --check".to_string()]
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
    let argv = match command {
        "npx vitest run login-form.test.tsx" => vec!["npx", "vitest", "run", "login-form.test.tsx"],
        "pnpm install --frozen-lockfile --offline" => {
            vec!["pnpm", "install", "--frozen-lockfile", "--offline"]
        }
        "pnpm format:check" => vec!["pnpm", "format:check"],
        "git diff --check" => vec!["git", "diff", "--check"],
        _ => return Err(format!("unsupported live validation command: {command}")),
    };
    let output = Command::new(argv[0])
        .args(&argv[1..])
        .current_dir(worktree)
        .output()
        .map_err(|error| error.to_string())?;
    Ok((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

#[derive(Debug, Clone)]
struct LiveReviewOutcome {
    signals: Vec<HepaReviewSignal>,
    arbitration: HepaArbitrationSummary,
    staging_allowed: bool,
    blockers: Vec<String>,
    reviewer_passes: u32,
}

fn live_review_fanout(
    config: &HepaFakeRunConfig,
    adapter_id: &str,
    spec: &hepa_adapters::spec::HepaAdapterSpec,
    environment: &std::collections::BTreeMap<String, String>,
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    allocation: &HepaWorktreeAllocation,
    changed_files: &[String],
    validation: &HepaValidationSummary,
    diff_context: &str,
) -> Result<LiveReviewOutcome, String> {
    if live_adapter_review_enabled() {
        return live_adapter_review(
            config,
            adapter_id,
            spec,
            environment,
            lane_paths,
            allocation,
            changed_files,
            validation,
            diff_context,
        );
    }
    live_deterministic_review_fanout(config, adapter_id, changed_files, validation, diff_context)
}

fn live_adapter_review_enabled() -> bool {
    matches!(
        std::env::var("HEPA_LIVE_REVIEW_MODE").ok().as_deref(),
        Some("adapter" | "ADAPTER" | "live-adapter" | "LIVE_ADAPTER")
    )
}

fn live_adapter_review(
    config: &HepaFakeRunConfig,
    adapter_id: &str,
    spec: &hepa_adapters::spec::HepaAdapterSpec,
    environment: &std::collections::BTreeMap<String, String>,
    lane_paths: &hepa_core::artifacts::HepaLaneArtifactPaths,
    allocation: &HepaWorktreeAllocation,
    changed_files: &[String],
    validation: &HepaValidationSummary,
    diff_context: &str,
) -> Result<LiveReviewOutcome, String> {
    let review_id = "review-live-adapter";
    let prompt = live_review_prompt(config, changed_files, validation, diff_context);
    let review_output = lane_paths.lane_dir.join("review/live-adapter-output.jsonl");
    let prompt_path = lane_paths.lane_dir.join("review/live-adapter-prompt.md");
    if let Some(parent) = prompt_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&prompt_path, &prompt).map_err(|error| error.to_string())?;
    let invocation = HepaOneshotAdapterInvocation {
        spec: spec.clone(),
        role: HepaAdapterRole::Reviewer,
        context: HepaAdapterTemplateContext {
            prompt_file: lane_paths.lane_dir.join("prompt.md").display().to_string(),
            worktree: allocation.worktree_path.display().to_string(),
            review_prompt_file: prompt_path.display().to_string(),
            output_file: lane_paths
                .lane_dir
                .join("attempts/attempt-1/attempt.json")
                .display()
                .to_string(),
            review_output_file: review_output.display().to_string(),
            artifact_dir: lane_paths.lane_dir.display().to_string(),
        },
        prompt,
        environment: environment.clone(),
        monitor_policy: live_monitor_policy(),
    };
    let result = HepaOneshotAdapterExecutor::new()
        .run(&invocation)
        .map_err(|error| error.to_string())?;
    let raw_review = adapter_review_payload(adapter_id, &result.stdout, &review_output)?;
    let normalization = normalize_reviewer_output_by_exception(
        HepaReviewerOutputInput {
            review_id: review_id.to_string(),
            lane_id: config.lane_id.clone(),
            adapter_id: format!("live-reviewer:{adapter_id}"),
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
    let signal = normalization.signal;
    let policy = HepaReviewFanout {
        adapters: vec![signal.adapter_id.clone()],
        pass_policy: HepaReviewPassPolicy::All,
    };
    let pass_decision = apply_review_pass_policy(&policy, std::slice::from_ref(&signal))
        .map_err(|error| error.to_string())?;
    let findings = aggregate_review_findings(std::slice::from_ref(&signal))
        .map_err(|error| error.to_string())?;
    let mut decisions = Vec::new();
    for finding in findings {
        decisions.push(arbitrate_live_finding(finding.finding)?);
    }
    let arbitration =
        summarize_arbitration_results(&decisions).map_err(format_arbitration_error)?;
    let staging_gate =
        evaluate_staging_after_arbitration(&decisions).map_err(format_arbitration_error)?;
    let mut blockers = staging_gate.blockers;
    if !pass_decision.passed {
        blockers.push(format!(
            "live adapter reviewer required {} approval but received {}",
            pass_decision.required_approvals, pass_decision.approvals
        ));
    }
    blockers.sort();
    Ok(LiveReviewOutcome {
        reviewer_passes: 1,
        signals: vec![signal],
        arbitration,
        staging_allowed: blockers.is_empty(),
        blockers,
    })
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
        live_force_review_block(),
    )
}

fn live_review_fanout_with_controlled_block(
    config: &HepaFakeRunConfig,
    adapter_id: &str,
    changed_files: &[String],
    validation: &HepaValidationSummary,
    diff_context: &str,
    force_review_block: bool,
) -> Result<LiveReviewOutcome, String> {
    let primary_adapter = format!("hepa-manager-live-review:primary:{adapter_id}");
    let policy_adapter = format!("hepa-manager-live-review:policy:{adapter_id}");
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
    let policy = HepaReviewFanout {
        adapters: vec![primary_adapter, policy_adapter],
        pass_policy: HepaReviewPassPolicy::All,
    };
    let pass_decision =
        apply_review_pass_policy(&policy, &result.signals).map_err(|error| error.to_string())?;
    let findings = aggregate_review_findings(&result.signals).map_err(|error| error.to_string())?;
    let mut decisions = Vec::new();
    for finding in findings {
        decisions.push(arbitrate_live_finding(finding.finding)?);
    }
    let arbitration =
        summarize_arbitration_results(&decisions).map_err(format_arbitration_error)?;
    let staging_gate =
        evaluate_staging_after_arbitration(&decisions).map_err(format_arbitration_error)?;
    let mut blockers = staging_gate.blockers;
    if !pass_decision.passed {
        blockers.push(format!(
            "review fanout policy required {} approvals but received {}",
            pass_decision.required_approvals, pass_decision.approvals
        ));
    }
    blockers.sort();
    Ok(LiveReviewOutcome {
        reviewer_passes: result.signals.len() as u32,
        signals: result.signals,
        arbitration,
        staging_allowed: blockers.is_empty(),
        blockers,
    })
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
        } else if request.adapter_id.contains(":primary:") {
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
    let route = if adapter_id.contains(":primary:") {
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

fn format_arbitration_error(error: hepa_review::arbitration::HepaArbitrationError) -> String {
    format!("{}: {}", error.field, error.message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LivePipelinePhase {
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
            status: HepaPhaseStatus::Completed,
            duration_seconds: worker_duration_seconds,
            round: Some(1),
            role: Some(HepaAgentRole::Worker),
            adapter_id: Some(adapter_id.to_string()),
            routing_reason: Some("explicit live adapter".to_string()),
            sandbox_posture: Some(run_sandbox_posture()),
        },
        HepaTimingPhase {
            name: "live_validation".to_string(),
            status: if terminal_phase == LivePipelinePhase::ValidationFailed
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
        LivePipelinePhase::ValidationFailed | LivePipelinePhase::SafetyBlocked
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
            role: Some(HepaAgentRole::Manager),
            adapter_id: Some(format!("hepa-manager-live-review-fanout:{adapter_id}")),
            routing_reason: Some("parallel deterministic manager review fanout".to_string()),
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
    use hepa_kanban::{
        card_mapping::{
            HepaHermesCardMappingInput, HepaHermesCommentKind, HepaHermesFieldValue,
            map_task_to_hermes_card,
        },
        sync::HepaMemoryHermesCardStore,
    };
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
        assert!(prompt.contains("/no_think"));
    }

    #[test]
    fn live_worker_prompt_does_not_add_no_think_for_cloud_or_non_pi() {
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
            !live_worker_prompt_for_adapter("Update README.md", "custom", &local_config)
                .contains("/no_think")
        );
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
        assert!(!prompt.contains("/no_think"));
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
        assert_eq!(outcome.arbitration.records.len(), 3);
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
        assert!(outcome.blockers.iter().any(|blocker| {
            blocker.contains("review fanout policy required 2 approvals but received 0")
        }));
        assert!(outcome.signals.iter().any(|signal| {
            signal
                .summary
                .iter()
                .any(|line| line.contains("inspect reviewer evidence"))
        }));
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
            "hepa-manager-live-review:primary:pi",
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
