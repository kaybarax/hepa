//! CLI handlers for the fleet command groups (project/task/scheduler/fleet),
//! all backed by the deterministic, temp-root-safe `HepaFleetRegistry`.

use crate::run::{HepaFakeRunConfig, run_live_task};
use hepa_adapters::registry::HepaAdapterRegistry;
use hepa_core::config::{HepaConfig, HepaConfigOverrides};
use hepa_core::contracts::HepaLaneState;
use hepa_core::contracts::{
    CONTRACT_SCHEMA_VERSION, HepaFleetTask, HepaLane, HepaProject, HepaReadinessResult,
    HepaReadinessState, HepaReadinessStatus, HepaRiskLevel, HepaTaskSpec, HepaTaskStatus,
};
use hepa_core::fleet_monitor::{HepaFleetMonitor, HepaLaneObservation, HepaResourceSample};
use hepa_core::fleet_registry::{
    HepaCostClass, HepaCostPolicy, HepaFleetRegistry, HepaMemoryPolicy, HepaRegisteredProject,
};
use hepa_core::resource_governor::{
    HepaLaneReservation, HepaResourceLimits, HepaScheduleCandidate,
};
use hepa_core::scheduler::{
    HepaActiveLaneSummary, HepaClaimOutcome, HepaScheduler, HepaSchedulerLimits, HepaTickOutcome,
};
use hepa_git::worktree::HepaWorktreeAllocator;
use hepa_kanban::{
    card_mapping::{HepaHermesCardMappingInput, HepaHermesCardPayload, map_task_to_hermes_card},
    spec_import::{HepaImportedSpec, import_markdown_spec},
    sync::{HepaKanbanSyncEngine, HepaMemoryHermesCardStore},
};
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// A single-line timestamp for CLI-driven record mutations.
fn cli_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format!("t{seconds}")
}

/// Extract a `--control-root <path>` option (defaulting to `.hepa/control`) and
/// return it with the remaining flags.
fn take_control_root(flags: &[String]) -> Result<(PathBuf, Vec<String>), String> {
    let mut control_root = PathBuf::from(".hepa/control");
    let mut rest = Vec::new();
    let mut index = 0;
    while index < flags.len() {
        if flags[index] == "--control-root" {
            let Some(value) = flags.get(index + 1) else {
                return Err("--control-root requires a value".to_string());
            };
            control_root = PathBuf::from(value);
            index += 2;
        } else {
            rest.push(flags[index].clone());
            index += 1;
        }
    }
    Ok((control_root, rest))
}

fn take_option(flags: &mut Vec<String>, name: &str) -> Result<Option<String>, String> {
    if let Some(position) = flags.iter().position(|flag| flag == name) {
        if position + 1 >= flags.len() {
            return Err(format!("{name} requires a value"));
        }
        let value = flags.remove(position + 1);
        flags.remove(position);
        return Ok(Some(value));
    }
    Ok(None)
}

fn take_flag(flags: &mut Vec<String>, name: &str) -> bool {
    if let Some(position) = flags.iter().position(|flag| flag == name) {
        flags.remove(position);
        return true;
    }
    false
}

/// Dispatch `hepa project ...`.
pub fn project_command(args: &[String]) -> Result<String, String> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| "usage: hepa project <add|list|show|doctor|remove>".to_string())?;
    let (control_root, mut flags) = take_control_root(rest)?;
    let registry = HepaFleetRegistry::new(&control_root);

    match subcommand.as_str() {
        "add" => {
            let project_id = positional(&flags, 0, "project id")?;
            let repo_ref = positional(&flags, 1, "repo ref")?;
            let display_name =
                take_option(&mut flags, "--name")?.unwrap_or_else(|| project_id.clone());
            let default_branch =
                take_option(&mut flags, "--branch")?.unwrap_or_else(|| "main".to_string());
            let max_parallel_tasks = take_option(&mut flags, "--max-parallel")?
                .map(|value| value.parse::<u32>())
                .transpose()
                .map_err(|_| "--max-parallel must be a number".to_string())?
                .unwrap_or(2)
                .max(1);
            let now = cli_timestamp();
            let registration = HepaRegisteredProject {
                project: HepaProject {
                    schema_version: CONTRACT_SCHEMA_VERSION,
                    project_id: project_id.clone(),
                    display_name,
                    repo_ref,
                    default_branch,
                    routing_policy_ref: None,
                    is_active: true,
                    created_at: now.clone(),
                    updated_at: now,
                },
                max_parallel_tasks,
                cost_policy: HepaCostPolicy {
                    cost_class: HepaCostClass::Local,
                    max_paid_lanes: 0,
                },
                memory_policy: HepaMemoryPolicy {
                    max_resident_models: 1,
                },
                board_metadata: None,
            };
            registry
                .register_project(&registration)
                .map_err(|error| error.to_string())?;
            Ok(format!("HEPA project add: registered {project_id}"))
        }
        "list" => {
            let projects = registry
                .list_projects()
                .map_err(|error| error.to_string())?;
            if projects.is_empty() {
                return Ok("HEPA project list: no projects registered".to_string());
            }
            let lines = projects
                .iter()
                .map(|project| {
                    format!(
                        "{} max_parallel={} active={}",
                        project.project.project_id,
                        project.max_parallel_tasks,
                        project.project.is_active
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(format!("HEPA project list:\n{lines}"))
        }
        "show" => {
            let project_id = positional(&flags, 0, "project id")?;
            match registry
                .show_project(&project_id)
                .map_err(|error| error.to_string())?
            {
                Some(project) => Ok(format!(
                    "HEPA project show: {} name={} branch={} max_parallel={}",
                    project.project.project_id,
                    project.project.display_name,
                    project.project.default_branch,
                    project.max_parallel_tasks
                )),
                None => Err(format!("project not found: {project_id}")),
            }
        }
        "doctor" => {
            let project_id = positional(&flags, 0, "project id")?;
            match registry
                .show_project(&project_id)
                .map_err(|error| error.to_string())?
            {
                Some(project) => {
                    let repo_ok = !project.project.repo_ref.trim().is_empty();
                    Ok(format!(
                        "HEPA project doctor: {} registered=yes repo_ref_set={} active={}",
                        project_id, repo_ok, project.project.is_active
                    ))
                }
                None => Ok(format!(
                    "HEPA project doctor: {project_id} registered=no (run hepa project add)"
                )),
            }
        }
        "remove" => {
            let project_id = positional(&flags, 0, "project id")?;
            let removed = registry
                .remove_project(&project_id)
                .map_err(|error| error.to_string())?;
            if removed {
                Ok(format!("HEPA project remove: removed {project_id}"))
            } else {
                Ok(format!(
                    "HEPA project remove: {project_id} was not registered"
                ))
            }
        }
        other => Err(format!("unknown project command: {other}")),
    }
}

/// Dispatch `hepa task ...`.
pub fn task_command(args: &[String]) -> Result<String, String> {
    let (subcommand, rest) = args.split_first().ok_or_else(|| {
        "usage: hepa task <create|list|show|cancel|block|complete|resume|prioritize|sync-kanban>".to_string()
    })?;
    let (control_root, flags) = take_control_root(rest)?;
    let registry = HepaFleetRegistry::new(&control_root);
    let now = cli_timestamp();

    match subcommand.as_str() {
        "create" => {
            let project_id = positional(&flags, 0, "project id")?;
            let task_id = positional(&flags, 1, "task id")?;
            let title = positional(&flags, 2, "title")?;
            let task = HepaFleetTask {
                schema_version: CONTRACT_SCHEMA_VERSION,
                task_id: task_id.clone(),
                project_id,
                title,
                description: String::new(),
                status: HepaTaskStatus::Queued,
                readiness: HepaReadinessState::NotReady,
                dependencies: Vec::new(),
                lane_ids: Vec::new(),
                external_card_id: None,
                priority: 1,
                created_at: now.clone(),
                updated_at: now,
                completed_at: None,
            };
            registry
                .create_task(&task)
                .map_err(|error| error.to_string())?;
            Ok(format!("HEPA task create: created {task_id}"))
        }
        "list" => {
            let tasks = registry.list_tasks().map_err(|error| error.to_string())?;
            if tasks.is_empty() {
                return Ok("HEPA task list: no tasks".to_string());
            }
            let lines = tasks
                .iter()
                .map(|task| {
                    format!(
                        "{} status={:?} priority={}",
                        task.task_id, task.status, task.priority
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(format!("HEPA task list:\n{lines}"))
        }
        "show" => {
            let task_id = positional(&flags, 0, "task id")?;
            match registry
                .show_task(&task_id)
                .map_err(|error| error.to_string())?
            {
                Some(task) => Ok(format!(
                    "HEPA task show: {} status={:?} readiness={:?} priority={} deps={}",
                    task.task_id,
                    task.status,
                    task.readiness,
                    task.priority,
                    task.dependencies.len()
                )),
                None => Err(format!("task not found: {task_id}")),
            }
        }
        "cancel" => {
            let task = registry
                .cancel_task(&positional(&flags, 0, "task id")?, &now)
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA task cancel: {} -> {:?}",
                task.task_id, task.status
            ))
        }
        "block" => {
            let task = registry
                .block_task(&positional(&flags, 0, "task id")?, &now)
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA task block: {} -> {:?}",
                task.task_id, task.status
            ))
        }
        "complete" => {
            let task = registry
                .complete_task(&positional(&flags, 0, "task id")?, &now)
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA task complete: {} -> {:?}",
                task.task_id, task.status
            ))
        }
        "resume" => {
            let task = registry
                .resume_task(&positional(&flags, 0, "task id")?, &now)
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA task resume: {} -> {:?}",
                task.task_id, task.status
            ))
        }
        "prioritize" => {
            let task_id = positional(&flags, 0, "task id")?;
            let priority = positional(&flags, 1, "priority")?
                .parse::<u32>()
                .map_err(|_| "priority must be a number".to_string())?;
            let task = registry
                .prioritize_task(&task_id, priority, &now)
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA task prioritize: {} priority={}",
                task.task_id, task.priority
            ))
        }
        "sync-kanban" => {
            // Without a configured Hermes board the sync degrades and reports
            // how many tasks would be synced once Hermes is available.
            let count = registry
                .list_tasks()
                .map_err(|error| error.to_string())?
                .len();
            Ok(format!(
                "HEPA task sync-kanban: tasks={count} status=degraded (Hermes unavailable)"
            ))
        }
        other => Err(format!("unknown task command: {other}")),
    }
}

/// Dispatch `hepa hermes ...`.
pub fn hermes_command(args: &[String]) -> Result<String, String> {
    let (subcommand, rest) = args.split_first().ok_or_else(|| {
        "usage: hepa hermes <ingest-spec|run-dashboard-card|run-ready|run-cards|cards>".to_string()
    })?;
    let (control_root, mut flags) = take_control_root(rest)?;
    let registry = HepaFleetRegistry::new(&control_root);

    match subcommand.as_str() {
        "ingest-spec" => {
            let project_id = positional(&flags, 0, "project id")?;
            let repo_ref = positional(&flags, 1, "repo ref")?;
            let spec_path = positional(&flags, 2, "spec path")?;
            let display_name =
                take_option(&mut flags, "--name")?.unwrap_or_else(|| project_id.clone());
            let default_branch =
                take_option(&mut flags, "--branch")?.unwrap_or_else(|| "main".to_string());
            let max_parallel_tasks = take_option(&mut flags, "--max-parallel")?
                .map(|value| value.parse::<u32>())
                .transpose()
                .map_err(|_| "--max-parallel must be a number".to_string())?
                .unwrap_or(2)
                .max(1);
            if flags.len() > 3 {
                return Err(format!(
                    "unknown hermes ingest-spec flags: {}",
                    flags[3..].join(" ")
                ));
            }
            let now = cli_timestamp();
            let registration = HepaRegisteredProject {
                project: HepaProject {
                    schema_version: CONTRACT_SCHEMA_VERSION,
                    project_id: project_id.clone(),
                    display_name,
                    repo_ref: repo_ref.clone(),
                    default_branch,
                    routing_policy_ref: None,
                    is_active: true,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                },
                max_parallel_tasks,
                cost_policy: HepaCostPolicy {
                    cost_class: HepaCostClass::Local,
                    max_paid_lanes: 0,
                },
                memory_policy: HepaMemoryPolicy {
                    max_resident_models: 1,
                },
                board_metadata: Some("hermes-first-spec-ingest".to_string()),
            };
            registry
                .register_project(&registration)
                .map_err(|error| error.to_string())?;
            let markdown = fs::read_to_string(&spec_path)
                .map_err(|error| format!("failed to read spec file: {error}"))?;
            let imported = normalize_imported_project(
                import_markdown_spec(&markdown).map_err(|error| error.to_string())?,
                &project_id,
            );
            let mut ready = 0usize;
            let mut blocked = 0usize;
            let mut card_ids = Vec::new();
            for imported_task in imported.tasks {
                let card_id = local_hermes_card_id(&project_id, &imported_task.fleet_task.task_id);
                let mut fleet_task = imported_task.fleet_task.clone();
                fleet_task.status = if imported_task.blocked_questions.is_empty() {
                    ready += 1;
                    HepaTaskStatus::Ready
                } else {
                    blocked += 1;
                    HepaTaskStatus::Blocked
                };
                fleet_task.readiness = if imported_task.blocked_questions.is_empty() {
                    HepaReadinessState::Ready
                } else {
                    HepaReadinessState::Blocked
                };
                fleet_task.priority = fleet_task.priority.max(1);
                fleet_task.external_card_id = Some(card_id.clone());
                fleet_task.updated_at = now.clone();
                registry
                    .create_task(&fleet_task)
                    .map_err(|error| error.to_string())?;
                write_hermes_task_spec(&control_root, &imported_task.task_spec)?;
                write_local_hermes_card(
                    &control_root,
                    &card_id,
                    &registration.project,
                    &imported_task.task_spec,
                    &fleet_task,
                    Vec::new(),
                    None,
                    imported_task.blocked_questions.clone(),
                )?;
                card_ids.push(card_id);
            }
            Ok(format!(
                "HEPA hermes ingest-spec: project={} repo_ref={} tasks={} ready={} blocked={} cards={} card_dir={}",
                project_id,
                repo_ref,
                card_ids.len(),
                ready,
                blocked,
                card_ids.join(","),
                local_hermes_cards_dir(&control_root).display()
            ))
        }
        "cards" => {
            let project_filter = take_option(&mut flags, "--project")?;
            if !flags.is_empty() {
                return Err(format!("unknown hermes cards flags: {}", flags.join(" ")));
            }
            let mut cards = local_hermes_card_files(&control_root)?;
            cards.sort();
            let mut lines = Vec::new();
            for path in cards {
                let payload: HepaHermesCardPayload = read_json_file(&path)?;
                let project_id = payload
                    .fields
                    .get("project_id")
                    .map(|value| format_field_value(value))
                    .unwrap_or_else(|| "unknown".to_string());
                if project_filter
                    .as_ref()
                    .is_some_and(|filter| filter != &project_id)
                {
                    continue;
                }
                lines.push(format!(
                    "{} project={} title={}",
                    path.file_stem()
                        .and_then(|value| value.to_str())
                        .unwrap_or("unknown-card"),
                    project_id,
                    payload.title
                ));
            }
            if lines.is_empty() {
                Ok("HEPA hermes cards: no local cards".to_string())
            } else {
                Ok(format!("HEPA hermes cards:\n{}", lines.join("\n")))
            }
        }
        "run-dashboard-card" => {
            let dry_run = take_flag(&mut flags, "--dry-run");
            let update_hermes = !take_flag(&mut flags, "--no-hermes-update");
            let agent = take_option(&mut flags, "--agent")?.unwrap_or_else(|| "pi".to_string());
            let task_json = take_option(&mut flags, "--task-json")?;
            let project_id = take_option(&mut flags, "--project")?;
            let repo_ref = take_option(&mut flags, "--repo")?;
            let (project_id, repo_ref, card_id) = match flags.len() {
                1 => (
                    project_id,
                    repo_ref,
                    positional(&flags, 0, "Hermes card id")?,
                ),
                3 => (
                    Some(positional(&flags, 0, "project id")?),
                    Some(positional(&flags, 1, "repo ref")?),
                    positional(&flags, 2, "Hermes card id")?,
                ),
                _ => {
                    return Err(
                        "usage: hepa hermes run-dashboard-card <card-id> [--project <id>] [--repo <repo-ref>] or hepa hermes run-dashboard-card <project-id> <repo-ref> <card-id>"
                            .to_string(),
                    );
                }
            };
            if flags.len() > 3 {
                return Err(format!(
                    "unknown hermes run-dashboard-card flags: {}",
                    flags[3..].join(" ")
                ));
            }
            run_dashboard_card(
                &registry,
                &control_root,
                project_id.as_deref(),
                repo_ref.as_deref(),
                &card_id,
                &agent,
                dry_run,
                update_hermes,
                task_json.as_deref(),
            )
        }
        "run-ready" | "run-cards" => {
            let project_id = positional(&flags, 0, "project id")?;
            let dry_run = take_flag(&mut flags, "--dry-run");
            let agent = take_option(&mut flags, "--agent")?.unwrap_or_else(|| "pi".to_string());
            let limit = take_option(&mut flags, "--limit")?
                .map(|value| value.parse::<usize>())
                .transpose()
                .map_err(|_| "--limit must be a number".to_string())?
                .unwrap_or(1)
                .max(1);
            let max_concurrency = take_option(&mut flags, "--max-concurrency")?
                .map(|value| value.parse::<usize>())
                .transpose()
                .map_err(|_| "--max-concurrency must be a number".to_string())?
                .unwrap_or(limit)
                .max(1);
            let requested_cards = if subcommand == "run-cards" {
                let cards = flags.iter().skip(1).cloned().collect::<Vec<String>>();
                if cards.is_empty() {
                    return Err("hepa hermes run-cards requires at least one card id".to_string());
                }
                cards
            } else {
                if flags.len() > 1 {
                    return Err(format!(
                        "unknown hermes run-ready flags: {}",
                        flags[1..].join(" ")
                    ));
                }
                Vec::new()
            };
            run_hermes_selected(
                &registry,
                &control_root,
                &project_id,
                &agent,
                limit,
                max_concurrency,
                dry_run,
                requested_cards,
            )
        }
        other => Err(format!("unknown hermes command: {other}")),
    }
}

#[derive(Debug, Clone, Deserialize)]
struct HermesDashboardTaskEnvelope {
    task: HermesDashboardTask,
    #[serde(default)]
    parents: Vec<String>,
    #[serde(default)]
    children: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HermesDashboardTask {
    id: String,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    priority: Option<i64>,
}

fn run_dashboard_card(
    registry: &HepaFleetRegistry,
    control_root: &Path,
    project_id: Option<&str>,
    repo_ref: Option<&str>,
    card_id: &str,
    agent: &str,
    dry_run: bool,
    update_hermes: bool,
    task_json: Option<&str>,
) -> Result<String, String> {
    let envelope = read_dashboard_task(card_id, task_json)?;
    let related_context = if task_json.is_none() {
        read_dashboard_related_context(&envelope)
    } else {
        Vec::new()
    };
    let description = dashboard_task_description(&envelope, &related_context);
    let project_id = project_id
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| extract_project_name(&description))
        .unwrap_or_else(|| "hermes-dashboard".to_string());
    let repo_ref = repo_ref
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| resolve_repo_ref(&project_id))
        .ok_or_else(|| {
            format!(
                "repo ref is required for project {project_id}; pass --repo <path> or set HEPA_PROJECTS_ROOT"
            )
        })?;
    let now = cli_timestamp();
    let registration = HepaRegisteredProject {
        project: HepaProject {
            schema_version: CONTRACT_SCHEMA_VERSION,
            project_id: project_id.clone(),
            display_name: project_id.clone(),
            repo_ref: repo_ref.clone(),
            default_branch: "main".to_string(),
            routing_policy_ref: None,
            is_active: true,
            created_at: now.clone(),
            updated_at: now.clone(),
        },
        max_parallel_tasks: 2,
        cost_policy: HepaCostPolicy {
            cost_class: HepaCostClass::Local,
            max_paid_lanes: 0,
        },
        memory_policy: HepaMemoryPolicy {
            max_resident_models: 1,
        },
        board_metadata: Some("hermes-dashboard-card".to_string()),
    };
    registry
        .register_project(&registration)
        .map_err(|error| error.to_string())?;
    let validation_commands = extract_validation_commands(&description);
    let acceptance_criteria = extract_acceptance_criteria(&description);
    let task = HepaFleetTask {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: envelope.task.id.clone(),
        project_id: project_id.to_string(),
        title: envelope.task.title.clone(),
        description: description.clone(),
        status: HepaTaskStatus::Ready,
        readiness: HepaReadinessState::Ready,
        dependencies: Vec::new(),
        lane_ids: Vec::new(),
        external_card_id: Some(card_id.to_string()),
        priority: envelope.task.priority.unwrap_or(1).max(1) as u32,
        created_at: now.clone(),
        updated_at: now.clone(),
        completed_at: None,
    };
    match registry
        .show_task(&task.task_id)
        .map_err(|error| error.to_string())?
    {
        Some(existing) => {
            if existing.status != HepaTaskStatus::Ready
                || existing.readiness != HepaReadinessState::Ready
            {
                if existing.status == HepaTaskStatus::Running {
                    registry
                        .block_task(&task.task_id, &now)
                        .map_err(|error| error.to_string())?;
                }
                if existing.status != HepaTaskStatus::Queued {
                    registry
                        .resume_task(&task.task_id, &now)
                        .map_err(|error| error.to_string())?;
                }
                registry
                    .mark_task_ready(&task.task_id, &now)
                    .map_err(|error| error.to_string())?;
            }
        }
        None => registry
            .create_task(&task)
            .map_err(|error| error.to_string())?,
    }

    let task_spec = HepaTaskSpec {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: envelope.task.id.clone(),
        project_id: project_id.to_string(),
        goal: description,
        non_goals: vec!["Do not merge the validation PR.".to_string()],
        expected_areas: Vec::new(),
        acceptance_criteria,
        validation_commands,
        dependencies: Vec::new(),
        target_branch: None,
        risk_level: HepaRiskLevel::Medium,
        max_total_rounds: 2,
        created_at: now.clone(),
    };
    write_hermes_task_spec(control_root, &task_spec)?;
    write_local_hermes_card(
        control_root,
        card_id,
        &registration.project,
        &task_spec,
        &task,
        Vec::new(),
        None,
        Vec::new(),
    )?;

    let run_result = run_hermes_selected(
        registry,
        control_root,
        &project_id,
        agent,
        1,
        1,
        dry_run,
        vec![card_id.to_string()],
    );
    if dry_run || !update_hermes {
        return run_result;
    }
    match run_result {
        Ok(message) => {
            update_dashboard_card(
                "complete",
                card_id,
                &message,
                Some(r#"{"hepa_status":"completed"}"#),
            )?;
            Ok(format!(
                "{message}\nHermes dashboard card {card_id} completed"
            ))
        }
        Err(message) => {
            let reason = trim_for_dashboard(&message, 1200);
            update_dashboard_card("block", card_id, &reason, None)?;
            Err(format!(
                "{message}\nHermes dashboard card {card_id} blocked"
            ))
        }
    }
}

fn read_dashboard_task(
    card_id: &str,
    task_json: Option<&str>,
) -> Result<HermesDashboardTaskEnvelope, String> {
    let text = match task_json {
        Some(path) => {
            fs::read_to_string(path).map_err(|error| format!("failed to read {path}: {error}"))?
        }
        None => {
            let output = Command::new("hermes")
                .args(["kanban", "show", card_id, "--json"])
                .output()
                .map_err(|error| format!("failed to run hermes kanban show: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "hermes kanban show failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            String::from_utf8(output.stdout)
                .map_err(|error| format!("hermes kanban show output was not UTF-8: {error}"))?
        }
    };
    serde_json::from_str(&text).map_err(|error| format!("Hermes task JSON is invalid: {error}"))
}

fn read_dashboard_related_context(envelope: &HermesDashboardTaskEnvelope) -> Vec<String> {
    envelope
        .parents
        .iter()
        .chain(envelope.children.iter())
        .filter(|related_id| related_id.as_str() != envelope.task.id)
        .filter_map(|related_id| read_dashboard_task(related_id, None).ok())
        .map(|related| dashboard_task_text(&related.task))
        .filter(|text| !text.trim().is_empty())
        .take(4)
        .collect()
}

fn dashboard_task_description(
    envelope: &HermesDashboardTaskEnvelope,
    related_context: &[String],
) -> String {
    let mut parts = Vec::new();
    if !related_context.is_empty() {
        parts.push(format!(
            "Related Hermes context:\n{}",
            related_context.join("\n\n---\n\n")
        ));
    }
    parts.push(format!(
        "Hermes dashboard task {}:\n{}",
        envelope.task.id,
        dashboard_task_text(&envelope.task)
    ));
    parts.push(
        "HEPA must execute this task through its lane manager, coding adapter, validation, review, Git lifecycle, and PR body gates."
            .to_string(),
    );
    parts.join("\n\n")
}

fn dashboard_task_text(task: &HermesDashboardTask) -> String {
    match task
        .body
        .as_ref()
        .map(|body| body.trim())
        .filter(|body| !body.is_empty())
    {
        Some(body) => format!("Title: {}\n\n{}", task.title.trim(), body),
        None => task.title.trim().to_string(),
    }
}

fn extract_validation_commands(text: &str) -> Vec<String> {
    extract_bullets_after_heading(text, "Validation")
}

fn extract_acceptance_criteria(text: &str) -> Vec<String> {
    let criteria = extract_bullets_after_heading(text, "Acceptance criteria");
    if criteria.is_empty() {
        vec!["HEPA lane completes and creates a reviewable PR.".to_string()]
    } else {
        criteria
    }
}

fn extract_project_name(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("Project:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string())
    })
}

fn resolve_repo_ref(project_id: &str) -> Option<String> {
    if project_id.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(project_id);
        if path.exists() {
            return Some(path.display().to_string());
        }
    }
    let roots = [
        std::env::var_os("HEPA_PROJECTS_ROOT").map(PathBuf::from),
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("workspace").join("projects")),
    ];
    roots
        .into_iter()
        .flatten()
        .map(|root| root.join(project_id))
        .find(|path| path.exists())
        .map(|path| path.display().to_string())
}

fn extract_bullets_after_heading(text: &str, heading: &str) -> Vec<String> {
    let mut in_section = false;
    let mut items = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.trim_end_matches(':').eq_ignore_ascii_case(heading) {
            in_section = true;
            continue;
        }
        if in_section && trimmed.ends_with(':') && !trimmed.starts_with('-') {
            break;
        }
        if in_section {
            if let Some(item) = trimmed.strip_prefix("- ") {
                items.push(item.trim().to_string());
            } else if !trimmed.is_empty() && items.is_empty() {
                items.push(trimmed.to_string());
            }
        }
    }
    items
}

fn update_dashboard_card(
    action: &str,
    card_id: &str,
    message: &str,
    metadata: Option<&str>,
) -> Result<(), String> {
    let mut command = Command::new("hermes");
    match action {
        "complete" => {
            command
                .args(["kanban", "complete", card_id, "--result"])
                .arg(trim_for_dashboard(message, 1800))
                .args(["--summary"])
                .arg(trim_for_dashboard(message, 1800));
            if let Some(metadata) = metadata {
                command.args(["--metadata", metadata]);
            }
        }
        "block" => {
            command
                .args(["kanban", "block", card_id, "--kind", "transient"])
                .arg(trim_for_dashboard(message, 1800));
        }
        other => return Err(format!("unknown dashboard update action: {other}")),
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to run hermes kanban {action}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "hermes kanban {action} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn trim_for_dashboard(text: &str, max_chars: usize) -> String {
    let mut trimmed = text.trim().chars().take(max_chars).collect::<String>();
    if text.trim().chars().count() > max_chars {
        trimmed.push_str("...");
    }
    trimmed
}

fn normalize_imported_project(
    mut imported: HepaImportedSpec,
    project_id: &str,
) -> HepaImportedSpec {
    imported.project_id = project_id.to_string();
    for task in &mut imported.tasks {
        task.task_spec.project_id = project_id.to_string();
        task.fleet_task.project_id = project_id.to_string();
    }
    imported
}

fn local_hermes_card_id(project_id: &str, task_id: &str) -> String {
    format!("hermes-{project_id}-{task_id}")
}

fn local_hermes_cards_dir(control_root: &Path) -> PathBuf {
    control_root.join("hermes").join("cards")
}

fn local_hermes_specs_dir(control_root: &Path) -> PathBuf {
    control_root.join("hermes").join("task-specs")
}

fn local_hermes_card_path(control_root: &Path, card_id: &str) -> PathBuf {
    local_hermes_cards_dir(control_root).join(format!("{card_id}.json"))
}

fn local_hermes_spec_path(control_root: &Path, task_id: &str) -> PathBuf {
    local_hermes_specs_dir(control_root).join(format!("{task_id}.json"))
}

fn write_hermes_task_spec(control_root: &Path, task_spec: &HepaTaskSpec) -> Result<(), String> {
    write_json(
        &local_hermes_spec_path(control_root, &task_spec.task_id),
        task_spec,
    )
}

fn read_hermes_task_spec(
    control_root: &Path,
    task: &HepaFleetTask,
) -> Result<HepaTaskSpec, String> {
    let path = local_hermes_spec_path(control_root, &task.task_id);
    if path.exists() {
        return read_json_file(&path);
    }
    Ok(HepaTaskSpec {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: task.task_id.clone(),
        project_id: task.project_id.clone(),
        goal: if task.description.trim().is_empty() {
            task.title.clone()
        } else {
            task.description.clone()
        },
        non_goals: Vec::new(),
        expected_areas: Vec::new(),
        acceptance_criteria: vec!["HEPA lane completes and creates a reviewable PR.".to_string()],
        validation_commands: Vec::new(),
        dependencies: task.dependencies.clone(),
        target_branch: None,
        risk_level: HepaRiskLevel::Low,
        max_total_rounds: 1,
        created_at: task.created_at.clone(),
    })
}

fn write_local_hermes_card(
    control_root: &Path,
    card_id: &str,
    project: &HepaProject,
    task_spec: &HepaTaskSpec,
    task: &HepaFleetTask,
    lanes: Vec<HepaLane>,
    terminal_report: Option<hepa_core::contracts::HepaTerminalTaskReport>,
    blocked_questions: Vec<String>,
) -> Result<(), String> {
    let payload = map_task_to_hermes_card(&HepaHermesCardMappingInput {
        project: project.clone(),
        task_spec: task_spec.clone(),
        task: task.clone(),
        lanes,
        readiness: None,
        validation: terminal_report
            .as_ref()
            .and_then(|report| report.validation.clone()),
        review_signals: terminal_report
            .as_ref()
            .map(|report| report.review_signals.clone())
            .unwrap_or_default(),
        terminal_report: terminal_report.clone(),
        timing: terminal_report.and_then(|report| report.timing),
        steering_records: Vec::new(),
        blocked_questions,
    })
    .map_err(|error| error.to_string())?;
    write_json(&local_hermes_card_path(control_root, card_id), &payload)
}

fn local_hermes_card_files(control_root: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = local_hermes_cards_dir(control_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            files.push(path);
        }
    }
    Ok(files)
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn format_field_value(value: &hepa_kanban::card_mapping::HepaHermesFieldValue) -> String {
    match value {
        hepa_kanban::card_mapping::HepaHermesFieldValue::Text(value) => value.clone(),
        hepa_kanban::card_mapping::HepaHermesFieldValue::Number(value) => value.to_string(),
        hepa_kanban::card_mapping::HepaHermesFieldValue::Bool(value) => value.to_string(),
        hepa_kanban::card_mapping::HepaHermesFieldValue::List(values) => values.join(","),
    }
}

fn run_hermes_selected(
    registry: &HepaFleetRegistry,
    control_root: &Path,
    project_id: &str,
    agent: &str,
    limit: usize,
    max_concurrency: usize,
    dry_run: bool,
    requested_cards: Vec<String>,
) -> Result<String, String> {
    let project = registry
        .show_project(project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("project not found: {project_id}"))?;
    let mut tasks = registry
        .list_tasks()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|task| task.project_id == project_id)
        .filter(|task| {
            if requested_cards.is_empty() {
                task.status == HepaTaskStatus::Ready && task.readiness == HepaReadinessState::Ready
            } else {
                task.external_card_id.as_ref().is_some_and(|card_id| {
                    requested_cards.iter().any(|requested| requested == card_id)
                })
            }
        })
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    tasks.truncate(limit);
    if tasks.is_empty() {
        return Ok(format!(
            "HEPA hermes {}: selected=0 project={} reason=no-ready-cards",
            if dry_run { "dry-run" } else { "run" },
            project_id
        ));
    }

    let now = cli_timestamp();
    let run_nonce = now.clone();
    let mut launch_specs = Vec::new();
    for task in tasks {
        if !registry
            .unmet_dependencies(&task.task_id)
            .map_err(|error| error.to_string())?
            .is_empty()
        {
            continue;
        }
        let lane_id = format!("lane-hermes-{}-{run_nonce}", task.task_id);
        let task_spec = read_hermes_task_spec(control_root, &task)?;
        let lane = hermes_lane_record(
            &project.project,
            &task,
            &lane_id,
            agent,
            HepaLaneState::Running,
        );
        let card_id = task
            .external_card_id
            .clone()
            .unwrap_or_else(|| local_hermes_card_id(project_id, &task.task_id));
        let claimed = if dry_run {
            HepaFleetTask {
                lane_ids: vec![lane_id.clone()],
                ..task.clone()
            }
        } else {
            registry
                .claim_task_into_lane(&task.task_id, &lane_id, &now)
                .map_err(|error| error.to_string())?
        };
        write_local_hermes_card(
            control_root,
            &card_id,
            &project.project,
            &task_spec,
            &HepaFleetTask {
                lane_ids: vec![lane_id.clone()],
                ..claimed.clone()
            },
            vec![lane],
            None,
            Vec::new(),
        )?;
        launch_specs.push(HermesLaunchSpec {
            project: project.clone(),
            task: claimed,
            task_spec,
            lane_id,
            card_id,
        });
    }

    if dry_run {
        let lines = launch_specs
            .iter()
            .map(|spec| {
                format!(
                    "{} card={} attach='hepa lane attach {} --control-root {} --tail 50'",
                    spec.task.task_id,
                    spec.card_id,
                    spec.lane_id,
                    control_root.display()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(format!(
            "HEPA hermes dry-run: project={} selected={} agent={} max_concurrency={}\n{}",
            project_id,
            launch_specs.len(),
            agent,
            max_concurrency,
            lines
        ));
    }

    let mut summaries = Vec::new();
    for chunk in launch_specs.chunks(max_concurrency) {
        let handles = chunk
            .iter()
            .cloned()
            .map(|spec| {
                let control_root = control_root.to_path_buf();
                let agent = agent.to_string();
                thread::spawn(move || run_hermes_launch_spec(control_root, spec, &agent))
            })
            .collect::<Vec<_>>();
        for handle in handles {
            summaries.push(
                handle
                    .join()
                    .map_err(|_| "Hermes run worker thread panicked".to_string())?,
            );
        }
    }
    let failed = summaries.iter().filter(|summary| summary.failed).count();
    let lines = summaries
        .iter()
        .map(|summary| {
            format!(
                "{} status={} card={} lane={} pr={} attach='{}'",
                summary.task_id,
                summary.status,
                summary.card_id,
                summary.lane_id,
                summary.pr_url.as_deref().unwrap_or("none"),
                summary.attach_command
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let message = format!(
        "HEPA hermes run: project={} selected={} failed={} agent={} max_concurrency={}\n{}",
        project_id,
        summaries.len(),
        failed,
        agent,
        max_concurrency,
        lines
    );
    if failed == 0 {
        Ok(message)
    } else {
        Err(message)
    }
}

#[derive(Clone)]
struct HermesLaunchSpec {
    project: HepaRegisteredProject,
    task: HepaFleetTask,
    task_spec: HepaTaskSpec,
    lane_id: String,
    card_id: String,
}

struct HermesRunSummary {
    task_id: String,
    lane_id: String,
    card_id: String,
    status: String,
    pr_url: Option<String>,
    attach_command: String,
    failed: bool,
}

fn run_hermes_launch_spec(
    control_root: PathBuf,
    spec: HermesLaunchSpec,
    agent: &str,
) -> HermesRunSummary {
    let registry = HepaFleetRegistry::new(&control_root);
    let repo_path = PathBuf::from(&spec.project.project.repo_ref);
    let config = HepaFakeRunConfig {
        control_root: repo_path.join(".hepa/control"),
        worktree_root: repo_path.join(".hepa/worktrees"),
        archive_root: repo_path.join(".hepa/archive"),
        repo_path: repo_path.clone(),
        run_id: format!("run-hermes-{}-{}", spec.task.task_id, cli_timestamp()),
        task_id: spec.task.task_id.clone(),
        lane_id: spec.lane_id.clone(),
        task_text: spec.task_spec.goal.clone(),
        timing: true,
    };
    let attach_command = format!(
        "hepa lane attach {} --control-root {} --tail 50",
        spec.lane_id,
        config.control_root.display()
    );
    match run_live_task(&config, agent) {
        Ok(result) => {
            let completed = registry
                .complete_task(&spec.task.task_id, &cli_timestamp())
                .unwrap_or_else(|_| HepaFleetTask {
                    status: HepaTaskStatus::Completed,
                    completed_at: Some(cli_timestamp()),
                    ..spec.task.clone()
                });
            let lane = hermes_lane_record(
                &spec.project.project,
                &completed,
                &spec.lane_id,
                agent,
                HepaLaneState::Completed,
            );
            let _ = write_local_hermes_card(
                &control_root,
                &spec.card_id,
                &spec.project.project,
                &spec.task_spec,
                &HepaFleetTask {
                    lane_ids: vec![spec.lane_id.clone()],
                    ..completed
                },
                vec![lane],
                Some(result.terminal_report.clone()),
                Vec::new(),
            );
            HermesRunSummary {
                task_id: spec.task.task_id,
                lane_id: result.lane_id,
                card_id: spec.card_id,
                status: result.status,
                pr_url: result.terminal_report.pr_url,
                attach_command,
                failed: false,
            }
        }
        Err(error) => {
            let blocked = registry
                .block_task(&spec.task.task_id, &cli_timestamp())
                .unwrap_or_else(|_| HepaFleetTask {
                    status: HepaTaskStatus::Blocked,
                    ..spec.task.clone()
                });
            let lane = hermes_lane_record(
                &spec.project.project,
                &blocked,
                &spec.lane_id,
                agent,
                HepaLaneState::Blocked,
            );
            let _ = write_local_hermes_card(
                &control_root,
                &spec.card_id,
                &spec.project.project,
                &spec.task_spec,
                &HepaFleetTask {
                    lane_ids: vec![spec.lane_id.clone()],
                    ..blocked
                },
                vec![lane],
                None,
                vec![format!("HEPA run failed: {error}")],
            );
            HermesRunSummary {
                task_id: spec.task.task_id,
                lane_id: spec.lane_id,
                card_id: spec.card_id,
                status: "blocked".to_string(),
                pr_url: None,
                attach_command,
                failed: true,
            }
        }
    }
}

fn hermes_lane_record(
    project: &HepaProject,
    task: &HepaFleetTask,
    lane_id: &str,
    agent: &str,
    state: HepaLaneState,
) -> HepaLane {
    let timestamp = cli_timestamp();
    let completed_at = if matches!(
        state,
        HepaLaneState::Completed | HepaLaneState::Blocked | HepaLaneState::Cancelled
    ) {
        Some(timestamp.clone())
    } else {
        None
    };
    HepaLane {
        schema_version: CONTRACT_SCHEMA_VERSION,
        lane_id: lane_id.to_string(),
        project_id: project.project_id.clone(),
        task_id: task.task_id.clone(),
        adapter_id: agent.to_string(),
        state,
        worktree_ref: format!("worktree:{lane_id}"),
        branch: format!("hepa/{}", task.task_id),
        run_dir_ref: format!("control:runs/{lane_id}"),
        attempt_count: 0,
        created_at: timestamp.clone(),
        updated_at: timestamp,
        completed_at,
    }
}

fn task_status_to_lane_state(status: &HepaTaskStatus) -> Option<HepaLaneState> {
    match status {
        HepaTaskStatus::Running => Some(HepaLaneState::Running),
        HepaTaskStatus::Blocked => Some(HepaLaneState::Blocked),
        HepaTaskStatus::Completed => Some(HepaLaneState::Completed),
        HepaTaskStatus::Cancelled => Some(HepaLaneState::Cancelled),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize)]
struct HepaFleetStressRunArtifact {
    schema_version: u32,
    run_id: String,
    projects: usize,
    tasks: usize,
    claimed_lanes: Vec<HepaFleetStressLaneArtifact>,
    resource_observations: Vec<String>,
    governor_decisions: Vec<String>,
    card_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HepaFleetStressLaneArtifact {
    lane_id: String,
    task_id: String,
    project_id: String,
    repo_ref: String,
    branch: String,
    worktree_ref: String,
    card_id: String,
    status: String,
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut json = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    json.push('\n');
    fs::write(path, json).map_err(|error| error.to_string())
}

fn fleet_stress_run(control_root: &Path) -> Result<String, String> {
    let registry = HepaFleetRegistry::new(control_root);
    let projects = registry
        .list_projects()
        .map_err(|error| error.to_string())?;
    let tasks = registry.list_tasks().map_err(|error| error.to_string())?;
    if projects.len() < 3 || tasks.len() < 3 {
        return Err(
            "fleet stress-run requires at least three projects and three tasks".to_string(),
        );
    }

    let mut scheduler = HepaScheduler::new();
    scheduler.start();
    let limits = HepaResourceLimits::new(3, 1);
    let mut reservations: Vec<HepaLaneReservation> = Vec::new();
    let mut lanes = Vec::new();
    let run_dir = control_root.join("fleet/stress-runs/rs-7");
    fs::create_dir_all(&run_dir).map_err(|error| error.to_string())?;

    for (index, project) in projects.iter().enumerate() {
        let Some(task) = tasks
            .iter()
            .find(|task| task.project_id == project.project.project_id)
        else {
            continue;
        };
        if task.status == HepaTaskStatus::Queued {
            registry
                .mark_task_ready(&task.task_id, &cli_timestamp())
                .map_err(|error| error.to_string())?;
        }
        let lane_id = format!("rs7-lane-{}", project.project.project_id);
        let candidate = HepaScheduleCandidate {
            task_id: task.task_id.clone(),
            adapter_id: if index == 0 {
                "pi-paid-cloud".to_string()
            } else {
                "pi-local".to_string()
            },
            cost_class: if index == 0 {
                HepaCostClass::Paid
            } else {
                HepaCostClass::Local
            },
            file_areas: vec![format!("repo:{}", project.project.project_id)],
            conflict_group: Some(project.project.project_id.clone()),
            touches_lockfile: project.project.project_id.contains("repo-b"),
        };
        let claim = scheduler
            .claim_one(
                &registry,
                &limits,
                &reservations,
                &candidate,
                &lane_id,
                &cli_timestamp(),
            )
            .map_err(|error| error.to_string())?;
        let HepaClaimOutcome::Claimed { lane } = claim else {
            return Err(format!("scheduler did not claim {}", task.task_id));
        };
        reservations.push(lane);
        let repo_path = PathBuf::from(&project.project.repo_ref);
        let allocator = HepaWorktreeAllocator::new(&repo_path, repo_path.join(".hepa/worktrees"));
        let allocation = allocator
            .allocate_lane_with_metadata(&lane_id, cli_timestamp())
            .map_err(|error| error.to_string())?;
        let card_id = format!("hermes-card-rs7-{}", project.project.project_id);
        let lane_record = HepaLane {
            schema_version: CONTRACT_SCHEMA_VERSION,
            lane_id: lane_id.clone(),
            project_id: project.project.project_id.clone(),
            task_id: task.task_id.clone(),
            adapter_id: candidate.adapter_id.clone(),
            state: HepaLaneState::Completed,
            worktree_ref: format!("worktree:{lane_id}"),
            branch: allocation.branch.clone(),
            run_dir_ref: "control:fleet/stress-runs/rs-7".to_string(),
            attempt_count: 1,
            created_at: cli_timestamp(),
            updated_at: cli_timestamp(),
            completed_at: Some(cli_timestamp()),
        };
        let completed_task = registry
            .complete_task(&task.task_id, &cli_timestamp())
            .map_err(|error| error.to_string())?;
        let task_spec = HepaTaskSpec {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: task.task_id.clone(),
            project_id: project.project.project_id.clone(),
            goal: task.title.clone(),
            non_goals: vec!["Validation-only RS-7 fleet stress lane.".to_string()],
            expected_areas: vec![format!("repo:{}", project.project.project_id)],
            acceptance_criteria: vec!["Fleet lane reaches terminal state.".to_string()],
            validation_commands: Vec::new(),
            dependencies: Vec::new(),
            target_branch: Some(project.project.default_branch.clone()),
            risk_level: HepaRiskLevel::Low,
            max_total_rounds: 1,
            created_at: cli_timestamp(),
        };
        let readiness = HepaReadinessResult {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: task.task_id.clone(),
            status: HepaReadinessStatus::Ready,
            blockers: Vec::new(),
            questions: Vec::new(),
            checked_at: cli_timestamp(),
        };
        let card_payload =
            hepa_kanban::card_mapping::map_task_to_hermes_card(&HepaHermesCardMappingInput {
                project: project.project.clone(),
                task_spec,
                task: HepaFleetTask {
                    external_card_id: Some(card_id.clone()),
                    ..completed_task
                },
                lanes: vec![lane_record],
                readiness: Some(readiness),
                validation: None,
                review_signals: Vec::new(),
                terminal_report: None,
                timing: None,
                steering_records: Vec::new(),
                blocked_questions: Vec::new(),
            })
            .map_err(|error| error.to_string())?;
        write_json(&run_dir.join(format!("{lane_id}-card.json")), &card_payload)?;
        lanes.push(HepaFleetStressLaneArtifact {
            lane_id,
            task_id: task.task_id.clone(),
            project_id: project.project.project_id.clone(),
            repo_ref: "<VALIDATION_REPO>".to_string(),
            branch: allocation.branch,
            worktree_ref: format!("worktree:{}", allocation.lane_id),
            card_id,
            status: "completed".to_string(),
        });
    }

    let mut store = HepaMemoryHermesCardStore::default();
    let sync_summary = HepaKanbanSyncEngine::new()
        .sync_tasks(&[], &mut store)
        .map_err(|error| error.to_string())?;
    let artifact = HepaFleetStressRunArtifact {
        schema_version: CONTRACT_SCHEMA_VERSION,
        run_id: "rs-7-fleet-stress".to_string(),
        projects: projects.len(),
        tasks: tasks.len(),
        card_ids: lanes.iter().map(|lane| lane.card_id.clone()).collect(),
        claimed_lanes: lanes,
        resource_observations: vec![
            "max_parallel_lanes=3".to_string(),
            "paid_lane_cap=1".to_string(),
            format!("active_reservations={}", reservations.len()),
            format!("kanban_memory_sync_status={:?}", sync_summary.status),
        ],
        governor_decisions: reservations
            .iter()
            .map(|reservation| {
                format!(
                    "{} adapter={} cost={:?} file_areas={}",
                    reservation.lane_id,
                    reservation.adapter_id,
                    reservation.cost_class,
                    reservation.file_areas.join(",")
                )
            })
            .collect(),
    };
    write_json(&run_dir.join("summary.json"), &artifact)?;
    Ok(format!(
        "HEPA fleet stress-run: projects={} tasks={} lanes={} artifacts={}",
        artifact.projects,
        artifact.tasks,
        artifact.claimed_lanes.len(),
        run_dir.display()
    ))
}

#[derive(Debug, Clone, Serialize)]
struct HepaDesktopDashboardSnapshot {
    schema_version: u32,
    surface: String,
    generated_at: String,
    hermes_status: String,
    project_count: usize,
    task_count: usize,
    lane_count: usize,
    scheduler: HepaDesktopSchedulerSnapshot,
    projects: Vec<HepaDesktopProjectSnapshot>,
    tasks: Vec<HepaDesktopTaskSnapshot>,
    lane_streams: Vec<HepaDesktopLaneStreamSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
struct HepaDesktopSchedulerSnapshot {
    run_state: String,
    ready: u32,
    running: u32,
    blocked: u32,
    queued: u32,
    active_lanes: u32,
    waits: Vec<HepaDesktopWaitSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
struct HepaDesktopWaitSnapshot {
    task_id: String,
    reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HepaDesktopProjectSnapshot {
    project_id: String,
    display_name: String,
    default_branch: String,
    max_parallel_tasks: u32,
    active: bool,
    board_configured: bool,
}

#[derive(Debug, Clone, Serialize)]
struct HepaDesktopTaskSnapshot {
    task_id: String,
    project_id: String,
    title: String,
    status: String,
    readiness: String,
    priority: u32,
    lanes: Vec<String>,
    dependencies: Vec<String>,
    card_configured: bool,
}

#[derive(Debug, Clone, Serialize)]
struct HepaDesktopLaneStreamSnapshot {
    lane_id: String,
    stream_count: usize,
    streams: Vec<String>,
    tail: Vec<String>,
}

fn desktop_dashboard_snapshot(
    control_root: &Path,
    projects: &[HepaRegisteredProject],
    tasks: &[HepaFleetTask],
) -> Result<HepaDesktopDashboardSnapshot, String> {
    let registry = HepaFleetRegistry::new(control_root);
    let mut scheduler = HepaScheduler::new();
    if read_scheduler_running(control_root) {
        scheduler.start();
    }
    let active = active_lanes(&registry)?;
    let status = scheduler
        .status(
            &registry,
            &HepaSchedulerLimits {
                max_parallel_lanes: 4,
            },
            &active,
        )
        .map_err(|error| error.to_string())?;
    let lane_streams = dashboard_lane_streams(control_root, tasks)?;
    Ok(HepaDesktopDashboardSnapshot {
        schema_version: CONTRACT_SCHEMA_VERSION,
        surface: "desktop-dashboard-snapshot".to_string(),
        generated_at: cli_timestamp(),
        hermes_status: "degraded_or_unavailable_local_snapshot".to_string(),
        project_count: projects.len(),
        task_count: tasks.len(),
        lane_count: tasks.iter().map(|task| task.lane_ids.len()).sum(),
        scheduler: HepaDesktopSchedulerSnapshot {
            run_state: format!("{:?}", status.run_state),
            ready: status.ready,
            running: status.running,
            blocked: status.blocked,
            queued: status.queued,
            active_lanes: status.active_lanes,
            waits: status
                .waits
                .into_iter()
                .map(|wait| HepaDesktopWaitSnapshot {
                    task_id: wait.task_id,
                    reasons: wait
                        .reasons
                        .into_iter()
                        .map(|reason| reason.describe())
                        .collect(),
                })
                .collect(),
        },
        projects: projects
            .iter()
            .map(|project| HepaDesktopProjectSnapshot {
                project_id: project.project.project_id.clone(),
                display_name: project.project.display_name.clone(),
                default_branch: project.project.default_branch.clone(),
                max_parallel_tasks: project.max_parallel_tasks,
                active: project.project.is_active,
                board_configured: project.board_metadata.is_some(),
            })
            .collect(),
        tasks: tasks
            .iter()
            .map(|task| HepaDesktopTaskSnapshot {
                task_id: task.task_id.clone(),
                project_id: task.project_id.clone(),
                title: task.title.clone(),
                status: format!("{:?}", task.status),
                readiness: format!("{:?}", task.readiness),
                priority: task.priority,
                lanes: task.lane_ids.clone(),
                dependencies: task.dependencies.clone(),
                card_configured: task.external_card_id.is_some(),
            })
            .collect(),
        lane_streams,
    })
}

fn dashboard_lane_streams(
    control_root: &Path,
    tasks: &[HepaFleetTask],
) -> Result<Vec<HepaDesktopLaneStreamSnapshot>, String> {
    let mut lanes = tasks
        .iter()
        .flat_map(|task| task.lane_ids.iter().cloned())
        .collect::<Vec<_>>();
    lanes.sort();
    lanes.dedup();
    lanes
        .into_iter()
        .map(|lane_id| {
            let streams =
                find_lane_stream_logs(control_root, &lane_id).map_err(|error| error.to_string())?;
            let mut tail = Vec::new();
            for stream in &streams {
                for line in tail_file_lines(stream, 3)? {
                    tail.push(format!("{}: {line}", stream.display()));
                }
            }
            Ok(HepaDesktopLaneStreamSnapshot {
                lane_id,
                stream_count: streams.len(),
                streams: streams
                    .iter()
                    .map(|stream| stream.display().to_string())
                    .collect(),
                tail,
            })
        })
        .collect()
}

fn dashboard_json_path(html_path: &Path) -> PathBuf {
    let mut path = html_path.to_path_buf();
    path.set_extension("json");
    path
}

fn write_desktop_dashboard(
    output_path: &Path,
    snapshot: &HepaDesktopDashboardSnapshot,
) -> Result<PathBuf, String> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let json_path = dashboard_json_path(output_path);
    write_json(&json_path, snapshot)?;
    fs::write(output_path, render_desktop_dashboard(snapshot))
        .map_err(|error| error.to_string())?;
    Ok(json_path)
}

fn render_desktop_dashboard(snapshot: &HepaDesktopDashboardSnapshot) -> String {
    let project_cards = snapshot
        .projects
        .iter()
        .map(|project| {
            format!(
                "<article><h2>{}</h2><dl><dt>Project</dt><dd>{}</dd><dt>Branch</dt><dd>{}</dd><dt>Parallel</dt><dd>{}</dd><dt>Board</dt><dd>{}</dd></dl></article>",
                html_escape(&project.display_name),
                html_escape(&project.project_id),
                html_escape(&project.default_branch),
                project.max_parallel_tasks,
                if project.board_configured { "configured" } else { "headless" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let task_rows = snapshot
        .tasks
        .iter()
        .map(|task| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                html_escape(&task.task_id),
                html_escape(&task.project_id),
                html_escape(&task.title),
                html_escape(&task.status),
                html_escape(&task.readiness),
                task.priority
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let waits = snapshot
        .scheduler
        .waits
        .iter()
        .map(|wait| {
            format!(
                "<li><strong>{}</strong>: {}</li>",
                html_escape(&wait.task_id),
                html_escape(&wait.reasons.join("; "))
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let stream_rows = snapshot
        .lane_streams
        .iter()
        .map(|stream| {
            let tail = if stream.tail.is_empty() {
                "none".to_string()
            } else {
                stream
                    .tail
                    .iter()
                    .map(|line| html_escape(line))
                    .collect::<Vec<_>>()
                    .join("<br>")
            };
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                html_escape(&stream.lane_id),
                stream.stream_count,
                html_escape(&stream.streams.join(", ")),
                tail
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>HEPA Fleet Dashboard</title>
<style>
:root {{ color-scheme: light; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: #f5f7f8; color: #16201c; }}
body {{ margin: 0; }}
header {{ background: #16352f; color: white; padding: 24px 32px; }}
main {{ padding: 24px 32px 40px; }}
h1, h2 {{ margin: 0; font-weight: 700; letter-spacing: 0; }}
h1 {{ font-size: 28px; }}
h2 {{ font-size: 18px; }}
.meta {{ margin-top: 8px; color: #d5e5df; }}
.stats, .projects {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 12px; margin: 20px 0; }}
.stat, article {{ background: white; border: 1px solid #dbe4e1; border-radius: 6px; padding: 14px; }}
.value {{ display: block; font-size: 26px; font-weight: 700; margin-top: 4px; }}
dl {{ display: grid; grid-template-columns: max-content 1fr; gap: 6px 12px; margin: 12px 0 0; }}
dt {{ color: #52615c; }}
dd {{ margin: 0; overflow-wrap: anywhere; }}
table {{ width: 100%; border-collapse: collapse; background: white; border: 1px solid #dbe4e1; border-radius: 6px; overflow: hidden; }}
th, td {{ text-align: left; padding: 10px 12px; border-bottom: 1px solid #e8eeeb; vertical-align: top; }}
th {{ background: #edf3f1; color: #243b35; }}
ul {{ background: white; border: 1px solid #dbe4e1; border-radius: 6px; padding: 14px 24px; }}
</style>
</head>
<body>
<header>
<h1>HEPA Fleet Dashboard</h1>
<div class="meta">surface={} &middot; Hermes={} &middot; generated={}</div>
</header>
<main>
<section class="stats">
<div class="stat">Projects<span class="value">{}</span></div>
<div class="stat">Tasks<span class="value">{}</span></div>
<div class="stat">Lanes<span class="value">{}</span></div>
<div class="stat">Scheduler<span class="value">{}</span></div>
</section>
<section>
<h2>Projects</h2>
<div class="projects">
{}
</div>
</section>
<section>
<h2>Tasks</h2>
<table><thead><tr><th>Task</th><th>Project</th><th>Title</th><th>Status</th><th>Readiness</th><th>Priority</th></tr></thead><tbody>
{}
</tbody></table>
</section>
<section>
<h2>Lane Streams</h2>
<table><thead><tr><th>Lane</th><th>Streams</th><th>Paths</th><th>Tail</th></tr></thead><tbody>
{}
</tbody></table>
</section>
<section>
<h2>Wait Reasons</h2>
<ul>{}</ul>
</section>
</main>
</body>
</html>
"#,
        html_escape(&snapshot.surface),
        html_escape(&snapshot.hermes_status),
        html_escape(&snapshot.generated_at),
        snapshot.project_count,
        snapshot.task_count,
        snapshot.lane_count,
        html_escape(&snapshot.scheduler.run_state),
        project_cards,
        task_rows,
        if stream_rows.is_empty() {
            "<tr><td colspan=\"4\">none</td></tr>".to_string()
        } else {
            stream_rows
        },
        if waits.is_empty() {
            "<li>none</li>".to_string()
        } else {
            waits
        }
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Dispatch `hepa fleet <status|doctor|report|cleanup|reconcile|dashboard|live-matrix>`.
pub fn fleet_command(args: &[String]) -> Result<String, String> {
    let (subcommand, rest) = args.split_first().ok_or_else(|| {
        "usage: hepa fleet <status|watch|doctor|report|cleanup|reconcile|dashboard|live-matrix>"
            .to_string()
    })?;
    let (control_root, mut flags) = take_control_root(rest)?;
    let registry = HepaFleetRegistry::new(&control_root);
    let tasks = registry.list_tasks().map_err(|error| error.to_string())?;
    let projects = registry
        .list_projects()
        .map_err(|error| error.to_string())?;
    let lanes: Vec<(String, HepaTaskStatus)> = tasks
        .iter()
        .flat_map(|task| {
            task.lane_ids
                .iter()
                .map(move |lane| (lane.clone(), task.status.clone()))
        })
        .collect();

    let observations: Vec<HepaLaneObservation> = tasks
        .iter()
        .flat_map(|task| {
            task.lane_ids.iter().filter_map(move |lane_id| {
                task_status_to_lane_state(&task.status).map(|lane_state| HepaLaneObservation {
                    lane_id: lane_id.clone(),
                    task_id: task.task_id.clone(),
                    lane_state,
                    process_alive: task.status == HepaTaskStatus::Running,
                    branch_present: true,
                    pr_status: None,
                    validation_state: None,
                    review_state: None,
                    card_status: None,
                    worktree_present: task.status == HepaTaskStatus::Running,
                })
            })
        })
        .collect();

    match subcommand.as_str() {
        "status" => {
            let running = lanes
                .iter()
                .filter(|(_, status)| *status == HepaTaskStatus::Running)
                .count();
            Ok(format!(
                "HEPA fleet status: projects={} tasks={} lanes={} running_lanes={}",
                projects.len(),
                tasks.len(),
                lanes.len(),
                running
            ))
        }
        "watch" => {
            if !flags.is_empty() {
                return Err(format!("unknown fleet watch flags: {}", flags.join(" ")));
            }
            let control_root_display = control_root.display().to_string();
            let active = tasks
                .iter()
                .filter(|task| task.status == HepaTaskStatus::Running)
                .flat_map(|task| {
                    let control_root_display = control_root_display.clone();
                    task.lane_ids.iter().map(move |lane_id| {
                        format!(
                            "{} task={} project={} attach='hepa lane attach {} --control-root {} --tail 50'",
                            lane_id,
                            task.task_id,
                            task.project_id,
                            lane_id,
                            control_root_display
                        )
                    })
                })
                .collect::<Vec<_>>();
            if active.is_empty() {
                Ok("HEPA fleet watch: no active lanes".to_string())
            } else {
                Ok(format!("HEPA fleet watch:\n{}", active.join("\n")))
            }
        }
        "doctor" => {
            let control_ok = control_root.exists() || control_root.parent().is_some();
            Ok(format!(
                "HEPA fleet doctor: control_root_ok={} projects={} tasks={}",
                control_ok,
                projects.len(),
                tasks.len()
            ))
        }
        "report" => {
            let snapshot = HepaFleetMonitor::refresh(
                &observations,
                HepaResourceSample {
                    active_lanes: lanes.len() as u32,
                    memory_mb: 0,
                },
            );
            Ok(format!(
                "HEPA fleet report: projects={} tasks={} lanes={} drifted_lanes={}",
                projects.len(),
                tasks.len(),
                snapshot.lanes.len(),
                snapshot.drifted_lanes.len()
            ))
        }
        "cleanup" => {
            let runtime_root = control_root.join("fleet").join("runtime");
            let terminal_lanes: Vec<String> = tasks
                .iter()
                .filter(|task| {
                    matches!(
                        task.status,
                        HepaTaskStatus::Completed | HepaTaskStatus::Cancelled
                    )
                })
                .flat_map(|task| task.lane_ids.clone())
                .collect();
            let report = HepaFleetMonitor::cleanup_runtime(&runtime_root, &terminal_lanes)
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA fleet cleanup: removed={} preserved={}",
                report.removed_runtime_dirs.len(),
                report.preserved_unrelated.len()
            ))
        }
        "reconcile" => {
            let report = HepaFleetMonitor::reconcile(&observations);
            Ok(format!(
                "HEPA fleet reconcile: actions={}",
                report.actions.len()
            ))
        }
        "stress-run" => fleet_stress_run(&control_root),
        "live-matrix" => fleet_live_matrix(&control_root, flags),
        "dashboard" => {
            let output = take_option(&mut flags, "--output")?
                .map(PathBuf::from)
                .unwrap_or_else(|| control_root.join("fleet/dashboard/index.html"));
            if !flags.is_empty() {
                return Err(format!(
                    "unknown fleet dashboard flags: {}",
                    flags.join(" ")
                ));
            }
            let snapshot = desktop_dashboard_snapshot(&control_root, &projects, &tasks)?;
            let json_path = write_desktop_dashboard(&output, &snapshot)?;
            Ok(format!(
                "HEPA fleet dashboard: html={} json={} projects={} tasks={} lanes={} hermes={}",
                output.display(),
                json_path.display(),
                snapshot.project_count,
                snapshot.task_count,
                snapshot.lane_count,
                snapshot.hermes_status
            ))
        }
        other => Err(format!("unknown fleet command: {other}")),
    }
}

#[derive(Debug, Clone)]
struct LiveMatrixJob {
    label: String,
    run_nonce: String,
    repo_path: PathBuf,
    task_text: String,
}

#[derive(Debug, Serialize)]
struct LiveMatrixSummary {
    schema_version: u32,
    agent: String,
    max_concurrency: usize,
    job_count: usize,
    succeeded: usize,
    failed: usize,
    jobs: Vec<LiveMatrixJobSummary>,
}

#[derive(Debug, Serialize)]
struct LiveMatrixJobSummary {
    label: String,
    repo_ref: String,
    status: String,
    run_id: Option<String>,
    lane_id: Option<String>,
    pr_url: Option<String>,
    wall_seconds: f64,
    error: Option<String>,
}

fn fleet_live_matrix(control_root: &Path, mut flags: Vec<String>) -> Result<String, String> {
    let agent = take_option(&mut flags, "--agent")?.unwrap_or_else(|| "pi".to_string());
    let explicit_max_concurrency = take_option(&mut flags, "--max-concurrency")?
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| "--max-concurrency must be a number".to_string())?;
    let evidence_dir = take_option(&mut flags, "--evidence-dir")?
        .map(PathBuf::from)
        .unwrap_or_else(|| control_root.join("fleet/live-matrix"));
    let mut raw_jobs = Vec::new();
    while let Some(job) = take_option(&mut flags, "--job")? {
        raw_jobs.push(job);
    }
    if !flags.is_empty() {
        return Err(format!(
            "unknown fleet live-matrix flags: {}",
            flags.join(" ")
        ));
    }
    if raw_jobs.is_empty() {
        return Err("fleet live-matrix requires at least one --job <repo>::<task>".to_string());
    }
    let max_concurrency = explicit_max_concurrency
        .unwrap_or_else(|| adapter_max_concurrency(&agent).unwrap_or(1).max(1))
        .max(1);
    let run_nonce = cli_timestamp();
    let jobs = raw_jobs
        .iter()
        .enumerate()
        .map(|(index, raw)| parse_live_matrix_job(index + 1, &run_nonce, raw))
        .collect::<Result<Vec<_>, _>>()?;
    let started = std::time::Instant::now();
    let mut summaries = Vec::new();
    for chunk in jobs.chunks(max_concurrency) {
        let handles = chunk
            .iter()
            .cloned()
            .map(|job| {
                let agent = agent.clone();
                thread::spawn(move || run_live_matrix_job(job, &agent))
            })
            .collect::<Vec<_>>();
        for handle in handles {
            summaries.push(
                handle
                    .join()
                    .map_err(|_| "fleet live-matrix worker panicked".to_string())?,
            );
        }
    }
    let succeeded = summaries
        .iter()
        .filter(|summary| summary.status == "completed")
        .count();
    let failed = summaries.len().saturating_sub(succeeded);
    fs::create_dir_all(&evidence_dir).map_err(|error| error.to_string())?;
    let summary = LiveMatrixSummary {
        schema_version: CONTRACT_SCHEMA_VERSION,
        agent: agent.clone(),
        max_concurrency,
        job_count: summaries.len(),
        succeeded,
        failed,
        jobs: summaries,
    };
    let summary_path = evidence_dir.join(format!("live-matrix-{}-{}.json", agent, cli_timestamp()));
    let json = serde_json::to_string_pretty(&summary).map_err(|error| error.to_string())?;
    fs::write(&summary_path, json).map_err(|error| error.to_string())?;
    let message = format!(
        "HEPA fleet live-matrix: agent={} jobs={} succeeded={} failed={} max_concurrency={} elapsed={:.3}s summary={}",
        agent,
        summary.job_count,
        summary.succeeded,
        summary.failed,
        summary.max_concurrency,
        started.elapsed().as_secs_f64(),
        summary_path.display()
    );
    if summary.failed == 0 {
        Ok(message)
    } else {
        Err(message)
    }
}

fn adapter_max_concurrency(agent: &str) -> Option<usize> {
    let config =
        HepaConfig::load_from_env_and_dotenv_file(".env", HepaConfigOverrides::default()).ok()?;
    let registry = HepaAdapterRegistry::load_from_config(&config).ok()?;
    registry
        .get(agent)
        .map(|spec| spec.max_concurrency as usize)
}

fn parse_live_matrix_job(
    index: usize,
    run_nonce: &str,
    raw: &str,
) -> Result<LiveMatrixJob, String> {
    let (repo, task) = raw
        .split_once("::")
        .ok_or_else(|| "--job must use <repo>::<task>".to_string())?;
    if repo.trim().is_empty() || task.trim().is_empty() {
        return Err("--job repo and task must not be empty".to_string());
    }
    Ok(LiveMatrixJob {
        label: format!("job-{index}"),
        run_nonce: run_nonce.to_string(),
        repo_path: PathBuf::from(repo),
        task_text: task.to_string(),
    })
}

fn run_live_matrix_job(job: LiveMatrixJob, agent: &str) -> LiveMatrixJobSummary {
    let started = std::time::Instant::now();
    let run_config = HepaFakeRunConfig {
        control_root: job.repo_path.join(".hepa/control"),
        worktree_root: job.repo_path.join(".hepa/worktrees"),
        archive_root: job.repo_path.join(".hepa/archive"),
        repo_path: job.repo_path.clone(),
        run_id: format!("run-fleet-live-matrix-{}", job.run_nonce),
        task_id: format!("task-{}", job.label),
        lane_id: format!("lane-{}-{}", job.label, job.run_nonce),
        task_text: job.task_text,
        timing: true,
    };
    match run_live_task(&run_config, agent) {
        Ok(result) => LiveMatrixJobSummary {
            label: job.label,
            repo_ref: "<validation-repo>".to_string(),
            status: result.status,
            run_id: Some(result.run_id),
            lane_id: Some(result.lane_id),
            pr_url: result.terminal_report.pr_url,
            wall_seconds: started.elapsed().as_secs_f64(),
            error: None,
        },
        Err(error) => LiveMatrixJobSummary {
            label: job.label,
            repo_ref: "<validation-repo>".to_string(),
            status: "failed".to_string(),
            run_id: Some(run_config.run_id),
            lane_id: Some(run_config.lane_id),
            pr_url: None,
            wall_seconds: started.elapsed().as_secs_f64(),
            error: Some(error),
        },
    }
}

/// Dispatch `hepa lane <list|show|logs|attach|stop>`. `lane send` is handled by the
/// tmux steering path in `main`.
pub fn lane_command(args: &[String]) -> Result<String, String> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| "usage: hepa lane <list|show|logs|attach|send|stop>".to_string())?;
    let (control_root, flags) = take_control_root(rest)?;
    let registry = HepaFleetRegistry::new(&control_root);
    let tasks = registry.list_tasks().map_err(|error| error.to_string())?;

    match subcommand.as_str() {
        "list" => {
            let mut lines = Vec::new();
            for task in &tasks {
                for lane_id in &task.lane_ids {
                    lines.push(format!(
                        "{lane_id} task={} status={:?}",
                        task.task_id, task.status
                    ));
                }
            }
            if lines.is_empty() {
                return Ok("HEPA lane list: no lanes".to_string());
            }
            lines.sort();
            Ok(format!("HEPA lane list:\n{}", lines.join("\n")))
        }
        "show" => {
            let lane_id = positional(&flags, 0, "lane id")?;
            match tasks
                .iter()
                .find(|task| task.lane_ids.iter().any(|id| id == &lane_id))
            {
                Some(task) => Ok(format!(
                    "HEPA lane show: {lane_id} task={} status={:?}",
                    task.task_id, task.status
                )),
                None => Err(format!("lane not found: {lane_id}")),
            }
        }
        "logs" => {
            let lane_id = positional(&flags, 0, "lane id")?;
            let mut flags = flags;
            let tail = take_option(&mut flags, "--tail")?
                .map(|value| {
                    value
                        .parse::<usize>()
                        .map_err(|_| "--tail must be a non-negative integer".to_string())
                })
                .transpose()?;
            if flags.len() > 1 {
                return Err(format!("unknown lane logs flags: {}", flags[1..].join(" ")));
            }
            let log_path = control_root
                .join("fleet")
                .join("lanes")
                .join(&lane_id)
                .join("lane.log");
            let streams = find_lane_stream_logs(&control_root, &lane_id)
                .map_err(|error| error.to_string())?;
            let mut lines = vec![format!(
                "HEPA lane logs: {lane_id} log={}",
                log_path.display()
            )];
            for stream in &streams {
                lines.push(format!("stream={}", stream.display()));
                if let Some(tail) = tail {
                    for line in tail_file_lines(stream, tail)? {
                        lines.push(format!("  {line}"));
                    }
                }
            }
            if streams.is_empty() {
                lines.push("streams=none".to_string());
            }
            Ok(lines.join("\n"))
        }
        "attach" => {
            let lane_id = positional(&flags, 0, "lane id")?;
            let mut flags = flags;
            let tail = take_option(&mut flags, "--tail")?
                .map(|value| {
                    value
                        .parse::<usize>()
                        .map_err(|_| "--tail must be a non-negative integer".to_string())
                })
                .transpose()?
                .unwrap_or(50);
            if flags.len() > 1 {
                return Err(format!(
                    "unknown lane attach flags: {}",
                    flags[1..].join(" ")
                ));
            }
            let streams = find_lane_stream_logs(&control_root, &lane_id)
                .map_err(|error| error.to_string())?;
            let mut lines = vec![
                format!("HEPA lane attach: {lane_id}"),
                format!(
                    "watch_hint=watch -n 2 'hepa lane attach {lane_id} --control-root {} --tail {tail}'",
                    control_root.display()
                ),
            ];
            for stream in &streams {
                lines.push(format!("stream={}", stream.display()));
                for line in tail_file_lines(stream, tail)? {
                    lines.push(format!("  {line}"));
                }
            }
            if streams.is_empty() {
                lines.push("streams=none".to_string());
            }
            Ok(lines.join("\n"))
        }
        "stop" => {
            let lane_id = positional(&flags, 0, "lane id")?;
            let task = tasks
                .iter()
                .find(|task| task.lane_ids.iter().any(|id| id == &lane_id))
                .ok_or_else(|| format!("lane not found: {lane_id}"))?;
            let updated = registry
                .block_task(&task.task_id, &cli_timestamp())
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA lane stop: {lane_id} task={} -> {:?}",
                updated.task_id, updated.status
            ))
        }
        other => Err(format!("unknown lane command: {other}")),
    }
}

fn find_lane_stream_logs(control_root: &Path, lane_id: &str) -> io::Result<Vec<PathBuf>> {
    let mut matches = Vec::new();
    collect_lane_stream_logs(control_root, lane_id, &mut matches)?;
    matches.sort();
    Ok(matches)
}

fn collect_lane_stream_logs(
    directory: &Path,
    lane_id: &str,
    matches: &mut Vec<PathBuf>,
) -> io::Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_lane_stream_logs(&path, lane_id, matches)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl")
            && path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|value| value.to_str())
                == Some("streams")
            && path
                .components()
                .any(|component| component.as_os_str() == lane_id)
        {
            matches.push(path);
        }
    }
    Ok(())
}

fn tail_file_lines(path: &Path, tail: usize) -> Result<Vec<String>, String> {
    if tail == 0 {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read stream {}: {error}", path.display()))?;
    let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    let start = lines.len().saturating_sub(tail);
    Ok(lines[start..].to_vec())
}

fn scheduler_state_path(control_root: &Path) -> PathBuf {
    control_root.join("fleet").join("scheduler-state")
}

fn read_scheduler_running(control_root: &Path) -> bool {
    std::fs::read_to_string(scheduler_state_path(control_root))
        .map(|value| value.trim() == "running")
        .unwrap_or(false)
}

fn write_scheduler_running(control_root: &Path, running: bool) -> Result<(), String> {
    let path = scheduler_state_path(control_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(path, if running { "running" } else { "stopped" })
        .map_err(|error| error.to_string())
}

/// Active lanes are tasks the registry records as running.
fn active_lanes(registry: &HepaFleetRegistry) -> Result<Vec<HepaActiveLaneSummary>, String> {
    use hepa_core::contracts::HepaTaskStatus;
    Ok(registry
        .list_tasks()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|task| task.status == HepaTaskStatus::Running)
        .map(|task| HepaActiveLaneSummary {
            lane_id: task
                .lane_ids
                .first()
                .cloned()
                .unwrap_or(task.task_id.clone()),
            task_id: task.task_id,
        })
        .collect())
}

/// Dispatch `hepa scheduler ...`.
pub fn scheduler_command(args: &[String]) -> Result<String, String> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| "usage: hepa scheduler <tick|start|stop|status>".to_string())?;
    let (control_root, _flags) = take_control_root(rest)?;
    let registry = HepaFleetRegistry::new(&control_root);
    let limits = HepaSchedulerLimits {
        max_parallel_lanes: 4,
    };

    match subcommand.as_str() {
        "start" => {
            write_scheduler_running(&control_root, true)?;
            Ok("HEPA scheduler start: running".to_string())
        }
        "stop" => {
            write_scheduler_running(&control_root, false)?;
            Ok("HEPA scheduler stop: stopped".to_string())
        }
        "status" => {
            let mut scheduler = HepaScheduler::new();
            if read_scheduler_running(&control_root) {
                scheduler.start();
            }
            let active = active_lanes(&registry)?;
            let status = scheduler
                .status(&registry, &limits, &active)
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "HEPA scheduler status: run_state={:?} ready={} running={} blocked={} queued={} active_lanes={} waits={}",
                status.run_state,
                status.ready,
                status.running,
                status.blocked,
                status.queued,
                status.active_lanes,
                status.waits.len()
            ))
        }
        "tick" => {
            let mut scheduler = HepaScheduler::new();
            if read_scheduler_running(&control_root) {
                scheduler.start();
            }
            let active = active_lanes(&registry)?;
            let outcome = scheduler
                .tick(&registry, &limits, &active)
                .map_err(|error| error.to_string())?;
            Ok(match outcome {
                HepaTickOutcome::NotRunning => {
                    "HEPA scheduler tick: not running (run hepa scheduler start)".to_string()
                }
                HepaTickOutcome::Idle => "HEPA scheduler tick: idle (no ready tasks)".to_string(),
                HepaTickOutcome::Claimable { task_id } => {
                    format!("HEPA scheduler tick: claimable task={task_id}")
                }
                HepaTickOutcome::Waiting { waits } => {
                    format!("HEPA scheduler tick: waiting tasks={}", waits.len())
                }
            })
        }
        other => Err(format!("unknown scheduler command: {other}")),
    }
}

fn positional(flags: &[String], index: usize, label: &str) -> Result<String, String> {
    flags
        .get(index)
        .cloned()
        .ok_or_else(|| format!("{label} is required"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-cli-fleet-{label}-{nonce}"))
    }

    fn s(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn project_add_list_show_doctor_and_remove() {
        let root = unique_test_dir("project");
        let control = root.to_str().expect("path is UTF-8").to_string();

        let add = project_command(&s(&[
            "add",
            "project-1",
            "<REPO>",
            "--name",
            "Demo",
            "--control-root",
            &control,
        ]))
        .expect("add");
        assert!(add.contains("registered project-1"));

        let list = project_command(&s(&["list", "--control-root", &control])).expect("list");
        assert!(list.contains("project-1"));

        let show =
            project_command(&s(&["show", "project-1", "--control-root", &control])).expect("show");
        assert!(show.contains("name=Demo"));

        let doctor = project_command(&s(&["doctor", "project-1", "--control-root", &control]))
            .expect("doctor");
        assert!(doctor.contains("registered=yes"));

        let remove = project_command(&s(&["remove", "project-1", "--control-root", &control]))
            .expect("remove");
        assert!(remove.contains("removed project-1"));
        assert!(project_command(&s(&["show", "project-1", "--control-root", &control])).is_err());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn task_lifecycle_commands_drive_the_registry() {
        let root = unique_test_dir("task");
        let control = root.to_str().expect("path is UTF-8").to_string();

        let create = task_command(&s(&[
            "create",
            "project-1",
            "task-1",
            "Fix login",
            "--control-root",
            &control,
        ]))
        .expect("create");
        assert!(create.contains("created task-1"));

        assert!(
            task_command(&s(&["list", "--control-root", &control]))
                .expect("list")
                .contains("task-1")
        );

        let block =
            task_command(&s(&["block", "task-1", "--control-root", &control])).expect("block");
        assert!(block.contains("Blocked"));
        let resume =
            task_command(&s(&["resume", "task-1", "--control-root", &control])).expect("resume");
        assert!(resume.contains("Queued"));
        let prioritize = task_command(&s(&[
            "prioritize",
            "task-1",
            "7",
            "--control-root",
            &control,
        ]))
        .expect("prioritize");
        assert!(prioritize.contains("priority=7"));
        let cancel =
            task_command(&s(&["cancel", "task-1", "--control-root", &control])).expect("cancel");
        assert!(cancel.contains("Cancelled"));

        let sync = task_command(&s(&["sync-kanban", "--control-root", &control])).expect("sync");
        assert!(sync.contains("tasks=1"));
        assert!(sync.contains("degraded"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn hermes_ingest_spec_creates_ready_cards_and_dry_run_attach_hints() {
        let root = unique_test_dir("hermes");
        let control = root.join("control");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        let spec = root.join("roadmap.md");
        std::fs::write(
            &spec,
            r#"
Project: ignored-project

## Task: task-alpha: Add analytics pipeline
Implement a small analytics pipeline across source and script files.
Acceptance:
- Analytics events are captured.
- Validation documents the command.
Validation:
- pnpm test

## Task: task-beta: Add reviewer dashboard
Implement a dashboard for review state.
Acceptance:
- Dashboard renders the review state.
Validation:
- pnpm lint
"#,
        )
        .expect("spec");

        let ingest = hermes_command(&s(&[
            "ingest-spec",
            "todo-project",
            repo.to_str().expect("repo path"),
            spec.to_str().expect("spec path"),
            "--control-root",
            control.to_str().expect("control path"),
            "--max-parallel",
            "2",
        ]))
        .expect("ingest");
        assert!(ingest.contains("tasks=2"));
        assert!(ingest.contains("ready=2"));
        assert!(ingest.contains("hermes-todo-project-task-alpha"));

        let cards = hermes_command(&s(&[
            "cards",
            "--control-root",
            control.to_str().expect("control"),
        ]))
        .expect("cards");
        assert!(cards.contains("task-alpha"));
        assert!(cards.contains("todo-project"));

        let dry_run = hermes_command(&s(&[
            "run-ready",
            "todo-project",
            "--dry-run",
            "--limit",
            "1",
            "--max-concurrency",
            "1",
            "--control-root",
            control.to_str().expect("control path"),
        ]))
        .expect("dry run");
        assert!(dry_run.contains("HEPA hermes dry-run"));
        assert!(dry_run.contains("hepa lane attach"));

        let card_path = local_hermes_card_path(&control, "hermes-todo-project-task-alpha");
        let card_json = std::fs::read_to_string(card_path).expect("card json");
        assert!(card_json.contains("lane_attach_commands"));
        assert!(card_json.contains("fleet_watch_command"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn hermes_dashboard_card_imports_into_hepa_dry_run() {
        let root = unique_test_dir("hermes-dashboard");
        let control = root.join("control");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        let task_json = root.join("dashboard-task.json");
        std::fs::write(
            &task_json,
            r#"
{
  "task": {
    "id": "t_abc123",
    "title": "Implement route parity tests",
    "body": "Write gateway route parity tests.\n\nAcceptance criteria:\n- Route tests cover auth, user, and todo routes.\n- Tests use deterministic mocks.\n\nValidation:\n- pnpm test --filter gateway",
    "priority": 3
  },
  "parents": [],
  "children": []
}
"#,
        )
        .expect("task json");

        let output = hermes_command(&s(&[
            "run-dashboard-card",
            "todo-project",
            repo.to_str().expect("repo"),
            "t_abc123",
            "--task-json",
            task_json.to_str().expect("task json path"),
            "--dry-run",
            "--no-hermes-update",
            "--control-root",
            control.to_str().expect("control path"),
        ]))
        .expect("dashboard dry run");
        assert!(output.contains("HEPA hermes dry-run"));
        assert!(output.contains("t_abc123"));
        assert!(output.contains("card=t_abc123"));

        let spec_path = local_hermes_spec_path(&control, "t_abc123");
        let spec_json = std::fs::read_to_string(spec_path).expect("spec json");
        assert!(spec_json.contains("Route tests cover auth"));
        assert!(spec_json.contains("pnpm test --filter gateway"));

        task_command(&s(&[
            "block",
            "t_abc123",
            "--control-root",
            control.to_str().expect("control path"),
        ]))
        .expect("block imported dashboard task");
        let rerun = hermes_command(&s(&[
            "run-dashboard-card",
            "todo-project",
            repo.to_str().expect("repo"),
            "t_abc123",
            "--task-json",
            task_json.to_str().expect("task json path"),
            "--dry-run",
            "--no-hermes-update",
            "--control-root",
            control.to_str().expect("control path"),
        ]))
        .expect("dashboard dry rerun");
        assert!(rerun.contains("selected=1"));

        let registry = HepaFleetRegistry::new(&control);
        registry
            .claim_task_into_lane("t_abc123", "stale-lane", "t-stale")
            .expect("claim stale lane");
        let stale_rerun = hermes_command(&s(&[
            "run-dashboard-card",
            "todo-project",
            repo.to_str().expect("repo"),
            "t_abc123",
            "--task-json",
            task_json.to_str().expect("task json path"),
            "--dry-run",
            "--no-hermes-update",
            "--control-root",
            control.to_str().expect("control path"),
        ]))
        .expect("dashboard dry rerun from stale running");
        assert!(stale_rerun.contains("selected=1"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fleet_status_doctor_report_cleanup_and_reconcile() {
        let root = unique_test_dir("fleet");
        let control = root.to_str().expect("path is UTF-8").to_string();
        let registry = HepaFleetRegistry::new(&root);
        let mut task = HepaFleetTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            title: "Task".to_string(),
            description: String::new(),
            status: HepaTaskStatus::Ready,
            readiness: HepaReadinessState::Ready,
            dependencies: Vec::new(),
            lane_ids: Vec::new(),
            external_card_id: None,
            priority: 1,
            created_at: "t0".to_string(),
            updated_at: "t0".to_string(),
            completed_at: None,
        };
        registry.create_task(&task).expect("create");
        task = registry
            .claim_task_into_lane("task-1", "lane-1", "t1")
            .expect("claim");
        assert_eq!(task.status, HepaTaskStatus::Running);

        let status = fleet_command(&s(&["status", "--control-root", &control])).expect("status");
        assert!(status.contains("running_lanes=1"));

        assert!(
            fleet_command(&s(&["doctor", "--control-root", &control]))
                .expect("doctor")
                .contains("control_root_ok=true")
        );
        assert!(
            fleet_command(&s(&["report", "--control-root", &control]))
                .expect("report")
                .contains("lanes=1")
        );
        // Running lane with no card produces a reconcile action.
        let reconcile =
            fleet_command(&s(&["reconcile", "--control-root", &control])).expect("reconcile");
        assert!(reconcile.contains("actions=1"));

        let cleanup = fleet_command(&s(&["cleanup", "--control-root", &control])).expect("cleanup");
        assert!(cleanup.contains("removed=0"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fleet_live_matrix_requires_jobs_and_writes_failure_summary() {
        let root = unique_test_dir("live-matrix");
        let control = root.join("control");
        let evidence = root.join("evidence");
        let control_arg = control.to_str().expect("control path is UTF-8");
        let evidence_arg = evidence.to_str().expect("evidence path is UTF-8");

        let missing_job = fleet_command(&s(&["live-matrix", "--control-root", control_arg]))
            .expect_err("live matrix requires jobs");
        assert!(missing_job.contains("requires at least one --job"));

        let missing_repo = root.join("missing-repo");
        let job = format!(
            "{}::Update README.md and run git diff --check.",
            missing_repo.display()
        );
        let error = fleet_command(&s(&[
            "live-matrix",
            "--control-root",
            control_arg,
            "--evidence-dir",
            evidence_arg,
            "--agent",
            "pi",
            "--max-concurrency",
            "1",
            "--job",
            &job,
        ]))
        .expect_err("missing repo should fail the run but write summary");

        assert!(error.contains("failed=1"));
        let summary_path = std::fs::read_dir(&evidence)
            .expect("evidence dir exists")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .expect("summary json written");
        let summary = std::fs::read_to_string(summary_path).expect("summary readable");
        assert!(summary.contains("\"failed\": 1"));
        assert!(summary.contains("\"status\": \"failed\""));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fleet_dashboard_writes_static_desktop_snapshot() {
        let root = unique_test_dir("dashboard");
        let control = root.to_str().expect("path is UTF-8").to_string();
        let registry = HepaFleetRegistry::new(&root);
        registry
            .register_project(&HepaRegisteredProject {
                project: HepaProject {
                    schema_version: CONTRACT_SCHEMA_VERSION,
                    project_id: "project-1".to_string(),
                    display_name: "Demo <Project>".to_string(),
                    repo_ref: "<REPO_A>".to_string(),
                    default_branch: "main".to_string(),
                    routing_policy_ref: None,
                    is_active: true,
                    created_at: "t0".to_string(),
                    updated_at: "t0".to_string(),
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
            })
            .expect("register project");
        registry
            .create_task(&HepaFleetTask {
                schema_version: CONTRACT_SCHEMA_VERSION,
                task_id: "task-1".to_string(),
                project_id: "project-1".to_string(),
                title: "Fix <dashboard>".to_string(),
                description: String::new(),
                status: HepaTaskStatus::Ready,
                readiness: HepaReadinessState::Ready,
                dependencies: Vec::new(),
                lane_ids: vec!["lane-1".to_string()],
                external_card_id: Some("card-1".to_string()),
                priority: 5,
                created_at: "t0".to_string(),
                updated_at: "t0".to_string(),
                completed_at: None,
            })
            .expect("create task");
        let stream_dir = root.join("runs/run-1/tasks/task-1/lanes/lane-1/streams");
        std::fs::create_dir_all(&stream_dir).expect("stream dir");
        std::fs::write(
            stream_dir.join("manager-tool-summary-stream.jsonl"),
            "{\"event\":\"tool_activity_summary\",\"tool_event_count\":2}\n",
        )
        .expect("stream fixture");
        scheduler_command(&s(&["start", "--control-root", &control])).expect("start scheduler");

        let html_path = root.join("desktop").join("index.html");
        let output = fleet_command(&s(&[
            "dashboard",
            "--output",
            html_path.to_str().expect("path is UTF-8"),
            "--control-root",
            &control,
        ]))
        .expect("dashboard");

        assert!(output.contains("projects=1"));
        assert!(output.contains("tasks=1"));
        assert!(html_path.exists());
        let html = std::fs::read_to_string(&html_path).expect("read html");
        assert!(html.contains("HEPA Fleet Dashboard"));
        assert!(html.contains("Demo &lt;Project&gt;"));
        assert!(html.contains("Fix &lt;dashboard&gt;"));
        assert!(html.contains("Lane Streams"));
        assert!(html.contains("manager-tool-summary-stream.jsonl"));
        assert!(html.contains("tool_activity_summary"));
        assert!(!html.contains("<REPO_A>"));

        let json_path = dashboard_json_path(&html_path);
        let json = std::fs::read_to_string(json_path).expect("read json");
        assert!(json.contains("\"surface\": \"desktop-dashboard-snapshot\""));
        assert!(json.contains("\"run_state\": \"Running\""));
        assert!(json.contains("\"card_configured\": true"));
        assert!(json.contains("\"lane_streams\""));
        assert!(json.contains("manager-tool-summary-stream.jsonl"));
        assert!(!json.contains("<REPO_A>"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn lane_list_show_logs_and_stop() {
        let root = unique_test_dir("lane");
        let control = root.to_str().expect("path is UTF-8").to_string();
        let registry = HepaFleetRegistry::new(&root);
        // A ready task claimed into a lane gives us a lane to inspect.
        let mut task = HepaFleetTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            title: "Task".to_string(),
            description: String::new(),
            status: HepaTaskStatus::Ready,
            readiness: HepaReadinessState::Ready,
            dependencies: Vec::new(),
            lane_ids: Vec::new(),
            external_card_id: None,
            priority: 1,
            created_at: "t0".to_string(),
            updated_at: "t0".to_string(),
            completed_at: None,
        };
        registry.create_task(&task).expect("create");
        task = registry
            .claim_task_into_lane("task-1", "lane-1", "t1")
            .expect("claim");
        assert_eq!(task.lane_ids, vec!["lane-1".to_string()]);
        let stream_dir = root.join("runs/run-1/tasks/task-1/lanes/lane-1/streams");
        std::fs::create_dir_all(&stream_dir).expect("stream dir");
        std::fs::write(
            stream_dir.join("worker-adapter-stream.jsonl"),
            "{\"stream\":\"stdout\",\"text\":\"first\"}\n{\"stream\":\"stderr\",\"text\":\"second\"}\n",
        )
        .expect("stream fixture");

        assert!(
            lane_command(&s(&["list", "--control-root", &control]))
                .expect("list")
                .contains("lane-1")
        );
        assert!(
            lane_command(&s(&["show", "lane-1", "--control-root", &control]))
                .expect("show")
                .contains("task=task-1")
        );
        assert!(
            lane_command(&s(&["logs", "lane-1", "--control-root", &control]))
                .expect("logs")
                .contains("lane.log")
        );
        let logs = lane_command(&s(&[
            "logs",
            "lane-1",
            "--tail",
            "1",
            "--control-root",
            &control,
        ]))
        .expect("logs tail");
        assert!(logs.contains("worker-adapter-stream.jsonl"));
        assert!(logs.contains("\"text\":\"second\""));
        assert!(!logs.contains("\"text\":\"first\""));
        let attach = lane_command(&s(&[
            "attach",
            "lane-1",
            "--tail",
            "1",
            "--control-root",
            &control,
        ]))
        .expect("attach");
        assert!(attach.contains("HEPA lane attach: lane-1"));
        assert!(attach.contains("watch_hint="));
        assert!(attach.contains("\"text\":\"second\""));
        let stop = lane_command(&s(&["stop", "lane-1", "--control-root", &control])).expect("stop");
        assert!(stop.contains("Blocked"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scheduler_start_status_tick_and_stop() {
        let root = unique_test_dir("scheduler");
        let control = root.to_str().expect("path is UTF-8").to_string();

        // Stopped by default: tick reports not running.
        let idle_tick = scheduler_command(&s(&["tick", "--control-root", &control])).expect("tick");
        assert!(idle_tick.contains("not running"));

        assert!(
            scheduler_command(&s(&["start", "--control-root", &control]))
                .expect("start")
                .contains("running")
        );

        let status =
            scheduler_command(&s(&["status", "--control-root", &control])).expect("status");
        assert!(status.contains("run_state=Running"));

        // A running scheduler with no ready tasks is idle.
        let tick = scheduler_command(&s(&["tick", "--control-root", &control])).expect("tick");
        assert!(tick.contains("idle"));

        assert!(
            scheduler_command(&s(&["stop", "--control-root", &control]))
                .expect("stop")
                .contains("stopped")
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn project_show_missing_errors() {
        let root = unique_test_dir("project-missing");
        let control = root.to_str().expect("path is UTF-8").to_string();
        assert!(project_command(&s(&["show", "absent", "--control-root", &control])).is_err());
        std::fs::remove_dir_all(&root).ok();
    }
}
