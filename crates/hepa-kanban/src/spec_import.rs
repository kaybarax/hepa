use hepa_core::contracts::{
    CONTRACT_SCHEMA_VERSION, HepaFleetTask, HepaHermesManagerIntakeArtifact, HepaProject,
    HepaReadinessState, HepaRiskLevel, HepaTaskSpec, HepaTaskStatus, HepaValidate,
};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

use crate::card_mapping::{
    HepaHermesCardMappingInput, HepaHermesCardPayload, map_task_to_hermes_card,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaImportedSpec {
    pub project_id: String,
    pub tasks: Vec<HepaImportedTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaImportedTask {
    pub task_spec: HepaTaskSpec,
    pub fleet_task: HepaFleetTask,
    pub blocked_questions: Vec<String>,
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
        if line.eq_ignore_ascii_case("Questions:") {
            section = TaskSection::Questions;
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
                    .push(normalize_validation_command(strip_list_marker(line)));
            }
            TaskSection::Dependencies => {
                task.dependencies.push(strip_list_marker(line).to_string());
            }
            TaskSection::Questions => {
                task.blocked_questions
                    .push(strip_list_marker(line).to_string());
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

pub fn imported_spec_to_draft_cards(
    project: HepaProject,
    imported: &HepaImportedSpec,
) -> Result<Vec<HepaHermesCardPayload>, HepaSpecImportError> {
    if project.project_id != imported.project_id {
        return Err(HepaSpecImportError::new(
            "project_id",
            "project and imported spec must agree",
        ));
    }
    imported
        .tasks
        .iter()
        .map(|task| {
            let input = HepaHermesCardMappingInput {
                project: project.clone(),
                task_spec: task.task_spec.clone(),
                task: task.fleet_task.clone(),
                lanes: Vec::new(),
                readiness: None,
                validation: None,
                review_signals: Vec::new(),
                terminal_report: None,
                timing: None,
                steering_records: Vec::new(),
                blocked_questions: task.blocked_questions.clone(),
            };
            map_task_to_hermes_card(&input)
                .map_err(|error| HepaSpecImportError::new("card_mapping", error.to_string()))
        })
        .collect()
}

pub fn import_hermes_manager_intake(
    artifact: HepaHermesManagerIntakeArtifact,
) -> Result<HepaImportedSpec, HepaSpecImportError> {
    artifact
        .validate()
        .map_err(|error| HepaSpecImportError::new(error.field, error.message))?;
    let project_id = artifact.project.project_id;
    let tasks = artifact
        .tasks
        .into_iter()
        .map(|task| {
            let created_at = task.task_spec.created_at.clone();
            let blocked_questions = task.blocked_questions;
            let readiness = if blocked_questions.is_empty() {
                HepaReadinessState::NotReady
            } else {
                HepaReadinessState::Blocked
            };
            let fleet_task = HepaFleetTask {
                schema_version: CONTRACT_SCHEMA_VERSION,
                task_id: task.task_spec.task_id.clone(),
                project_id: task.task_spec.project_id.clone(),
                title: task.title,
                description: task.task_spec.goal.clone(),
                status: HepaTaskStatus::Draft,
                readiness,
                dependencies: task.task_spec.dependencies.clone(),
                lane_ids: Vec::new(),
                external_card_id: None,
                priority: task.priority,
                created_at: created_at.clone(),
                updated_at: created_at,
                completed_at: None,
            };
            Ok(HepaImportedTask {
                task_spec: task.task_spec,
                fleet_task,
                blocked_questions,
            })
        })
        .collect::<Result<Vec<_>, HepaSpecImportError>>()?;
    Ok(HepaImportedSpec { project_id, tasks })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskSection {
    Description,
    Acceptance,
    Validation,
    Dependencies,
    Questions,
}

#[derive(Debug)]
struct TaskBuilder {
    task_id: String,
    title: String,
    description: Vec<String>,
    acceptance_criteria: Vec<String>,
    validation_commands: Vec<String>,
    dependencies: Vec<String>,
    blocked_questions: Vec<String>,
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
            blocked_questions: Vec::new(),
        }
    }

    fn finish(self, project_id: &str) -> Result<HepaImportedTask, HepaSpecImportError> {
        if self.acceptance_criteria.is_empty() && self.blocked_questions.is_empty() {
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
            max_total_rounds: 3,
            created_at: created_at.clone(),
        };
        let fleet_task = HepaFleetTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: self.task_id,
            project_id: project_id.to_string(),
            title: self.title,
            description: goal,
            status: HepaTaskStatus::Draft,
            readiness: if self.blocked_questions.is_empty() {
                HepaReadinessState::NotReady
            } else {
                HepaReadinessState::Blocked
            },
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
            blocked_questions: self.blocked_questions,
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

fn normalize_validation_command(command: &str) -> String {
    let command = command.trim();
    command
        .strip_prefix('`')
        .and_then(|value| value.strip_suffix('`'))
        .unwrap_or(command)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_mapping::HepaHermesFieldValue;

    fn project() -> HepaProject {
        HepaProject {
            schema_version: CONTRACT_SCHEMA_VERSION,
            project_id: "project-1".to_string(),
            display_name: "Project One".to_string(),
            repo_ref: "<PROJECT_REPO>".to_string(),
            default_branch: "main".to_string(),
            routing_policy_ref: None,
            is_active: true,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
        }
    }

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
    fn markdown_spec_import_strips_inline_code_ticks_from_validation_commands() {
        let spec = r#"
Project: project-1

## Task: task-1: Validate shell commands
Acceptance:
- Commands are stored in executable form.
Validation:
- `pnpm --filter @todo/api-gateway test -- route-proxy app`
- `git diff --check`
"#;

        let imported = import_markdown_spec(spec).expect("spec should import");

        assert_eq!(
            imported.tasks[0].task_spec.validation_commands,
            vec![
                "pnpm --filter @todo/api-gateway test -- route-proxy app",
                "git diff --check"
            ]
        );
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

    #[test]
    fn imported_specs_create_draft_hermes_cards() {
        let imported = import_markdown_spec(
            r#"
Project: project-1
## Task: task-1: Write docs
Acceptance:
- Docs describe usage.
"#,
        )
        .expect("spec should import");

        let cards = imported_spec_to_draft_cards(project(), &imported)
            .expect("imported spec should create cards");

        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "Write docs");
        assert_eq!(
            cards[0].fields.get("task_status"),
            Some(&HepaHermesFieldValue::Text("draft".to_string()))
        );
        assert_eq!(
            cards[0].fields.get("readiness_state"),
            Some(&HepaHermesFieldValue::Text("not_ready".to_string()))
        );
    }

    #[test]
    fn hermes_manager_intake_imports_draft_tasks_for_kanban_population() {
        let artifact = HepaHermesManagerIntakeArtifact {
            schema_version: CONTRACT_SCHEMA_VERSION,
            author_profile_id: "hepa-manager".to_string(),
            project: project(),
            tasks: vec![hepa_core::contracts::HepaHermesManagerTaskIntake {
                task_spec: HepaTaskSpec {
                    schema_version: CONTRACT_SCHEMA_VERSION,
                    task_id: "task-1".to_string(),
                    project_id: "project-1".to_string(),
                    goal: "Update README.md with the requested workflow.".to_string(),
                    non_goals: Vec::new(),
                    expected_areas: vec!["README.md".to_string()],
                    acceptance_criteria: vec!["Workflow docs are present.".to_string()],
                    validation_commands: vec!["cargo test".to_string()],
                    dependencies: Vec::new(),
                    target_branch: Some("main".to_string()),
                    risk_level: HepaRiskLevel::Low,
                    max_total_rounds: 3,
                    created_at: "2026-06-16T00:00:00Z".to_string(),
                },
                title: "Write workflow docs".to_string(),
                blocked_questions: Vec::new(),
                priority: 7,
            }],
        };

        let imported =
            import_hermes_manager_intake(artifact).expect("manager intake should import");
        let cards = imported_spec_to_draft_cards(project(), &imported)
            .expect("manager intake should create draft cards");

        assert_eq!(imported.project_id, "project-1");
        assert_eq!(imported.tasks.len(), 1);
        assert_eq!(imported.tasks[0].fleet_task.status, HepaTaskStatus::Draft);
        assert_eq!(imported.tasks[0].fleet_task.priority, 7);
        assert_eq!(imported.tasks[0].task_spec.max_total_rounds, 3);
        assert_eq!(cards[0].title, "Write workflow docs");
    }

    #[test]
    fn hermes_manager_intake_blocks_ambiguous_tasks_with_questions() {
        let mut task_spec = HepaTaskSpec {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            goal: "Clarify the requested workflow.".to_string(),
            non_goals: Vec::new(),
            expected_areas: Vec::new(),
            acceptance_criteria: Vec::new(),
            validation_commands: Vec::new(),
            dependencies: Vec::new(),
            target_branch: None,
            risk_level: HepaRiskLevel::Low,
            max_total_rounds: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
        };
        let artifact = HepaHermesManagerIntakeArtifact {
            schema_version: CONTRACT_SCHEMA_VERSION,
            author_profile_id: "hepa-manager".to_string(),
            project: project(),
            tasks: vec![hepa_core::contracts::HepaHermesManagerTaskIntake {
                task_spec: task_spec.clone(),
                title: "Clarify workflow".to_string(),
                blocked_questions: vec!["Which workflow should HEPA document?".to_string()],
                priority: 1,
            }],
        };

        let imported =
            import_hermes_manager_intake(artifact).expect("questioned intake should import");
        assert_eq!(
            imported.tasks[0].fleet_task.readiness,
            HepaReadinessState::Blocked
        );
        assert_eq!(
            imported.tasks[0].blocked_questions,
            vec!["Which workflow should HEPA document?"]
        );

        task_spec.project_id = "other-project".to_string();
        let error = import_hermes_manager_intake(HepaHermesManagerIntakeArtifact {
            schema_version: CONTRACT_SCHEMA_VERSION,
            author_profile_id: "hepa-manager".to_string(),
            project: project(),
            tasks: vec![hepa_core::contracts::HepaHermesManagerTaskIntake {
                task_spec,
                title: "Clarify workflow".to_string(),
                blocked_questions: vec!["Which workflow should HEPA document?".to_string()],
                priority: 1,
            }],
        })
        .expect_err("project mismatch should fail");
        assert_eq!(error.field, "tasks[0].task_spec.project_id");
    }

    #[test]
    fn imported_tasks_stay_draft_not_ready_until_readiness_passes() {
        let imported = import_markdown_spec(
            r#"
Project: project-1
## Task: task-1: Launch feature
Ready: true
Acceptance:
- Feature is documented.
"#,
        )
        .expect("spec should import");

        let task = &imported.tasks[0].fleet_task;
        assert_eq!(task.status, HepaTaskStatus::Draft);
        assert_eq!(task.readiness, HepaReadinessState::NotReady);
        assert!(task.lane_ids.is_empty());
    }

    #[test]
    fn sample_spec_imports_into_multiple_dependent_cards() {
        let imported = import_markdown_spec(
            r#"
Project: project-1
## Task: task-1: Write docs
Acceptance:
- Docs describe usage.

## Task: task-2: Review docs
Acceptance:
- Review is complete.
Dependencies:
- task-1
"#,
        )
        .expect("spec should import");
        let cards = imported_spec_to_draft_cards(project(), &imported)
            .expect("imported spec should create cards");

        assert_eq!(imported.tasks.len(), 2);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].title, "Write docs");
        assert_eq!(cards[1].title, "Review docs");
        assert_eq!(imported.tasks[1].task_spec.dependencies, vec!["task-1"]);
    }

    #[test]
    fn dependency_ordering_is_preserved() {
        let imported = import_markdown_spec(
            r#"
Project: project-1
## Task: task-3: Combine work
Acceptance:
- Combined work is ready.
Dependencies:
- task-1
- task-2
- task-0
"#,
        )
        .expect("spec should import");

        assert_eq!(
            imported.tasks[0].task_spec.dependencies,
            vec!["task-1", "task-2", "task-0"]
        );
        assert_eq!(
            imported.tasks[0].fleet_task.dependencies,
            vec!["task-1", "task-2", "task-0"]
        );
    }

    #[test]
    fn ambiguous_tasks_block_with_questions_instead_of_launching() {
        let imported = import_markdown_spec(
            r#"
Project: project-1
## Task: task-1: Clarify feature
Questions:
- Which user flow should this cover?
"#,
        )
        .expect("ambiguous task with questions should import");
        let cards = imported_spec_to_draft_cards(project(), &imported)
            .expect("ambiguous imported task should create card");
        let task = &imported.tasks[0];

        assert_eq!(task.fleet_task.status, HepaTaskStatus::Draft);
        assert_eq!(task.fleet_task.readiness, HepaReadinessState::Blocked);
        assert!(task.fleet_task.lane_ids.is_empty());
        assert_eq!(
            task.blocked_questions,
            vec!["Which user flow should this cover?"]
        );
        assert!(
            cards[0]
                .comments
                .iter()
                .any(|comment| comment.body.contains("Which user flow"))
        );
    }
}
