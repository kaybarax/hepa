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
    card_mapping::HepaHermesCardMappingInput,
    sync::{HepaKanbanSyncEngine, HepaMemoryHermesCardStore},
};
use serde::Serialize;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    fs,
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
    })
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
        "usage: hepa fleet <status|doctor|report|cleanup|reconcile|dashboard|live-matrix>"
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

/// Dispatch `hepa lane <list|show|logs|stop>`. `lane send` is handled by the
/// tmux steering path in `main`.
pub fn lane_command(args: &[String]) -> Result<String, String> {
    let (subcommand, rest) = args
        .split_first()
        .ok_or_else(|| "usage: hepa lane <list|show|logs|send|stop>".to_string())?;
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
            let log_path = control_root
                .join("fleet")
                .join("lanes")
                .join(&lane_id)
                .join("lane.log");
            Ok(format!(
                "HEPA lane logs: {lane_id} log={}",
                log_path.display()
            ))
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
                lane_ids: Vec::new(),
                external_card_id: Some("card-1".to_string()),
                priority: 5,
                created_at: "t0".to_string(),
                updated_at: "t0".to_string(),
                completed_at: None,
            })
            .expect("create task");
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
        assert!(!html.contains("<REPO_A>"));

        let json_path = dashboard_json_path(&html_path);
        let json = std::fs::read_to_string(json_path).expect("read json");
        assert!(json.contains("\"surface\": \"desktop-dashboard-snapshot\""));
        assert!(json.contains("\"run_state\": \"Running\""));
        assert!(json.contains("\"card_configured\": true"));
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
