use crate::contracts::{HepaProject, HepaValidate};
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

    /// List registered projects in stable `project_id` order.
    pub fn list_projects(&self) -> Result<Vec<HepaRegisteredProject>, HepaFleetError> {
        let mut projects = self.read_records(&self.projects_dir())?;
        projects.sort_by(|left: &HepaRegisteredProject, right| {
            left.project.project_id.cmp(&right.project.project_id)
        });
        Ok(projects)
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
    use crate::contracts::CONTRACT_SCHEMA_VERSION;
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
