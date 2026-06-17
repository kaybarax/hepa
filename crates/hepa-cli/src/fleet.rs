//! CLI handlers for the fleet command groups (project/task/scheduler/fleet),
//! all backed by the deterministic, temp-root-safe `HepaFleetRegistry`.

use hepa_core::contracts::{
    CONTRACT_SCHEMA_VERSION, HepaFleetTask, HepaProject, HepaReadinessState, HepaTaskStatus,
};
use hepa_core::fleet_registry::{
    HepaCostClass, HepaCostPolicy, HepaFleetRegistry, HepaMemoryPolicy, HepaRegisteredProject,
};
use hepa_core::scheduler::{
    HepaActiveLaneSummary, HepaScheduler, HepaSchedulerLimits, HepaTickOutcome,
};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
        "usage: hepa task <create|list|show|cancel|block|resume|prioritize|sync-kanban>".to_string()
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
