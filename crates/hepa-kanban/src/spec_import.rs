use hepa_core::contracts::{
    CONTRACT_SCHEMA_VERSION, HepaFleetTask, HepaReadinessState, HepaRiskLevel, HepaTaskSpec,
    HepaTaskStatus,
};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaImportedSpec {
    pub project_id: String,
    pub tasks: Vec<HepaImportedTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaImportedTask {
    pub task_spec: HepaTaskSpec,
    pub fleet_task: HepaFleetTask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaSpecImportError {
    pub field: String,
    pub message: String,
}

impl HepaSpecImportError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaSpecImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaSpecImportError {}

pub fn import_markdown_spec(markdown: &str) -> Result<HepaImportedSpec, HepaSpecImportError> {
    let mut project_id = "default-project".to_string();
    let mut tasks = Vec::new();
    let mut current: Option<TaskBuilder> = None;
    let mut section = TaskSection::Description;

    for raw_line in markdown.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("Project:") {
            project_id = require_value("project_id", value)?;
            continue;
        }
        if let Some(value) = line.strip_prefix("## Task") {
            if let Some(task) = current.take() {
                tasks.push(task.finish(&project_id)?);
            }
            current = Some(TaskBuilder::new(parse_task_heading(value)?));
            section = TaskSection::Description;
            continue;
        }
        if line.eq_ignore_ascii_case("Acceptance:") {
            section = TaskSection::Acceptance;
            continue;
        }
        if line.eq_ignore_ascii_case("Validation:") {
            section = TaskSection::Validation;
            continue;
        }
        if line.eq_ignore_ascii_case("Dependencies:") {
            section = TaskSection::Dependencies;
            continue;
        }

        let Some(task) = current.as_mut() else {
            continue;
        };
        match section {
            TaskSection::Description => {
                task.description.push(strip_list_marker(line).to_string());
            }
            TaskSection::Acceptance => {
                task.acceptance_criteria
                    .push(strip_list_marker(line).to_string());
            }
            TaskSection::Validation => {
                task.validation_commands
                    .push(strip_list_marker(line).to_string());
            }
            TaskSection::Dependencies => {
                task.dependencies.push(strip_list_marker(line).to_string());
            }
        }
    }
    if let Some(task) = current.take() {
        tasks.push(task.finish(&project_id)?);
    }
    if tasks.is_empty() {
        return Err(HepaSpecImportError::new(
            "tasks",
            "spec must contain at least one task heading",
        ));
    }
    Ok(HepaImportedSpec { project_id, tasks })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskSection {
    Description,
    Acceptance,
    Validation,
    Dependencies,
}

#[derive(Debug)]
struct TaskBuilder {
    task_id: String,
    title: String,
    description: Vec<String>,
    acceptance_criteria: Vec<String>,
    validation_commands: Vec<String>,
    dependencies: Vec<String>,
}

impl TaskBuilder {
    fn new((task_id, title): (String, String)) -> Self {
        Self {
            task_id,
            title,
            description: Vec::new(),
            acceptance_criteria: Vec::new(),
            validation_commands: Vec::new(),
            dependencies: Vec::new(),
        }
    }

    fn finish(self, project_id: &str) -> Result<HepaImportedTask, HepaSpecImportError> {
        if self.acceptance_criteria.is_empty() {
            return Err(HepaSpecImportError::new(
                format!("{}.acceptance_criteria", self.task_id),
                "task must include acceptance criteria",
            ));
        }
        let goal = if self.description.is_empty() {
            self.title.clone()
        } else {
            self.description.join(" ")
        };
        let created_at = "2026-06-16T00:00:00Z".to_string();
        let task_spec = HepaTaskSpec {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: self.task_id.clone(),
            project_id: project_id.to_string(),
            goal: goal.clone(),
            non_goals: Vec::new(),
            expected_areas: Vec::new(),
            acceptance_criteria: self.acceptance_criteria,
            validation_commands: self.validation_commands,
            dependencies: self.dependencies.clone(),
            target_branch: None,
            risk_level: HepaRiskLevel::Low,
            max_total_rounds: 1,
            created_at: created_at.clone(),
        };
        let fleet_task = HepaFleetTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: self.task_id,
            project_id: project_id.to_string(),
            title: self.title,
            description: goal,
            status: HepaTaskStatus::Draft,
            readiness: HepaReadinessState::NotReady,
            dependencies: self.dependencies,
            lane_ids: Vec::new(),
            external_card_id: None,
            priority: 0,
            created_at: created_at.clone(),
            updated_at: created_at,
            completed_at: None,
        };
        Ok(HepaImportedTask {
            task_spec,
            fleet_task,
        })
    }
}

fn parse_task_heading(value: &str) -> Result<(String, String), HepaSpecImportError> {
    let value = value.trim();
    let value = value.strip_prefix(':').unwrap_or(value).trim();
    let Some((task_id, title)) = value.split_once(':') else {
        return Err(HepaSpecImportError::new(
            "task_heading",
            "expected '## Task: task-id: title'",
        ));
    };
    let task_id = require_value("task_id", task_id)?;
    let title = require_value("title", title)?;
    Ok((task_id, title))
}

fn require_value(field: &str, value: &str) -> Result<String, HepaSpecImportError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(HepaSpecImportError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaSpecImportError::new(field, "must be a single line"));
    }
    Ok(value.to_string())
}

fn strip_list_marker(line: &str) -> &str {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .unwrap_or(line)
        .trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_spec_imports_tasks_with_dependencies_acceptance_and_validation() {
        let spec = r#"
Project: project-1

## Task: task-1: Write docs
Explain the feature.
Acceptance:
- Docs describe usage.
Validation:
- cargo test
Dependencies:
- task-0
"#;

        let imported = import_markdown_spec(spec).expect("spec should import");

        assert_eq!(imported.project_id, "project-1");
        assert_eq!(imported.tasks.len(), 1);
        let task = &imported.tasks[0];
        assert_eq!(task.task_spec.task_id, "task-1");
        assert_eq!(task.task_spec.dependencies, vec!["task-0"]);
        assert_eq!(
            task.task_spec.acceptance_criteria,
            vec!["Docs describe usage."]
        );
        assert_eq!(task.task_spec.validation_commands, vec!["cargo test"]);
        assert_eq!(task.fleet_task.status, HepaTaskStatus::Draft);
        assert_eq!(task.fleet_task.readiness, HepaReadinessState::NotReady);
    }

    #[test]
    fn markdown_spec_import_requires_acceptance_criteria() {
        let error = import_markdown_spec(
            r#"
Project: project-1
## Task: task-1: Write docs
"#,
        )
        .expect_err("tasks without acceptance criteria must fail");

        assert_eq!(error.field, "task-1.acceptance_criteria");
    }
}
