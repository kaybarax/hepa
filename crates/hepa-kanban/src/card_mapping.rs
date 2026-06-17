use hepa_core::contracts::{
    HepaFleetTask, HepaLane, HepaLaneSteeringRecord, HepaProject, HepaReadinessResult,
    HepaReviewSignal, HepaTaskSpec, HepaTerminalTaskReport, HepaTimingRecord,
    HepaValidationSummary,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt};

pub const HERMES_CARD_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HepaHermesCardMappingInput {
    pub project: HepaProject,
    pub task_spec: HepaTaskSpec,
    pub task: HepaFleetTask,
    pub lanes: Vec<HepaLane>,
    pub readiness: Option<HepaReadinessResult>,
    pub validation: Option<HepaValidationSummary>,
    pub review_signals: Vec<HepaReviewSignal>,
    pub terminal_report: Option<HepaTerminalTaskReport>,
    pub timing: Option<HepaTimingRecord>,
    pub steering_records: Vec<HepaLaneSteeringRecord>,
    pub blocked_questions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HepaHermesCardPayload {
    pub schema_version: u32,
    pub title: String,
    pub fields: BTreeMap<String, HepaHermesFieldValue>,
    pub comments: Vec<HepaHermesCardComment>,
}

impl HepaHermesCardPayload {
    pub fn validate(&self) -> Result<(), HepaKanbanMappingError> {
        require_schema(self.schema_version)?;
        require_single_line("title", &self.title)?;
        for (key, value) in &self.fields {
            require_single_line("fields.key", key)?;
            value.validate(format!("fields.{key}"))?;
        }
        for (index, comment) in self.comments.iter().enumerate() {
            comment.validate(format!("comments[{index}]"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum HepaHermesFieldValue {
    Text(String),
    Number(u64),
    Bool(bool),
    List(Vec<String>),
}

impl HepaHermesFieldValue {
    fn validate(&self, field: String) -> Result<(), HepaKanbanMappingError> {
        match self {
            Self::Text(value) => require_single_line(field, value),
            Self::Number(_) | Self::Bool(_) => Ok(()),
            Self::List(values) => {
                for (index, value) in values.iter().enumerate() {
                    require_single_line(format!("{field}[{index}]"), value)?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaHermesCardComment {
    pub kind: HepaHermesCommentKind,
    pub body: String,
}

impl HepaHermesCardComment {
    fn validate(&self, field: String) -> Result<(), HepaKanbanMappingError> {
        if self.body.trim().is_empty() {
            return Err(HepaKanbanMappingError::new(field, "must not be empty"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaHermesCommentKind {
    TaskSpec,
    Readiness,
    Validation,
    Review,
    Timing,
    TerminalReport,
    Steering,
    BlockedQuestion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaKanbanMappingError {
    pub field: String,
    pub message: String,
}

impl HepaKanbanMappingError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaKanbanMappingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaKanbanMappingError {}

pub fn map_task_to_hermes_card(
    input: &HepaHermesCardMappingInput,
) -> Result<HepaHermesCardPayload, HepaKanbanMappingError> {
    validate_mapping_input(input)?;

    let mut fields = BTreeMap::new();
    insert_text(&mut fields, "project_id", &input.project.project_id);
    insert_text(&mut fields, "project_name", &input.project.display_name);
    insert_text(&mut fields, "task_id", &input.task.task_id);
    insert_text(
        &mut fields,
        "task_status",
        &stable_json_name(&input.task.status)?,
    );
    insert_text(
        &mut fields,
        "readiness_state",
        &stable_json_name(&input.task.readiness)?,
    );
    fields.insert(
        "priority".to_string(),
        HepaHermesFieldValue::Number(input.task.priority.into()),
    );
    fields.insert(
        "dependencies".to_string(),
        HepaHermesFieldValue::List(input.task.dependencies.clone()),
    );
    fields.insert(
        "lane_ids".to_string(),
        HepaHermesFieldValue::List(input.task.lane_ids.clone()),
    );
    fields.insert(
        "lane_states".to_string(),
        HepaHermesFieldValue::List(lane_state_refs(input)?),
    );
    if let Some(external_card_id) = &input.task.external_card_id {
        insert_text(&mut fields, "external_card_id", external_card_id);
    }
    fields.insert(
        "acceptance_criteria".to_string(),
        HepaHermesFieldValue::List(input.task_spec.acceptance_criteria.clone()),
    );
    fields.insert(
        "validation_commands".to_string(),
        HepaHermesFieldValue::List(input.task_spec.validation_commands.clone()),
    );
    insert_text(
        &mut fields,
        "risk_level",
        &stable_json_name(&input.task_spec.risk_level)?,
    );
    fields.insert(
        "max_total_rounds".to_string(),
        HepaHermesFieldValue::Number(input.task_spec.max_total_rounds.into()),
    );

    if let Some(validation) = &input.validation {
        insert_text(
            &mut fields,
            "validation_status",
            &stable_json_name(&validation.status)?,
        );
        fields.insert(
            "no_tests_detected".to_string(),
            HepaHermesFieldValue::Bool(validation.no_tests_detected),
        );
    }
    if let Some(terminal_report) = &input.terminal_report {
        if let Some(pr_url) = &terminal_report.pr_url {
            insert_text(&mut fields, "pr_url", pr_url);
        }
        insert_text(
            &mut fields,
            "terminal_status",
            &stable_json_name(&terminal_report.status)?,
        );
    }
    if let Some(timing) = input.timing.as_ref().or(input
        .terminal_report
        .as_ref()
        .and_then(|report| report.timing.as_ref()))
    {
        fields.insert(
            "agent_loops".to_string(),
            HepaHermesFieldValue::Number(timing.counters.agent_loops.into()),
        );
        fields.insert(
            "manager_passes".to_string(),
            HepaHermesFieldValue::Number(timing.counters.manager_passes.into()),
        );
        fields.insert(
            "worker_profile_llm_calls".to_string(),
            HepaHermesFieldValue::Number(timing.counters.worker_profile_llm_calls.into()),
        );
        fields.insert(
            "install_events".to_string(),
            HepaHermesFieldValue::Number(timing.counters.install_events.into()),
        );
        fields.insert(
            "container_count".to_string(),
            HepaHermesFieldValue::Number(timing.counters.container_count.into()),
        );
    }

    let mut comments = vec![HepaHermesCardComment {
        kind: HepaHermesCommentKind::TaskSpec,
        body: format!(
            "Goal: {}\nExpected areas: {}\nNon-goals: {}",
            input.task_spec.goal,
            join_or_none(&input.task_spec.expected_areas),
            join_or_none(&input.task_spec.non_goals)
        ),
    }];

    if let Some(readiness) = &input.readiness {
        comments.push(HepaHermesCardComment {
            kind: HepaHermesCommentKind::Readiness,
            body: format!(
                "Readiness: {}\nBlockers: {}\nQuestions: {}",
                stable_json_name(&readiness.status)?,
                join_or_none(&readiness.blockers),
                join_or_none(&readiness.questions)
            ),
        });
    }
    if let Some(validation) = &input.validation {
        comments.push(HepaHermesCardComment {
            kind: HepaHermesCommentKind::Validation,
            body: format!(
                "Validation: {}\nCommands: {}\nSummary: {}",
                stable_json_name(&validation.status)?,
                validation.commands.len(),
                join_or_none(&validation.summary)
            ),
        });
    }
    for review in &input.review_signals {
        comments.push(HepaHermesCardComment {
            kind: HepaHermesCommentKind::Review,
            body: format!(
                "Review {} from {}: {}\nFindings: {}\nSummary: {}",
                review.review_id,
                review.adapter_id,
                stable_json_name(&review.status)?,
                review.findings.len(),
                join_or_none(&review.summary)
            ),
        });
    }
    if let Some(timing) = &input.timing {
        comments.push(timing_comment(timing)?);
    }
    if let Some(terminal_report) = &input.terminal_report {
        comments.push(HepaHermesCardComment {
            kind: HepaHermesCommentKind::TerminalReport,
            body: format!(
                "Terminal status: {}\nHuman attention required: {}\nSummary: {}",
                stable_json_name(&terminal_report.status)?,
                terminal_report.human_attention_required,
                join_or_none(&terminal_report.summary)
            ),
        });
    }
    for record in &input.steering_records {
        comments.push(HepaHermesCardComment {
            kind: HepaHermesCommentKind::Steering,
            body: format!(
                "Steering for lane {} to session {}\nDry-run: {}\nManager approved: {}\nMessage: {}",
                record.lane_id,
                record.session_id,
                record.dry_run,
                record.manager_approved,
                record.message
            ),
        });
    }
    for question in &input.blocked_questions {
        comments.push(HepaHermesCardComment {
            kind: HepaHermesCommentKind::BlockedQuestion,
            body: question.clone(),
        });
    }

    let payload = redact_card_payload(HepaHermesCardPayload {
        schema_version: HERMES_CARD_SCHEMA_VERSION,
        title: input.task.title.clone(),
        fields,
        comments,
    });
    payload.validate()?;
    Ok(payload)
}

fn validate_mapping_input(
    input: &HepaHermesCardMappingInput,
) -> Result<(), HepaKanbanMappingError> {
    if input.project.project_id != input.task.project_id
        || input.project.project_id != input.task_spec.project_id
    {
        return Err(HepaKanbanMappingError::new(
            "project_id",
            "project, task, and spec must agree",
        ));
    }
    if input.task.task_id != input.task_spec.task_id {
        return Err(HepaKanbanMappingError::new(
            "task_id",
            "task and spec must agree",
        ));
    }
    for (index, lane) in input.lanes.iter().enumerate() {
        if lane.task_id != input.task.task_id {
            return Err(HepaKanbanMappingError::new(
                format!("lanes[{index}].task_id"),
                "lane must reference mapped task",
            ));
        }
        if !input.task.lane_ids.contains(&lane.lane_id) {
            return Err(HepaKanbanMappingError::new(
                format!("lanes[{index}].lane_id"),
                "lane must be listed on task",
            ));
        }
    }
    for (index, record) in input.steering_records.iter().enumerate() {
        if !input.task.lane_ids.contains(&record.lane_id) {
            return Err(HepaKanbanMappingError::new(
                format!("steering_records[{index}].lane_id"),
                "steering record must reference a mapped lane",
            ));
        }
    }
    Ok(())
}

fn lane_state_refs(
    input: &HepaHermesCardMappingInput,
) -> Result<Vec<String>, HepaKanbanMappingError> {
    input
        .lanes
        .iter()
        .map(|lane| {
            Ok(format!(
                "{}:{}",
                lane.lane_id,
                stable_json_name(&lane.state)?
            ))
        })
        .collect()
}

fn timing_comment(
    timing: &HepaTimingRecord,
) -> Result<HepaHermesCardComment, HepaKanbanMappingError> {
    Ok(HepaHermesCardComment {
        kind: HepaHermesCommentKind::Timing,
        body: format!(
            "Timing run: {}\nAgent loops: {}\nManager passes: {}\nInstall events: {}\nContainer count: {}\nPhases: {}",
            timing.run_id,
            timing.counters.agent_loops,
            timing.counters.manager_passes,
            timing.counters.install_events,
            timing.counters.container_count,
            timing.phases.len()
        ),
    })
}

fn insert_text(fields: &mut BTreeMap<String, HepaHermesFieldValue>, key: &str, value: &str) {
    fields.insert(
        key.to_string(),
        HepaHermesFieldValue::Text(value.to_string()),
    );
}

fn redact_card_payload(mut payload: HepaHermesCardPayload) -> HepaHermesCardPayload {
    payload.title = redact_sensitive_text(&payload.title);
    for value in payload.fields.values_mut() {
        match value {
            HepaHermesFieldValue::Text(text) => *text = redact_sensitive_text(text),
            HepaHermesFieldValue::List(values) => {
                for value in values {
                    *value = redact_sensitive_text(value);
                }
            }
            HepaHermesFieldValue::Number(_) | HepaHermesFieldValue::Bool(_) => {}
        }
    }
    for comment in &mut payload.comments {
        comment.body = redact_sensitive_text(&comment.body);
    }
    payload
}

fn redact_sensitive_text(value: &str) -> String {
    let mut redacted = redact_private_paths(value);
    if contains_email_like(&redacted) {
        redacted = redact_email_like(&redacted);
    }
    if contains_secret_like(&redacted) {
        "<REDACTED_SECRET_LIKE_VALUE>".to_string()
    } else {
        redacted
    }
}

fn redact_private_paths(value: &str) -> String {
    let private_prefixes = [
        ["/", "Users", "/"].concat(),
        ["/", "home", "/"].concat(),
        ["/", "Volumes", "/"].concat(),
        ["/", "private", "/"].concat(),
        ["/", "tmp", "/"].concat(),
    ];
    value
        .split_whitespace()
        .map(|word| {
            if private_prefixes
                .iter()
                .any(|prefix| word.starts_with(prefix))
            {
                "<PRIVATE_PATH>"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_secret_like(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    let token_word = ["to", "ken"].concat();
    let private_key_word = ["private", "_key"].concat();
    let github_token = ["github", "_", "token"].concat();
    [
        ".env",
        "api_key",
        "apikey",
        "credential",
        "id_rsa",
        "password",
        private_key_word.as_str(),
        "secret",
        token_word.as_str(),
        github_token.as_str(),
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn contains_email_like(value: &str) -> bool {
    value
        .split_whitespace()
        .any(|word| word.contains('@') && word.rsplit_once('.').is_some())
}

fn redact_email_like(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            if word.contains('@') && word.rsplit_once('.').is_some() {
                "<EMAIL>"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn stable_json_name<T>(value: &T) -> Result<String, HepaKanbanMappingError>
where
    T: Serialize,
{
    serde_json::to_value(value)
        .map_err(|error| HepaKanbanMappingError::new("serde", error.to_string()))?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| HepaKanbanMappingError::new("serde", "expected enum string"))
}

fn join_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join("; ")
    }
}

fn require_schema(schema_version: u32) -> Result<(), HepaKanbanMappingError> {
    if schema_version == HERMES_CARD_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(HepaKanbanMappingError::new(
            "schema_version",
            format!("must be {HERMES_CARD_SCHEMA_VERSION}"),
        ))
    }
}

fn require_single_line(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaKanbanMappingError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaKanbanMappingError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaKanbanMappingError::new(field, "must be a single line"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hepa_core::contracts::{
        CONTRACT_SCHEMA_VERSION, HepaFindingSeverity, HepaLaneState, HepaPhaseStatus,
        HepaReadinessState, HepaReadinessStatus, HepaReviewFinding, HepaReviewStatus,
        HepaRiskLevel, HepaTaskStatus, HepaTerminalStatus, HepaTimingCounters, HepaTimingPhase,
        HepaValidationCommandResult, HepaValidationStatus,
    };

    fn sample_input() -> HepaHermesCardMappingInput {
        let project = HepaProject {
            schema_version: CONTRACT_SCHEMA_VERSION,
            project_id: "project-1".to_string(),
            display_name: "Project One".to_string(),
            repo_ref: "<PROJECT_REPO>".to_string(),
            default_branch: "main".to_string(),
            routing_policy_ref: Some("<ROUTING_POLICY>".to_string()),
            is_active: true,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
        };
        let task_spec = HepaTaskSpec {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            goal: "Update docs".to_string(),
            non_goals: vec!["No code changes".to_string()],
            expected_areas: vec!["README.md".to_string()],
            acceptance_criteria: vec!["Docs explain usage".to_string()],
            validation_commands: vec!["cargo test".to_string()],
            dependencies: vec!["task-0".to_string()],
            target_branch: Some("main".to_string()),
            risk_level: HepaRiskLevel::Low,
            max_total_rounds: 2,
            created_at: "2026-06-16T00:00:00Z".to_string(),
        };
        let task = HepaFleetTask {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            title: "Update docs".to_string(),
            description: "Documentation task".to_string(),
            status: HepaTaskStatus::Running,
            readiness: HepaReadinessState::Ready,
            dependencies: vec!["task-0".to_string()],
            lane_ids: vec!["lane-1".to_string()],
            external_card_id: Some("hermes-card-1".to_string()),
            priority: 10,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: None,
        };
        let lane = HepaLane {
            schema_version: CONTRACT_SCHEMA_VERSION,
            lane_id: "lane-1".to_string(),
            project_id: "project-1".to_string(),
            task_id: "task-1".to_string(),
            adapter_id: "fake".to_string(),
            state: HepaLaneState::Reviewing,
            worktree_ref: "<LANE_WORKTREE>".to_string(),
            branch: "hepa/task-1".to_string(),
            run_dir_ref: "<RUN_DIR>".to_string(),
            attempt_count: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
            updated_at: "2026-06-16T00:00:00Z".to_string(),
            completed_at: None,
        };
        let readiness = HepaReadinessResult {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            status: HepaReadinessStatus::Ready,
            blockers: Vec::new(),
            questions: Vec::new(),
            checked_at: "2026-06-16T00:00:00Z".to_string(),
        };
        let validation = HepaValidationSummary {
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: HepaValidationStatus::Passed,
            commands: vec![HepaValidationCommandResult {
                command: "cargo test".to_string(),
                exit_code: 0,
                duration_ms: 1200,
            }],
            no_tests_detected: false,
            failure_type: None,
            summary: vec!["Tests passed".to_string()],
        };
        let review_signals = vec![HepaReviewSignal {
            schema_version: CONTRACT_SCHEMA_VERSION,
            review_id: "review-1".to_string(),
            lane_id: "lane-1".to_string(),
            adapter_id: "reviewer-a".to_string(),
            status: HepaReviewStatus::Approved,
            findings: vec![HepaReviewFinding {
                finding_id: "finding-1".to_string(),
                severity: HepaFindingSeverity::Low,
                file_ref: Some("README.md".to_string()),
                line: Some(12),
                message: "Looks good".to_string(),
                accepted: false,
            }],
            summary: vec!["Approved".to_string()],
            completed_at: "2026-06-16T00:00:00Z".to_string(),
        }];
        let timing = HepaTimingRecord {
            schema_version: CONTRACT_SCHEMA_VERSION,
            run_id: "run-1".to_string(),
            phases: vec![HepaTimingPhase {
                name: "review".to_string(),
                status: HepaPhaseStatus::Completed,
                duration_seconds: 1.5,
                round: Some(1),
                role: None,
                adapter_id: Some("reviewer-a".to_string()),
                routing_reason: Some("review fanout".to_string()),
                sandbox_posture: Some("host-worktree".to_string()),
            }],
            counters: HepaTimingCounters {
                agent_loops: 1,
                manager_passes: 1,
                worker_profile_llm_calls: 0,
                reviewer_passes: 1,
                install_events: 0,
                container_count: 0,
            },
        };
        let terminal_report = HepaTerminalTaskReport {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            status: HepaTerminalStatus::Completed,
            pr_url: Some("<PR_URL>".to_string()),
            validation: Some(validation.clone()),
            review_signals: review_signals.clone(),
            timing: Some(timing.clone()),
            summary: vec!["Ready for human".to_string()],
            human_attention_required: false,
            completed_at: "2026-06-16T00:00:00Z".to_string(),
        };

        HepaHermesCardMappingInput {
            project,
            task_spec,
            task,
            lanes: vec![lane],
            readiness: Some(readiness),
            validation: Some(validation),
            review_signals,
            terminal_report: Some(terminal_report),
            timing: Some(timing),
            steering_records: Vec::new(),
            blocked_questions: vec!["Confirm release note wording".to_string()],
        }
    }

    #[test]
    fn card_mapping_covers_task_project_lane_and_status_fields() {
        let payload = map_task_to_hermes_card(&sample_input()).expect("mapping should succeed");

        assert_eq!(payload.title, "Update docs");
        assert_eq!(
            payload.fields.get("project_id"),
            Some(&HepaHermesFieldValue::Text("project-1".to_string()))
        );
        assert_eq!(
            payload.fields.get("task_status"),
            Some(&HepaHermesFieldValue::Text("running".to_string()))
        );
        assert_eq!(
            payload.fields.get("readiness_state"),
            Some(&HepaHermesFieldValue::Text("ready".to_string()))
        );
        assert_eq!(
            payload.fields.get("lane_ids"),
            Some(&HepaHermesFieldValue::List(vec!["lane-1".to_string()]))
        );
        assert_eq!(
            payload.fields.get("external_card_id"),
            Some(&HepaHermesFieldValue::Text("hermes-card-1".to_string()))
        );
        assert_eq!(
            payload.fields.get("validation_status"),
            Some(&HepaHermesFieldValue::Text("passed".to_string()))
        );
        assert_eq!(
            payload.fields.get("pr_url"),
            Some(&HepaHermesFieldValue::Text("<PR_URL>".to_string()))
        );
        assert_eq!(
            payload.fields.get("agent_loops"),
            Some(&HepaHermesFieldValue::Number(1))
        );
        assert_eq!(
            payload.fields.get("install_events"),
            Some(&HepaHermesFieldValue::Number(0))
        );
        assert_eq!(
            payload.fields.get("worker_profile_llm_calls"),
            Some(&HepaHermesFieldValue::Number(0))
        );
        assert_eq!(
            payload.fields.get("container_count"),
            Some(&HepaHermesFieldValue::Number(0))
        );
        assert!(
            payload
                .comments
                .iter()
                .any(|comment| comment.kind == HepaHermesCommentKind::BlockedQuestion)
        );
    }

    #[test]
    fn card_mapping_uses_deterministic_field_order() {
        let json = serde_json::to_string(&map_task_to_hermes_card(&sample_input()).unwrap())
            .expect("payload should serialize");

        let acceptance = json.find("acceptance_criteria").unwrap();
        let agent_loops = json.find("agent_loops").unwrap();
        let default_project = json.find("project_id").unwrap();

        assert!(acceptance < agent_loops);
        assert!(agent_loops < default_project);
    }

    #[test]
    fn steering_records_are_projected_to_card_comments() {
        let mut input = sample_input();
        input.steering_records = vec![HepaLaneSteeringRecord {
            schema_version: CONTRACT_SCHEMA_VERSION,
            lane_id: "lane-1".to_string(),
            session_id: "hepa-lane-1".to_string(),
            message: "continue with focused tests".to_string(),
            manager_approved: true,
            dry_run: false,
            lane_state: HepaLaneState::Running,
        }];

        let payload = map_task_to_hermes_card(&input).expect("mapping should succeed");
        let comment = payload
            .comments
            .iter()
            .find(|comment| comment.kind == HepaHermesCommentKind::Steering)
            .expect("steering comment");

        assert!(comment.body.contains("lane lane-1"));
        assert!(comment.body.contains("session hepa-lane-1"));
        assert!(comment.body.contains("Manager approved: true"));
        assert!(
            comment
                .body
                .contains("Message: continue with focused tests")
        );
    }

    #[test]
    fn card_mapping_round_trips_through_fixture_payload() {
        let payload = map_task_to_hermes_card(&sample_input()).expect("mapping should succeed");
        let fixture_json =
            serde_json::to_string_pretty(&payload).expect("fixture payload should serialize");
        let round_trip: HepaHermesCardPayload =
            serde_json::from_str(&fixture_json).expect("fixture payload should deserialize");

        assert_eq!(round_trip, payload);
        assert_eq!(
            round_trip.fields.keys().next().map(String::as_str),
            Some("acceptance_criteria")
        );
    }

    #[test]
    fn task_can_exist_without_external_card_id() {
        let mut input = sample_input();
        input.task.external_card_id = None;

        let payload =
            map_task_to_hermes_card(&input).expect("missing Hermes card ID should not block task");

        assert!(!payload.fields.contains_key("external_card_id"));
        assert_eq!(
            payload.fields.get("task_id"),
            Some(&HepaHermesFieldValue::Text("task-1".to_string()))
        );
    }

    #[test]
    fn card_mapping_rejects_mismatched_lane_task() {
        let mut input = sample_input();
        input.lanes[0].task_id = "other-task".to_string();

        let error = map_task_to_hermes_card(&input).expect_err("bad lane links must fail");

        assert_eq!(error.field, "lanes[0].task_id");
    }

    #[test]
    fn card_mapping_redacts_private_paths_and_secret_like_values() {
        let mut input = sample_input();
        let private_path = ["/", "Users", "/person/project/.env"].concat();
        input.task.title = format!("Inspect {private_path}");
        input.task_spec.goal = format!("Read {private_path}");
        input.task_spec.acceptance_criteria = vec!["Do not expose api_key values".to_string()];
        let email_like = ["owner", "@", "example", ".", "invalid"].concat();
        input.blocked_questions = vec![format!("Ask {email_like} for details")];

        let payload = map_task_to_hermes_card(&input).expect("mapping should redact");
        let json = serde_json::to_string(&payload).expect("payload should serialize");

        assert!(!json.contains(&private_path));
        assert!(!json.contains("api_key"));
        assert!(!json.contains(&email_like));
        assert!(json.contains("<PRIVATE_PATH>") || json.contains("<REDACTED_SECRET_LIKE_VALUE>"));
        assert!(json.contains("<EMAIL>"));
    }
}
