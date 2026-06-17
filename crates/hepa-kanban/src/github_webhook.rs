use hepa_core::{
    contracts::{
        CONTRACT_SCHEMA_VERSION, HepaFleetTask, HepaReadinessState, HepaRiskLevel, HepaTaskSpec,
        HepaTaskStatus, HepaValidate,
    },
    redaction::redact_secrets,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{error::Error, fmt};

use crate::spec_import::{HepaImportedSpec, HepaImportedTask};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaGithubIssueWebhookRequest {
    pub event: String,
    pub delivery_id: String,
    pub signature_256: Option<String>,
    pub project_id: String,
    pub secret: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaGithubIssueWebhookOutcome {
    pub delivery_id: String,
    pub event: String,
    pub action: String,
    pub verification: HepaGithubWebhookVerification,
    pub imported: Option<HepaImportedSpec>,
    pub ignored_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaGithubWebhookVerification {
    VerifiedSha256,
    NotConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaGithubWebhookError {
    pub field: String,
    pub message: String,
}

impl HepaGithubWebhookError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaGithubWebhookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaGithubWebhookError {}

pub fn import_github_issue_webhook(
    request: &HepaGithubIssueWebhookRequest,
    payload: &str,
) -> Result<HepaGithubIssueWebhookOutcome, HepaGithubWebhookError> {
    require_single_line("event", &request.event)?;
    require_single_line("delivery_id", &request.delivery_id)?;
    require_single_line("project_id", &request.project_id)?;
    let verification = verify_signature(payload, request)?;
    let payload: GithubIssuesWebhookPayload = serde_json::from_str(payload).map_err(|error| {
        HepaGithubWebhookError::new("payload", format!("invalid JSON payload: {error}"))
    })?;
    if request.event != "issues" {
        return Ok(ignored(
            request,
            verification,
            payload.action,
            "only GitHub issues events create HEPA draft tasks",
        ));
    }
    if !is_supported_issue_action(&payload.action) {
        return Ok(ignored(
            request,
            verification,
            payload.action,
            "issue action does not create or refresh a HEPA task",
        ));
    }
    if payload.issue.pull_request.is_some() {
        return Ok(ignored(
            request,
            verification,
            payload.action,
            "pull request issue payloads are ignored",
        ));
    }

    let imported = issue_payload_to_imported_spec(&request.project_id, &payload)?;
    Ok(HepaGithubIssueWebhookOutcome {
        delivery_id: request.delivery_id.clone(),
        event: request.event.clone(),
        action: payload.action,
        verification,
        imported: Some(imported),
        ignored_reason: None,
    })
}

fn issue_payload_to_imported_spec(
    project_id: &str,
    payload: &GithubIssuesWebhookPayload,
) -> Result<HepaImportedSpec, HepaGithubWebhookError> {
    let task_id = format!("github-issue-{}", payload.issue.number);
    let created_at = payload
        .issue
        .updated_at
        .as_deref()
        .or(payload.issue.created_at.as_deref())
        .unwrap_or("2026-06-18T00:00:00Z")
        .to_string();
    let title = sanitize_text(&payload.issue.title);
    let body = sanitize_text(payload.issue.body.as_deref().unwrap_or(""));
    let sections = parse_issue_sections(&body);
    let blocked_questions = if sections.acceptance_criteria.is_empty() {
        vec!["GitHub issue must include acceptance criteria before launch.".to_string()]
    } else {
        sections.blocked_questions
    };
    let goal = if body.is_empty() {
        title.clone()
    } else {
        format!("{title}\n\n{body}")
    };
    let risk_level = risk_from_labels(&payload.issue.labels);
    let expected_areas = expected_areas_from_labels(&payload.issue.labels);
    let task_spec = HepaTaskSpec {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id: task_id.clone(),
        project_id: project_id.to_string(),
        goal: goal.clone(),
        non_goals: Vec::new(),
        expected_areas,
        acceptance_criteria: sections.acceptance_criteria,
        validation_commands: sections.validation_commands,
        dependencies: sections.dependencies.clone(),
        target_branch: payload.repository.default_branch.clone(),
        risk_level,
        max_total_rounds: 1,
        created_at: created_at.clone(),
    };
    let fleet_task = HepaFleetTask {
        schema_version: CONTRACT_SCHEMA_VERSION,
        task_id,
        project_id: project_id.to_string(),
        title,
        description: goal,
        status: HepaTaskStatus::Draft,
        readiness: if blocked_questions.is_empty() {
            HepaReadinessState::NotReady
        } else {
            HepaReadinessState::Blocked
        },
        dependencies: sections.dependencies,
        lane_ids: Vec::new(),
        external_card_id: Some(format!("github-issue:{}", payload.issue.number)),
        priority: priority_from_labels(&payload.issue.labels),
        created_at: created_at.clone(),
        updated_at: created_at,
        completed_at: None,
    };
    task_spec
        .validate()
        .map_err(|error| HepaGithubWebhookError::new(error.field, error.message))?;
    fleet_task
        .validate()
        .map_err(|error| HepaGithubWebhookError::new(error.field, error.message))?;
    Ok(HepaImportedSpec {
        project_id: project_id.to_string(),
        tasks: vec![HepaImportedTask {
            task_spec,
            fleet_task,
            blocked_questions,
        }],
    })
}

fn verify_signature(
    payload: &str,
    request: &HepaGithubIssueWebhookRequest,
) -> Result<HepaGithubWebhookVerification, HepaGithubWebhookError> {
    let Some(secret) = request.secret.as_deref() else {
        return Ok(HepaGithubWebhookVerification::NotConfigured);
    };
    require_single_line("secret", secret)?;
    let signature = request.signature_256.as_deref().ok_or_else(|| {
        HepaGithubWebhookError::new(
            "signature_256",
            "X-Hub-Signature-256 is required when a webhook secret is configured",
        )
    })?;
    let signature = signature.strip_prefix("sha256=").ok_or_else(|| {
        HepaGithubWebhookError::new("signature_256", "must use the sha256= prefix")
    })?;
    let signature = decode_hex(signature)?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| HepaGithubWebhookError::new("secret", "failed to initialize HMAC verifier"))?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature).map_err(|_| {
        HepaGithubWebhookError::new("signature_256", "webhook signature verification failed")
    })?;
    Ok(HepaGithubWebhookVerification::VerifiedSha256)
}

fn ignored(
    request: &HepaGithubIssueWebhookRequest,
    verification: HepaGithubWebhookVerification,
    action: String,
    reason: impl Into<String>,
) -> HepaGithubIssueWebhookOutcome {
    HepaGithubIssueWebhookOutcome {
        delivery_id: request.delivery_id.clone(),
        event: request.event.clone(),
        action,
        verification,
        imported: None,
        ignored_reason: Some(reason.into()),
    }
}

#[derive(Debug, Default)]
struct IssueSections {
    acceptance_criteria: Vec<String>,
    validation_commands: Vec<String>,
    dependencies: Vec<String>,
    blocked_questions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IssueSection {
    Other,
    Acceptance,
    Validation,
    Dependencies,
    Questions,
}

fn parse_issue_sections(body: &str) -> IssueSections {
    let mut sections = IssueSections::default();
    let mut current = IssueSection::Other;
    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.eq_ignore_ascii_case("acceptance:") || line.eq_ignore_ascii_case("acceptance") {
            current = IssueSection::Acceptance;
            continue;
        }
        if line.eq_ignore_ascii_case("validation:") || line.eq_ignore_ascii_case("validation") {
            current = IssueSection::Validation;
            continue;
        }
        if line.eq_ignore_ascii_case("dependencies:") || line.eq_ignore_ascii_case("dependencies") {
            current = IssueSection::Dependencies;
            continue;
        }
        if line.eq_ignore_ascii_case("questions:") || line.eq_ignore_ascii_case("questions") {
            current = IssueSection::Questions;
            continue;
        }
        let value = strip_list_marker(line).to_string();
        match current {
            IssueSection::Other => {}
            IssueSection::Acceptance => sections.acceptance_criteria.push(value),
            IssueSection::Validation => sections.validation_commands.push(value),
            IssueSection::Dependencies => sections.dependencies.push(value),
            IssueSection::Questions => sections.blocked_questions.push(value),
        }
    }
    sections
}

fn is_supported_issue_action(action: &str) -> bool {
    matches!(
        action,
        "opened" | "edited" | "reopened" | "labeled" | "unlabeled"
    )
}

fn risk_from_labels(labels: &[GithubLabel]) -> HepaRiskLevel {
    if labels.iter().any(|label| {
        matches!(
            label.name.as_str(),
            "hepa:risk=high" | "hepa:risk:high" | "risk:high"
        )
    }) {
        HepaRiskLevel::High
    } else if labels.iter().any(|label| {
        matches!(
            label.name.as_str(),
            "hepa:risk=medium" | "hepa:risk:medium" | "risk:medium"
        )
    }) {
        HepaRiskLevel::Medium
    } else {
        HepaRiskLevel::Low
    }
}

fn priority_from_labels(labels: &[GithubLabel]) -> u32 {
    labels
        .iter()
        .filter_map(|label| {
            label
                .name
                .strip_prefix("hepa:priority=")
                .and_then(|value| value.parse::<u32>().ok())
        })
        .max()
        .unwrap_or(0)
}

fn expected_areas_from_labels(labels: &[GithubLabel]) -> Vec<String> {
    labels
        .iter()
        .filter_map(|label| label.name.strip_prefix("hepa:area="))
        .map(sanitize_text)
        .filter(|value| !value.is_empty())
        .collect()
}

fn sanitize_text(text: &str) -> String {
    redact_private_refs(&redact_secrets(text))
        .trim()
        .to_string()
}

fn redact_private_refs(text: &str) -> String {
    let mut redacted = text
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|token| {
                    if token.contains(&private_path_marker())
                        || token.contains("/home/")
                        || token.contains(&windows_private_path_marker())
                        || token.contains(&windows_slash_private_path_marker())
                    {
                        "<redacted>"
                    } else {
                        token
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.ends_with('\n') {
        redacted.push('\n');
    }
    redacted
}

fn strip_list_marker(line: &str) -> &str {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .unwrap_or(line)
        .trim()
}

fn private_path_marker() -> String {
    ["/", "Users", "/"].concat()
}

fn windows_private_path_marker() -> String {
    ["C:", "\\", "Users", "\\"].concat()
}

fn windows_slash_private_path_marker() -> String {
    ["C:", "/", "Users", "/"].concat()
}

fn require_single_line(field: &str, value: &str) -> Result<(), HepaGithubWebhookError> {
    if value.trim().is_empty() {
        return Err(HepaGithubWebhookError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaGithubWebhookError::new(field, "must be a single line"));
    }
    Ok(())
}

fn decode_hex(value: &str) -> Result<Vec<u8>, HepaGithubWebhookError> {
    if value.len() % 2 != 0 {
        return Err(HepaGithubWebhookError::new(
            "signature_256",
            "hex digest must have an even number of characters",
        ));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_value(pair[0])?;
            let low = hex_value(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_value(byte: u8) -> Result<u8, HepaGithubWebhookError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(HepaGithubWebhookError::new(
            "signature_256",
            "hex digest contains a non-hex character",
        )),
    }
}

#[derive(Debug, Deserialize)]
struct GithubIssuesWebhookPayload {
    action: String,
    issue: GithubIssue,
    repository: GithubRepository,
}

#[derive(Debug, Deserialize)]
struct GithubIssue {
    number: u64,
    title: String,
    body: Option<String>,
    labels: Vec<GithubLabel>,
    pull_request: Option<serde_json::Value>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct GithubRepository {
    default_branch: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request() -> HepaGithubIssueWebhookRequest {
        HepaGithubIssueWebhookRequest {
            event: "issues".to_string(),
            delivery_id: "delivery-1".to_string(),
            signature_256: None,
            project_id: "project-1".to_string(),
            secret: None,
        }
    }

    fn issue_payload(body: &str) -> String {
        format!(
            r#"{{
  "action": "opened",
  "issue": {{
    "number": 42,
    "title": "Build webhook intake",
    "body": {body:?},
    "labels": [
      {{ "name": "hepa:priority=7" }},
      {{ "name": "hepa:risk=medium" }},
      {{ "name": "hepa:area=crates/hepa-kanban" }}
    ],
    "created_at": "2026-06-18T00:00:00Z",
    "updated_at": "2026-06-18T00:01:00Z"
  }},
  "repository": {{ "default_branch": "main" }},
  "sender": {{ "login": "octocat" }}
}}"#
        )
    }

    #[test]
    fn github_issue_webhook_imports_draft_task_from_opened_issue() {
        let payload = issue_payload(
            "Implement issue intake.\nAcceptance:\n- Task is drafted.\nValidation:\n- cargo test -p hepa-kanban\nDependencies:\n- task-1",
        );

        let outcome = import_github_issue_webhook(&base_request(), &payload)
            .expect("issue webhook should import");
        let imported = outcome.imported.expect("task should import");
        let task = &imported.tasks[0];

        assert_eq!(
            outcome.verification,
            HepaGithubWebhookVerification::NotConfigured
        );
        assert_eq!(imported.project_id, "project-1");
        assert_eq!(task.task_spec.task_id, "github-issue-42");
        assert_eq!(
            task.fleet_task.external_card_id,
            Some("github-issue:42".to_string())
        );
        assert_eq!(task.fleet_task.status, HepaTaskStatus::Draft);
        assert_eq!(task.fleet_task.readiness, HepaReadinessState::NotReady);
        assert_eq!(task.fleet_task.priority, 7);
        assert_eq!(task.task_spec.risk_level, HepaRiskLevel::Medium);
        assert_eq!(task.task_spec.expected_areas, vec!["crates/hepa-kanban"]);
        assert_eq!(task.task_spec.acceptance_criteria, vec!["Task is drafted."]);
        assert_eq!(
            task.task_spec.validation_commands,
            vec!["cargo test -p hepa-kanban"]
        );
        assert_eq!(task.task_spec.dependencies, vec!["task-1"]);
    }

    #[test]
    fn missing_acceptance_blocks_questions_instead_of_marking_ready() {
        let payload = issue_payload("Please figure out the implementation.");

        let outcome = import_github_issue_webhook(&base_request(), &payload)
            .expect("ambiguous issue should import");
        let task = &outcome.imported.expect("task should import").tasks[0];

        assert_eq!(task.fleet_task.readiness, HepaReadinessState::Blocked);
        assert_eq!(
            task.blocked_questions,
            vec!["GitHub issue must include acceptance criteria before launch."]
        );
    }

    #[test]
    fn pull_request_issue_payloads_are_ignored() {
        let payload = r#"{
  "action": "opened",
  "issue": {
    "number": 43,
    "title": "PR mirror",
    "body": "Acceptance:\n- ignored",
    "labels": [],
    "pull_request": { "url": "https://api.github.invalid/repos/example/pulls/1" }
  },
  "repository": { "default_branch": "main" }
}"#;

        let outcome =
            import_github_issue_webhook(&base_request(), payload).expect("payload should parse");

        assert!(outcome.imported.is_none());
        assert_eq!(
            outcome.ignored_reason,
            Some("pull request issue payloads are ignored".to_string())
        );
    }

    #[test]
    fn verifies_sha256_signature_when_secret_is_configured() {
        let payload = issue_payload("Acceptance:\n- Signature verifies.");
        let mut mac = HmacSha256::new_from_slice(b"shared-secret").expect("hmac");
        mac.update(payload.as_bytes());
        let signature = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let request = HepaGithubIssueWebhookRequest {
            signature_256: Some(format!("sha256={signature}")),
            secret: Some("shared-secret".to_string()),
            ..base_request()
        };

        let outcome =
            import_github_issue_webhook(&request, &payload).expect("signature should verify");

        assert_eq!(
            outcome.verification,
            HepaGithubWebhookVerification::VerifiedSha256
        );
    }

    #[test]
    fn signature_mismatch_blocks_import_before_payload_processing() {
        let request = HepaGithubIssueWebhookRequest {
            signature_256: Some("sha256=00000000000000000000000000000000".to_string()),
            secret: Some("shared-secret".to_string()),
            ..base_request()
        };

        let error = import_github_issue_webhook(&request, "{}")
            .expect_err("bad signature should block import");

        assert_eq!(error.field, "signature_256");
    }

    #[test]
    fn issue_body_is_redacted_before_task_creation() {
        let github_token = ["ghp", "_", "supersecret"].concat();
        let private_path = ["/", "Users", "/example/repo"].concat();
        let payload = issue_payload(&format!(
            "Token: {github_token}\nAcceptance:\n- Do not leak {private_path}."
        ));

        let outcome =
            import_github_issue_webhook(&base_request(), &payload).expect("payload should import");
        let task = &outcome.imported.expect("task should import").tasks[0];

        assert!(!task.task_spec.goal.contains(&github_token));
        assert!(!task.task_spec.goal.contains(&private_path));
        assert!(task.task_spec.goal.contains("<redacted>"));
    }
}
