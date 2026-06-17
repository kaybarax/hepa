//! CLI handlers for the fleet command groups (project/task/scheduler/fleet),
//! all backed by the deterministic, temp-root-safe `HepaFleetRegistry`.

use hepa_core::contracts::{
    CONTRACT_SCHEMA_VERSION, HepaFleetTask, HepaProject, HepaReadinessState, HepaTaskStatus,
};
use hepa_core::fleet_registry::{
    HepaCostClass, HepaCostPolicy, HepaFleetRegistry, HepaMemoryPolicy, HepaRegisteredProject,
};
use std::path::PathBuf;
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
    fn project_show_missing_errors() {
        let root = unique_test_dir("project-missing");
        let control = root.to_str().expect("path is UTF-8").to_string();
        assert!(project_command(&s(&["show", "absent", "--control-root", &control])).is_err());
        std::fs::remove_dir_all(&root).ok();
    }
}
