use hepa_core::contracts::{
    CONTRACT_SCHEMA_VERSION, HepaFindingSeverity, HepaReviewFinding, HepaReviewSignal,
    HepaReviewStatus, HepaValidate, to_stable_json,
};
use serde::Deserialize;
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaReviewerOutputInput {
    pub review_id: String,
    pub lane_id: String,
    pub adapter_id: String,
    pub completed_at: String,
    pub raw_output: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaReviewerOutputError {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HepaReviewerNormalizationRoute {
    DeterministicParser,
    ReviewerProfileFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaReviewerNormalizationResult {
    pub signal: HepaReviewSignal,
    pub route: HepaReviewerNormalizationRoute,
    pub fallback_reason: Option<String>,
    pub normalized_output_json: String,
}

impl HepaReviewerOutputError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaReviewerOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaReviewerOutputError {}

#[derive(Debug, Deserialize)]
struct RawReviewOutput {
    status: String,
    #[serde(default)]
    summary: Vec<String>,
    #[serde(default)]
    findings: Vec<RawFinding>,
}

#[derive(Debug, Deserialize)]
struct RawFinding {
    finding_id: Option<String>,
    severity: String,
    category: String,
    evidence: String,
    in_scope: bool,
    release_risk: bool,
    recommended_action: String,
    file_ref: Option<String>,
    line: Option<u32>,
    message: String,
    #[serde(default)]
    accepted: bool,
}

pub fn normalize_reviewer_output(
    input: HepaReviewerOutputInput,
) -> Result<HepaReviewSignal, HepaReviewerOutputError> {
    require_single_line("review_id", &input.review_id)?;
    require_single_line("lane_id", &input.lane_id)?;
    require_single_line("adapter_id", &input.adapter_id)?;
    require_single_line("completed_at", &input.completed_at)?;
    let raw: RawReviewOutput = serde_json::from_str(&input.raw_output)
        .map_err(|error| HepaReviewerOutputError::new("raw_output", error.to_string()))?;
    let findings = raw
        .findings
        .into_iter()
        .enumerate()
        .map(|(index, finding)| normalize_finding(index, finding))
        .collect::<Result<Vec<_>, _>>()?;
    let signal = HepaReviewSignal {
        schema_version: CONTRACT_SCHEMA_VERSION,
        review_id: input.review_id,
        lane_id: input.lane_id,
        adapter_id: input.adapter_id,
        status: parse_status(&raw.status)?,
        findings,
        summary: raw.summary,
        completed_at: input.completed_at,
    };
    signal
        .validate()
        .map_err(|error| HepaReviewerOutputError::new(error.field, error.message))?;
    Ok(signal)
}

pub fn normalize_reviewer_output_by_exception(
    input: HepaReviewerOutputInput,
    fallback: impl FnOnce(
        HepaReviewerOutputInput,
        HepaReviewerOutputError,
    ) -> Result<HepaReviewSignal, HepaReviewerOutputError>,
) -> Result<HepaReviewerNormalizationResult, HepaReviewerOutputError> {
    match normalize_reviewer_output(input.clone()) {
        Ok(signal) => normalization_result(
            signal,
            HepaReviewerNormalizationRoute::DeterministicParser,
            None,
        ),
        Err(error) => {
            let fallback_reason = Some(format!("{}: {}", error.field, error.message));
            fallback(input, error).and_then(|signal| {
                signal
                    .validate()
                    .map_err(|error| HepaReviewerOutputError::new(error.field, error.message))?;
                normalization_result(
                    signal,
                    HepaReviewerNormalizationRoute::ReviewerProfileFallback,
                    fallback_reason,
                )
            })
        }
    }
}

fn normalization_result(
    signal: HepaReviewSignal,
    route: HepaReviewerNormalizationRoute,
    fallback_reason: Option<String>,
) -> Result<HepaReviewerNormalizationResult, HepaReviewerOutputError> {
    let normalized_output_json = to_stable_json(&signal)
        .map_err(|error| HepaReviewerOutputError::new("normalized_output", error.to_string()))?;
    Ok(HepaReviewerNormalizationResult {
        signal,
        route,
        fallback_reason,
        normalized_output_json,
    })
}

fn normalize_finding(
    index: usize,
    finding: RawFinding,
) -> Result<HepaReviewFinding, HepaReviewerOutputError> {
    Ok(HepaReviewFinding {
        finding_id: finding
            .finding_id
            .unwrap_or_else(|| format!("finding-{}", index + 1)),
        severity: parse_severity(&finding.severity)?,
        category: finding.category,
        evidence: finding.evidence,
        in_scope: finding.in_scope,
        release_risk: finding.release_risk,
        recommended_action: finding.recommended_action,
        file_ref: finding.file_ref,
        line: finding.line,
        message: finding.message,
        accepted: finding.accepted,
    })
}

fn parse_status(value: &str) -> Result<HepaReviewStatus, HepaReviewerOutputError> {
    match value {
        "approved" => Ok(HepaReviewStatus::Approved),
        "changes_requested" => Ok(HepaReviewStatus::ChangesRequested),
        "blocked" => Ok(HepaReviewStatus::Blocked),
        "failed" => Ok(HepaReviewStatus::Failed),
        _ => Err(HepaReviewerOutputError::new(
            "status",
            format!("unsupported review status: {value}"),
        )),
    }
}

fn parse_severity(value: &str) -> Result<HepaFindingSeverity, HepaReviewerOutputError> {
    match value {
        "low" => Ok(HepaFindingSeverity::Low),
        "medium" => Ok(HepaFindingSeverity::Medium),
        "high" => Ok(HepaFindingSeverity::High),
        "critical" => Ok(HepaFindingSeverity::Critical),
        _ => Err(HepaReviewerOutputError::new(
            "severity",
            format!("unsupported finding severity: {value}"),
        )),
    }
}

fn require_single_line(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaReviewerOutputError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaReviewerOutputError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaReviewerOutputError::new(field, "must be a single line"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn normalizes_clean_approved_reviewer_output() {
        let signal = normalize_reviewer_output(input(
            "reviewer-a",
            r#"{
              "status": "approved",
              "summary": ["No blocking findings."],
              "findings": []
            }"#,
        ))
        .expect("approved output normalizes");

        assert_eq!(signal.status, HepaReviewStatus::Approved);
        assert!(signal.findings.is_empty());
        assert_eq!(signal.summary, vec!["No blocking findings."]);
    }

    #[test]
    fn normalizes_findings_with_phase_five_fields() {
        let signal = normalize_reviewer_output(input(
            "reviewer-b",
            r#"{
              "status": "changes_requested",
              "summary": ["One release-blocking issue."],
              "findings": [{
                "severity": "high",
                "category": "correctness",
                "evidence": "The validation summary reports failing command test:api.",
                "in_scope": true,
                "release_risk": true,
                "recommended_action": "Fix the API error and rerun validation.",
                "file_ref": "src/api.rs",
                "line": 42,
                "message": "API handler returns the wrong status.",
                "accepted": true
              }]
            }"#,
        ))
        .expect("findings output normalizes");

        assert_eq!(signal.status, HepaReviewStatus::ChangesRequested);
        assert_eq!(signal.findings.len(), 1);
        let finding = &signal.findings[0];
        assert_eq!(finding.finding_id, "finding-1");
        assert_eq!(finding.severity, HepaFindingSeverity::High);
        assert_eq!(finding.category, "correctness");
        assert!(finding.in_scope);
        assert!(finding.release_risk);
        assert_eq!(
            finding.recommended_action,
            "Fix the API error and rerun validation."
        );
    }

    #[test]
    fn parser_corpus_covers_supported_adapter_output_shapes() {
        let cases = [
            (
                "reviewer-a",
                HepaReviewStatus::Approved,
                r#"{
                  "status": "approved",
                  "summary": ["No findings."],
                  "findings": []
                }"#,
                0,
            ),
            (
                "reviewer-b",
                HepaReviewStatus::ChangesRequested,
                r#"{
                  "status": "changes_requested",
                  "summary": ["Needs repair."],
                  "findings": [{
                    "finding_id": "reviewer-b-finding",
                    "severity": "medium",
                    "category": "tests",
                    "evidence": "Validation shows missing coverage.",
                    "in_scope": true,
                    "release_risk": false,
                    "recommended_action": "Add the missing test.",
                    "file_ref": "tests/review.rs",
                    "line": 12,
                    "message": "Coverage is incomplete.",
                    "accepted": true
                  }]
                }"#,
                1,
            ),
            (
                "reviewer-c",
                HepaReviewStatus::Blocked,
                r#"{
                  "status": "blocked",
                  "summary": ["Reviewer could not assess generated diff."],
                  "findings": [{
                    "severity": "high",
                    "category": "reviewability",
                    "evidence": "The diff context was incomplete.",
                    "in_scope": true,
                    "release_risk": true,
                    "recommended_action": "Re-run review with complete context.",
                    "file_ref": null,
                    "line": null,
                    "message": "Review context is insufficient.",
                    "accepted": false
                  }]
                }"#,
                1,
            ),
            (
                "reviewer-d",
                HepaReviewStatus::Failed,
                r#"{
                  "status": "failed",
                  "summary": ["Adapter returned a non-review failure."],
                  "findings": []
                }"#,
                0,
            ),
        ];

        for (adapter_id, expected_status, raw_output, expected_findings) in cases {
            let signal =
                normalize_reviewer_output(input(adapter_id, raw_output)).expect("corpus parses");

            assert_eq!(signal.status, expected_status);
            assert_eq!(signal.findings.len(), expected_findings);
        }
    }

    #[test]
    fn clean_parser_path_does_not_engage_reviewer_profile_fallback() {
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&fallback_calls);

        let result = normalize_reviewer_output_by_exception(
            input(
                "reviewer-a",
                r#"{
                  "status": "approved",
                  "summary": ["No findings."],
                  "findings": []
                }"#,
            ),
            move |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                panic!("clean parser output must not call reviewer-profile fallback")
            },
        )
        .expect("clean output normalizes deterministically");

        assert_eq!(result.signal.status, HepaReviewStatus::Approved);
        assert_eq!(
            result.route,
            HepaReviewerNormalizationRoute::DeterministicParser
        );
        assert_eq!(result.fallback_reason, None);
        assert!(
            result
                .normalized_output_json
                .contains("\"status\": \"approved\"")
        );
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn malformed_parser_output_engages_reviewer_profile_fallback_once() {
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&fallback_calls);

        let result = normalize_reviewer_output_by_exception(
            input("reviewer-a", r#"{"status": "not-a-status"}"#),
            move |input, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(HepaReviewSignal {
                    schema_version: CONTRACT_SCHEMA_VERSION,
                    review_id: input.review_id,
                    lane_id: input.lane_id,
                    adapter_id: input.adapter_id,
                    status: HepaReviewStatus::Failed,
                    findings: Vec::new(),
                    summary: vec!["reviewer-profile fallback normalized output".to_string()],
                    completed_at: input.completed_at,
                })
            },
        )
        .expect("fallback normalizes malformed output");

        assert_eq!(
            result.route,
            HepaReviewerNormalizationRoute::ReviewerProfileFallback
        );
        assert_eq!(result.signal.status, HepaReviewStatus::Failed);
        assert_eq!(
            result.fallback_reason,
            Some("status: unsupported review status: not-a-status".to_string())
        );
        assert!(
            result
                .normalized_output_json
                .contains("reviewer-profile fallback normalized output")
        );
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    fn input(adapter_id: &str, raw_output: &str) -> HepaReviewerOutputInput {
        HepaReviewerOutputInput {
            review_id: format!("review-{adapter_id}"),
            lane_id: "lane-1".to_string(),
            adapter_id: adapter_id.to_string(),
            completed_at: "2026-06-16T00:00:00Z".to_string(),
            raw_output: raw_output.to_string(),
        }
    }
}
