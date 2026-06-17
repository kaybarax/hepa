use crate::contracts::{
    HepaFleetTask, HepaProject, HepaReadinessState, HepaTaskStatus, HepaValidate,
};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

/// Cost class of a project's lanes for budgeting paid-cloud adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaCostClass {
    Local,
    Paid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaCostPolicy {
    pub cost_class: HepaCostClass,
    pub max_paid_lanes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaMemoryPolicy {
    pub max_resident_models: u32,
}

/// A project registered with the fleet: the base contract plus fleet policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaRegisteredProject {
    pub project: HepaProject,
    pub max_parallel_tasks: u32,
    pub cost_policy: HepaCostPolicy,
    pub memory_policy: HepaMemoryPolicy,
    pub board_metadata: Option<String>,
}

impl HepaValidate for HepaRegisteredProject {
    fn validate(&self) -> Result<(), crate::contracts::HepaContractError> {
        self.project.validate()?;
        if self.max_parallel_tasks == 0 {
            return Err(crate::contracts::HepaContractError {
                field: "max_parallel_tasks".to_string(),
                message: "must be greater than zero".to_string(),
            });
        }
        Ok(())
    }
}

/// Deterministic, temp-root-safe registry of projects and fleet tasks.
///
/// All records live under the control root, so tests can target a temp root and
/// every list operation returns records in a stable id order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaFleetRegistry {
    control_root: PathBuf,
}

impl HepaFleetRegistry {
    pub fn new(control_root: impl Into<PathBuf>) -> Self {
        Self {
            control_root: control_root.into(),
        }
    }

    fn projects_dir(&self) -> PathBuf {
        self.control_root.join("fleet").join("projects")
    }

    fn project_path(&self, project_id: &str) -> PathBuf {
        self.projects_dir().join(format!("{project_id}.json"))
    }

    /// Register or update a project. Invalid repo paths and secret-like fields
    /// are rejected by contract validation before anything is written.
    pub fn register_project(
        &self,
        registration: &HepaRegisteredProject,
    ) -> Result<(), HepaFleetError> {
        registration
            .validate()
            .map_err(|error| HepaFleetError::new(error.field, error.message))?;
        require_safe_segment("project_id", &registration.project.project_id)?;
        require_repo_path("repo_ref", &registration.project.repo_ref)?;
        fs::create_dir_all(self.projects_dir())?;
        write_stable_json(
            &self.project_path(&registration.project.project_id),
            registration,
        )
    }

    /// Show a registered project, or `None` if absent.
    pub fn show_project(
        &self,
        project_id: &str,
    ) -> Result<Option<HepaRegisteredProject>, HepaFleetError> {
        let path = self.project_path(project_id);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(read_json(&path)?))
    }

    /// Remove a registered project. Returns whether a project was removed.
    pub fn remove_project(&self, project_id: &str) -> Result<bool, HepaFleetError> {
        require_safe_segment("project_id", project_id)?;
        let path = self.project_path(project_id);
        if !path.exists() {
            return Ok(false);
        }
        fs::remove_file(path)?;
        Ok(true)
    }

    /// List registered projects in stable `project_id` order.
    pub fn list_projects(&self) -> Result<Vec<HepaRegisteredProject>, HepaFleetError> {
        let mut projects = self.read_records(&self.projects_dir())?;
        projects.sort_by(|left: &HepaRegisteredProject, right| {
            left.project.project_id.cmp(&right.project.project_id)
        });
        Ok(projects)
    }

    fn tasks_dir(&self) -> PathBuf {
        self.control_root.join("fleet").join("tasks")
    }

    fn task_path(&self, task_id: &str) -> PathBuf {
        self.tasks_dir().join(format!("{task_id}.json"))
    }

    /// Create a fleet task. Dependencies and readiness state ride along on the
    /// contract record.
    pub fn create_task(&self, task: &HepaFleetTask) -> Result<(), HepaFleetError> {
        task.validate()
            .map_err(|error| HepaFleetError::new(error.field, error.message))?;
        require_safe_segment("task_id", &task.task_id)?;
        fs::create_dir_all(self.tasks_dir())?;
        write_stable_json(&self.task_path(&task.task_id), task)
    }

    /// Show a task, or `None` if absent.
    pub fn show_task(&self, task_id: &str) -> Result<Option<HepaFleetTask>, HepaFleetError> {
        let path = self.task_path(task_id);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(read_json(&path)?))
    }

    /// List tasks in stable `task_id` order.
    pub fn list_tasks(&self) -> Result<Vec<HepaFleetTask>, HepaFleetError> {
        let mut tasks: Vec<HepaFleetTask> = self.read_records(&self.tasks_dir())?;
        tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
        Ok(tasks)
    }

    /// Block a task, recording the requested status transition.
    pub fn block_task(
        &self,
        task_id: &str,
        updated_at: &str,
    ) -> Result<HepaFleetTask, HepaFleetError> {
        self.set_task_status(task_id, HepaTaskStatus::Blocked, updated_at)
    }

    /// Cancel a task.
    pub fn cancel_task(
        &self,
        task_id: &str,
        updated_at: &str,
    ) -> Result<HepaFleetTask, HepaFleetError> {
        self.set_task_status(task_id, HepaTaskStatus::Cancelled, updated_at)
    }

    /// Complete a task after its lane reaches a terminal done state.
    pub fn complete_task(
        &self,
        task_id: &str,
        updated_at: &str,
    ) -> Result<HepaFleetTask, HepaFleetError> {
        let mut task = self.require_task(task_id)?;
        if !task.status.can_transition_to(&HepaTaskStatus::Completed) {
            return Err(HepaFleetError::new(
                "status",
                format!(
                    "invalid task transition to {}",
                    status_name(&HepaTaskStatus::Completed)
                ),
            ));
        }
        task.status = HepaTaskStatus::Completed;
        task.updated_at = updated_at.to_string();
        task.completed_at = Some(updated_at.to_string());
        self.persist_task(&task)?;
        Ok(task)
    }

    /// Resume a blocked task back into the queue.
    pub fn resume_task(
        &self,
        task_id: &str,
        updated_at: &str,
    ) -> Result<HepaFleetTask, HepaFleetError> {
        self.set_task_status(task_id, HepaTaskStatus::Queued, updated_at)
    }

    /// Set a task's scheduling priority. Terminal tasks cannot be reprioritized.
    pub fn prioritize_task(
        &self,
        task_id: &str,
        priority: u32,
        updated_at: &str,
    ) -> Result<HepaFleetTask, HepaFleetError> {
        let mut task = self.require_task(task_id)?;
        if matches!(
            task.status,
            HepaTaskStatus::Completed | HepaTaskStatus::Cancelled
        ) {
            return Err(HepaFleetError::new(
                "status",
                "terminal tasks cannot be reprioritized",
            ));
        }
        task.priority = priority;
        task.updated_at = updated_at.to_string();
        self.persist_task(&task)?;
        Ok(task)
    }

    fn set_task_status(
        &self,
        task_id: &str,
        next: HepaTaskStatus,
        updated_at: &str,
    ) -> Result<HepaFleetTask, HepaFleetError> {
        let mut task = self.require_task(task_id)?;
        if !task.status.can_transition_to(&next) {
            return Err(HepaFleetError::new(
                "status",
                format!("invalid task transition to {}", status_name(&next)),
            ));
        }
        task.status = next;
        task.updated_at = updated_at.to_string();
        self.persist_task(&task)?;
        Ok(task)
    }

    /// Atomically claim a ready task into exactly one lane.
    ///
    /// The task must currently be `Ready` with no existing lane; the second
    /// claim attempt fails, so a task can never be double-claimed or fan out to
    /// more than one lane.
    pub fn claim_task_into_lane(
        &self,
        task_id: &str,
        lane_id: &str,
        updated_at: &str,
    ) -> Result<HepaFleetTask, HepaFleetError> {
        require_safe_segment("lane_id", lane_id)?;
        let mut task = self.require_task(task_id)?;
        if task.status != HepaTaskStatus::Ready {
            return Err(HepaFleetError::new(
                "status",
                "only ready tasks can be claimed into a lane",
            ));
        }
        if !task.lane_ids.is_empty() {
            return Err(HepaFleetError::new(
                "lane_ids",
                "task is already claimed into a lane",
            ));
        }
        task.status = HepaTaskStatus::Running;
        task.lane_ids = vec![lane_id.to_string()];
        task.updated_at = updated_at.to_string();
        self.persist_task(&task)?;
        Ok(task)
    }

    /// Record a task's readiness state.
    pub fn set_task_readiness(
        &self,
        task_id: &str,
        readiness: HepaReadinessState,
        updated_at: &str,
    ) -> Result<HepaFleetTask, HepaFleetError> {
        let mut task = self.require_task(task_id)?;
        task.readiness = readiness;
        task.updated_at = updated_at.to_string();
        self.persist_task(&task)?;
        Ok(task)
    }

    /// Mark a queued task ready for scheduler claim after readiness checks pass.
    pub fn mark_task_ready(
        &self,
        task_id: &str,
        updated_at: &str,
    ) -> Result<HepaFleetTask, HepaFleetError> {
        let mut task = self.require_task(task_id)?;
        if task.status != HepaTaskStatus::Queued {
            return Err(HepaFleetError::new(
                "status",
                "only queued tasks can be marked ready",
            ));
        }
        task.status = HepaTaskStatus::Ready;
        task.readiness = HepaReadinessState::Ready;
        task.updated_at = updated_at.to_string();
        self.persist_task(&task)?;
        Ok(task)
    }

    /// Dependency task ids that are not yet completed. Unknown dependencies count
    /// as unmet so a task never proceeds on a dangling reference. Result is
    /// deterministically sorted and de-duplicated.
    pub fn unmet_dependencies(&self, task_id: &str) -> Result<Vec<String>, HepaFleetError> {
        let task = self.require_task(task_id)?;
        let mut unmet = Vec::new();
        for dependency in &task.dependencies {
            let completed = matches!(
                self.show_task(dependency)?.map(|task| task.status),
                Some(HepaTaskStatus::Completed)
            );
            if !completed {
                unmet.push(dependency.clone());
            }
        }
        unmet.sort();
        unmet.dedup();
        Ok(unmet)
    }

    fn require_task(&self, task_id: &str) -> Result<HepaFleetTask, HepaFleetError> {
        self.show_task(task_id)?
            .ok_or_else(|| HepaFleetError::new("task_id", "task not found"))
    }

    fn persist_task(&self, task: &HepaFleetTask) -> Result<(), HepaFleetError> {
        task.validate()
            .map_err(|error| HepaFleetError::new(error.field, error.message))?;
        write_stable_json(&self.task_path(&task.task_id), task)
    }

    fn read_records<T>(&self, dir: &Path) -> Result<Vec<T>, HepaFleetError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let mut records = Vec::new();
        if !dir.exists() {
            return Ok(records);
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                records.push(read_json(&path)?);
            }
        }
        Ok(records)
    }
}

#[derive(Debug)]
pub struct HepaFleetError {
    pub field: String,
    pub message: String,
}

impl HepaFleetError {
    pub(crate) fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaFleetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaFleetError {}

impl From<io::Error> for HepaFleetError {
    fn from(error: io::Error) -> Self {
        Self::new("io", error.to_string())
    }
}

impl From<serde_json::Error> for HepaFleetError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("serde_json", error.to_string())
    }
}

fn status_name(status: &HepaTaskStatus) -> &'static str {
    match status {
        HepaTaskStatus::Draft => "draft",
        HepaTaskStatus::Queued => "queued",
        HepaTaskStatus::Ready => "ready",
        HepaTaskStatus::Running => "running",
        HepaTaskStatus::Blocked => "blocked",
        HepaTaskStatus::Cancelled => "cancelled",
        HepaTaskStatus::Completed => "completed",
    }
}

pub(crate) fn require_safe_segment(field: &str, value: &str) -> Result<(), HepaFleetError> {
    if value.trim().is_empty() {
        return Err(HepaFleetError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaFleetError::new(field, "must be a single line"));
    }
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(HepaFleetError::new(
            field,
            "must not contain path separators or traversal",
        ));
    }
    Ok(())
}

fn require_repo_path(field: &str, value: &str) -> Result<(), HepaFleetError> {
    if value.trim().is_empty() {
        return Err(HepaFleetError::new(field, "repo path must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaFleetError::new(
            field,
            "repo path must be a single line",
        ));
    }
    Ok(())
}

pub(crate) fn write_stable_json<T>(path: &Path, value: &T) -> Result<(), HepaFleetError>
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
    fs::write(path, json)?;
    Ok(())
}

pub(crate) fn read_json<T>(path: &Path) -> Result<T, HepaFleetError>
where
    T: for<'de> Deserialize<'de>,
{
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{CONTRACT_SCHEMA_VERSION, HepaReadinessState};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn project(project_id: &str, repo_ref: &str) -> HepaRegisteredProject {
        HepaRegisteredProject {
            project: HepaProject {
                schema_version: CONTRACT_SCHEMA_VERSION,
                project_id: project_id.to_string(),
                display_name: "Demo Project".to_string(),
                repo_ref: repo_ref.to_string(),
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
        }
    }

    #[test]
    fn register_and_list_projects_is_deterministic_and_temp_safe() {
        let root = unique_test_dir("projects");
        let registry = HepaFleetRegistry::new(&root);

        registry
            .register_project(&project("project-b", "<REPO_B>"))
            .expect("register b");
        registry
            .register_project(&project("project-a", "<REPO_A>"))
            .expect("register a");

        let projects = registry.list_projects().expect("list");
        let ids: Vec<&str> = projects
            .iter()
            .map(|registered| registered.project.project_id.as_str())
            .collect();
        assert_eq!(ids, vec!["project-a", "project-b"]);
        // Everything lives under the temp control root.
        assert!(root.join("fleet/projects/project-a.json").exists());

        remove_test_dir(root);
    }

    #[test]
    fn show_missing_project_returns_none() {
        let root = unique_test_dir("show-missing");
        let registry = HepaFleetRegistry::new(&root);

        assert!(registry.show_project("absent").expect("show").is_none());

        remove_test_dir(root);
    }

    #[test]
    fn remove_project_deletes_registration() {
        let root = unique_test_dir("remove-project");
        let registry = HepaFleetRegistry::new(&root);
        registry
            .register_project(&project("project-1", "<REPO_A>"))
            .expect("register");

        assert!(registry.remove_project("project-1").expect("remove"));
        assert!(registry.show_project("project-1").expect("show").is_none());
        // Removing an absent project is a no-op, not an error.
        assert!(!registry.remove_project("project-1").expect("idempotent"));

        remove_test_dir(root);
    }

    #[test]
    fn rejects_secret_like_repo_and_traversal_project_id() {
        let root = unique_test_dir("reject");
        let registry = HepaFleetRegistry::new(&root);

        let secret = registry.register_project(&project("project-1", "repo-with-secret-token"));
        assert!(secret.is_err());

        let traversal = registry.register_project(&project("../escape", "<REPO_A>"));
        assert!(traversal.is_err());

        let zero_parallel = {
            let mut registration = project("project-2", "<REPO_A>");
            registration.max_parallel_tasks = 0;
            registry.register_project(&registration)
        };
        assert!(zero_parallel.is_err());

        remove_test_dir(root);
    }

    fn task(task_id: &str) -> HepaFleetTask {
        HepaFleetTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: task_id.to_string(),
            project_id: "project-1".to_string(),
            title: "Demo task".to_string(),
            description: "Demo task description".to_string(),
            status: HepaTaskStatus::Queued,
            readiness: HepaReadinessState::NotReady,
            dependencies: Vec::new(),
            lane_ids: Vec::new(),
            external_card_id: None,
            priority: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: None,
        }
    }

    #[test]
    fn create_list_and_show_tasks() {
        let root = unique_test_dir("tasks");
        let registry = HepaFleetRegistry::new(&root);

        registry.create_task(&task("task-b")).expect("create b");
        registry.create_task(&task("task-a")).expect("create a");

        let ids: Vec<String> = registry
            .list_tasks()
            .expect("list")
            .into_iter()
            .map(|task| task.task_id)
            .collect();
        assert_eq!(ids, vec!["task-a".to_string(), "task-b".to_string()]);
        assert!(registry.show_task("task-a").expect("show").is_some());
        assert!(registry.show_task("absent").expect("show").is_none());

        remove_test_dir(root);
    }

    #[test]
    fn task_lifecycle_block_resume_prioritize_and_cancel() {
        let root = unique_test_dir("lifecycle");
        let registry = HepaFleetRegistry::new(&root);
        registry.create_task(&task("task-1")).expect("create");

        let blocked = registry
            .block_task("task-1", "2026-06-16T00:01:00Z")
            .expect("block");
        assert_eq!(blocked.status, HepaTaskStatus::Blocked);

        let resumed = registry
            .resume_task("task-1", "2026-06-16T00:02:00Z")
            .expect("resume");
        assert_eq!(resumed.status, HepaTaskStatus::Queued);

        let prioritized = registry
            .prioritize_task("task-1", 9, "2026-06-16T00:03:00Z")
            .expect("prioritize");
        assert_eq!(prioritized.priority, 9);

        let cancelled = registry
            .cancel_task("task-1", "2026-06-16T00:04:00Z")
            .expect("cancel");
        assert_eq!(cancelled.status, HepaTaskStatus::Cancelled);

        remove_test_dir(root);
    }

    #[test]
    fn invalid_transitions_and_terminal_reprioritize_are_rejected() {
        let root = unique_test_dir("invalid");
        let registry = HepaFleetRegistry::new(&root);
        registry.create_task(&task("task-1")).expect("create");
        registry
            .cancel_task("task-1", "2026-06-16T00:01:00Z")
            .expect("cancel");

        // Cancelled is terminal: cannot block or reprioritize.
        assert!(
            registry
                .block_task("task-1", "2026-06-16T00:02:00Z")
                .is_err()
        );
        assert!(
            registry
                .prioritize_task("task-1", 5, "2026-06-16T00:02:00Z")
                .is_err()
        );
        assert!(
            registry
                .block_task("absent", "2026-06-16T00:02:00Z")
                .is_err()
        );

        remove_test_dir(root);
    }

    #[test]
    fn stores_readiness_and_reports_unmet_dependencies() {
        let root = unique_test_dir("deps");
        let registry = HepaFleetRegistry::new(&root);

        let mut completed_dep = task("dep-1");
        completed_dep.status = HepaTaskStatus::Completed;
        completed_dep.completed_at = Some("2026-06-16T00:00:00Z".to_string());
        registry.create_task(&completed_dep).expect("create dep");

        let mut dependent = task("task-1");
        dependent.dependencies = vec!["dep-1".to_string(), "dep-2".to_string()];
        registry.create_task(&dependent).expect("create dependent");

        // dep-1 is completed; dep-2 does not exist yet, so it is unmet.
        let unmet = registry.unmet_dependencies("task-1").expect("unmet");
        assert_eq!(unmet, vec!["dep-2".to_string()]);

        let updated = registry
            .set_task_readiness("task-1", HepaReadinessState::Ready, "2026-06-16T00:05:00Z")
            .expect("set readiness");
        assert_eq!(updated.readiness, HepaReadinessState::Ready);
        // Readiness and dependencies persist across reload.
        let reloaded = registry
            .show_task("task-1")
            .expect("show")
            .expect("present");
        assert_eq!(reloaded.readiness, HepaReadinessState::Ready);
        assert_eq!(reloaded.dependencies.len(), 2);

        remove_test_dir(root);
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-fleet-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
