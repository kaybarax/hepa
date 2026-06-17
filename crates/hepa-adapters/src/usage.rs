use crate::spec::HepaAdapterCostClass;
use hepa_core::cost_accounting::{HepaAdapterUsageEntry, HepaUsageCostClass, HepaUsageSource};
use serde::Deserialize;
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaAdapterUsageExtraction {
    pub entry: Option<HepaAdapterUsageEntry>,
}

pub fn extract_adapter_usage(
    raw_output: &str,
    adapter_id: impl Into<String>,
    invocation_id: impl Into<String>,
    cost_class: &HepaAdapterCostClass,
) -> Result<HepaAdapterUsageExtraction, HepaAdapterUsageError> {
    let adapter_id = adapter_id.into();
    let invocation_id = invocation_id.into();
    let Some(payload) = first_usage_payload(raw_output)? else {
        return Ok(HepaAdapterUsageExtraction { entry: None });
    };
    let entry = HepaAdapterUsageEntry {
        adapter_id,
        invocation_id,
        cost_class: map_cost_class(cost_class),
        input_tokens: payload.input_tokens.or(payload.prompt_tokens),
        output_tokens: payload.output_tokens.or(payload.completion_tokens),
        total_tokens: payload.total_tokens,
        cost_micros: payload.cost_micros,
        currency: payload.currency,
        source: HepaUsageSource::AdapterReported,
    };
    entry
        .validate()
        .map_err(|error| HepaAdapterUsageError::new(error.field, error.message))?;
    Ok(HepaAdapterUsageExtraction { entry: Some(entry) })
}

#[derive(Debug, Deserialize)]
struct UsageEnvelope {
    usage: Option<UsagePayload>,
    input_tokens: Option<u64>,
    prompt_tokens: Option<u64>,
    output_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    cost_micros: Option<u64>,
    currency: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsagePayload {
    input_tokens: Option<u64>,
    prompt_tokens: Option<u64>,
    output_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    cost_micros: Option<u64>,
    currency: Option<String>,
}

impl UsageEnvelope {
    fn into_payload(self) -> Option<UsagePayload> {
        self.usage.or_else(|| {
            let has_top_level = self.input_tokens.is_some()
                || self.prompt_tokens.is_some()
                || self.output_tokens.is_some()
                || self.completion_tokens.is_some()
                || self.total_tokens.is_some()
                || self.cost_micros.is_some()
                || self.currency.is_some();
            has_top_level.then_some(UsagePayload {
                input_tokens: self.input_tokens,
                prompt_tokens: self.prompt_tokens,
                output_tokens: self.output_tokens,
                completion_tokens: self.completion_tokens,
                total_tokens: self.total_tokens,
                cost_micros: self.cost_micros,
                currency: self.currency,
            })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaAdapterUsageError {
    pub field: String,
    pub message: String,
}

impl HepaAdapterUsageError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaAdapterUsageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaAdapterUsageError {}

fn first_usage_payload(raw_output: &str) -> Result<Option<UsagePayload>, HepaAdapterUsageError> {
    for (line_index, line) in raw_output.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let envelope = serde_json::from_str::<UsageEnvelope>(line).map_err(|error| {
            HepaAdapterUsageError::new(
                "usage",
                format!("line {} usage JSON parse failed: {error}", line_index + 1),
            )
        })?;
        if let Some(payload) = envelope.into_payload() {
            return Ok(Some(payload));
        }
    }
    Ok(None)
}

fn map_cost_class(cost_class: &HepaAdapterCostClass) -> HepaUsageCostClass {
    match cost_class {
        HepaAdapterCostClass::PaidCloud => HepaUsageCostClass::PaidCloud,
        HepaAdapterCostClass::FreeTier => HepaUsageCostClass::FreeTier,
        HepaAdapterCostClass::Local => HepaUsageCostClass::Local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_nested_usage_payload() {
        let extraction = extract_adapter_usage(
            r#"{"status":"completed","usage":{"input_tokens":120,"output_tokens":30,"total_tokens":150,"cost_micros":4200,"currency":"USD"}}"#,
            "pi",
            "attempt-1",
            &HepaAdapterCostClass::PaidCloud,
        )
        .expect("usage should parse");
        let entry = extraction.entry.expect("entry should exist");

        assert_eq!(entry.input_tokens, Some(120));
        assert_eq!(entry.output_tokens, Some(30));
        assert_eq!(entry.total_tokens, Some(150));
        assert_eq!(entry.cost_micros, Some(4200));
        assert_eq!(entry.currency.as_deref(), Some("USD"));
        assert_eq!(entry.cost_class, HepaUsageCostClass::PaidCloud);
    }

    #[test]
    fn extracts_top_level_usage_payload_and_maps_local_cost_class() {
        let extraction = extract_adapter_usage(
            r#"{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}"#,
            "local-worker",
            "review-1",
            &HepaAdapterCostClass::Local,
        )
        .expect("usage should parse");
        let entry = extraction.entry.expect("entry should exist");

        assert_eq!(entry.input_tokens, Some(10));
        assert_eq!(entry.output_tokens, Some(5));
        assert_eq!(entry.total_tokens, Some(15));
        assert_eq!(entry.cost_class, HepaUsageCostClass::Local);
        assert_eq!(entry.cost_micros, None);
    }

    #[test]
    fn missing_usage_is_non_blocking() {
        let extraction = extract_adapter_usage(
            r#"{"status":"completed"}"#,
            "custom",
            "attempt-1",
            &HepaAdapterCostClass::PaidCloud,
        )
        .expect("missing usage should not fail");

        assert!(extraction.entry.is_none());
    }

    #[test]
    fn malformed_usage_fails_loudly() {
        let error = extract_adapter_usage(
            "not-json",
            "custom",
            "attempt-1",
            &HepaAdapterCostClass::PaidCloud,
        )
        .expect_err("malformed output should fail");

        assert_eq!(error.field, "usage");
    }
}
