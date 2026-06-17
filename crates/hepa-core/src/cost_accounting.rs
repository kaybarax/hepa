use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt};

pub const COST_ACCOUNTING_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaLaneCostReport {
    pub schema_version: u32,
    pub run_id: String,
    pub task_id: String,
    pub lane_id: String,
    pub entries: Vec<HepaAdapterUsageEntry>,
    pub totals: HepaCostTotals,
    pub generated_at: String,
}

impl HepaLaneCostReport {
    pub fn from_entries(
        run_id: impl Into<String>,
        task_id: impl Into<String>,
        lane_id: impl Into<String>,
        entries: Vec<HepaAdapterUsageEntry>,
        generated_at: impl Into<String>,
    ) -> Result<Self, HepaCostAccountingError> {
        let report = Self {
            schema_version: COST_ACCOUNTING_SCHEMA_VERSION,
            run_id: run_id.into(),
            task_id: task_id.into(),
            lane_id: lane_id.into(),
            totals: HepaCostTotals::from_entries(&entries),
            entries,
            generated_at: generated_at.into(),
        };
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<(), HepaCostAccountingError> {
        require_schema(self.schema_version)?;
        require_single_line("run_id", &self.run_id)?;
        require_single_line("task_id", &self.task_id)?;
        require_single_line("lane_id", &self.lane_id)?;
        require_single_line("generated_at", &self.generated_at)?;
        for entry in &self.entries {
            entry.validate()?;
        }
        let expected = HepaCostTotals::from_entries(&self.entries);
        if self.totals != expected {
            return Err(HepaCostAccountingError::new(
                "totals",
                "must equal the sum of cost entries",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaAdapterUsageEntry {
    pub adapter_id: String,
    pub invocation_id: String,
    pub cost_class: HepaUsageCostClass,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_micros: Option<u64>,
    pub currency: Option<String>,
    pub source: HepaUsageSource,
}

impl HepaAdapterUsageEntry {
    pub fn validate(&self) -> Result<(), HepaCostAccountingError> {
        require_single_line("adapter_id", &self.adapter_id)?;
        require_single_line("invocation_id", &self.invocation_id)?;
        if let Some(total) = self.total_tokens {
            let parts = self.input_tokens.unwrap_or(0) + self.output_tokens.unwrap_or(0);
            if parts > 0 && total < parts {
                return Err(HepaCostAccountingError::new(
                    "total_tokens",
                    "must not be less than input_tokens + output_tokens",
                ));
            }
        }
        if let Some(currency) = &self.currency {
            require_single_line("currency", currency)?;
            if currency.len() != 3 || !currency.chars().all(|ch| ch.is_ascii_uppercase()) {
                return Err(HepaCostAccountingError::new(
                    "currency",
                    "must be an ISO-style uppercase 3-letter code",
                ));
            }
        }
        if self.cost_micros.is_some() && self.currency.is_none() {
            return Err(HepaCostAccountingError::new(
                "currency",
                "is required when cost_micros is recorded",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaCostTotals {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
    pub total_cost_micros: u64,
    pub currency_totals: BTreeMap<String, u64>,
    pub entries_without_cost: u32,
}

impl HepaCostTotals {
    pub fn from_entries(entries: &[HepaAdapterUsageEntry]) -> Self {
        let mut currency_totals = BTreeMap::new();
        let mut totals = Self {
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_tokens: 0,
            total_cost_micros: 0,
            currency_totals: BTreeMap::new(),
            entries_without_cost: 0,
        };
        for entry in entries {
            totals.total_input_tokens += entry.input_tokens.unwrap_or(0);
            totals.total_output_tokens += entry.output_tokens.unwrap_or(0);
            totals.total_tokens += entry.total_tokens.unwrap_or_else(|| {
                entry.input_tokens.unwrap_or(0) + entry.output_tokens.unwrap_or(0)
            });
            if let Some(cost_micros) = entry.cost_micros {
                totals.total_cost_micros += cost_micros;
                if let Some(currency) = &entry.currency {
                    *currency_totals.entry(currency.clone()).or_insert(0) += cost_micros;
                }
            } else {
                totals.entries_without_cost += 1;
            }
        }
        totals.currency_totals = currency_totals;
        totals
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HepaUsageCostClass {
    Local,
    FreeTier,
    PaidCloud,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HepaUsageSource {
    AdapterReported,
    Estimated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaCostAccountingError {
    pub field: String,
    pub message: String,
}

impl HepaCostAccountingError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaCostAccountingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaCostAccountingError {}

fn require_schema(schema_version: u32) -> Result<(), HepaCostAccountingError> {
    if schema_version == COST_ACCOUNTING_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(HepaCostAccountingError::new(
            "schema_version",
            format!("must be {COST_ACCOUNTING_SCHEMA_VERSION}"),
        ))
    }
}

fn require_single_line(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaCostAccountingError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaCostAccountingError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaCostAccountingError::new(field, "must be a single line"));
    }
    reject_sensitive_ref(field, value)
}

fn reject_sensitive_ref(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaCostAccountingError> {
    let field = field.into();
    let lowered = value.to_ascii_lowercase();
    if lowered.contains("/users/")
        || lowered.contains("/home/")
        || lowered.contains(".env")
        || lowered.contains("api_key")
        || lowered.contains("apikey")
        || lowered.contains("credential")
        || lowered.contains("password")
        || lowered.contains("private_key")
        || lowered.contains("secret")
        || lowered.contains("token")
    {
        return Err(HepaCostAccountingError::new(
            field,
            "must not contain sensitive references",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_cost_report_sums_usage_entries() {
        let report = HepaLaneCostReport::from_entries(
            "run-1",
            "task-1",
            "lane-1",
            vec![
                HepaAdapterUsageEntry {
                    adapter_id: "pi".to_string(),
                    invocation_id: "attempt-1".to_string(),
                    cost_class: HepaUsageCostClass::PaidCloud,
                    input_tokens: Some(100),
                    output_tokens: Some(40),
                    total_tokens: Some(140),
                    cost_micros: Some(2500),
                    currency: Some("USD".to_string()),
                    source: HepaUsageSource::AdapterReported,
                },
                HepaAdapterUsageEntry {
                    adapter_id: "local-worker".to_string(),
                    invocation_id: "review-1".to_string(),
                    cost_class: HepaUsageCostClass::Local,
                    input_tokens: Some(20),
                    output_tokens: Some(5),
                    total_tokens: None,
                    cost_micros: None,
                    currency: None,
                    source: HepaUsageSource::AdapterReported,
                },
            ],
            "2026-06-18T00:00:00Z",
        )
        .expect("cost report should validate");

        assert_eq!(report.totals.total_input_tokens, 120);
        assert_eq!(report.totals.total_output_tokens, 45);
        assert_eq!(report.totals.total_tokens, 165);
        assert_eq!(report.totals.total_cost_micros, 2500);
        assert_eq!(report.totals.currency_totals.get("USD"), Some(&2500));
        assert_eq!(report.totals.entries_without_cost, 1);
    }

    #[test]
    fn lane_cost_report_rejects_tampered_totals() {
        let mut report = HepaLaneCostReport::from_entries(
            "run-1",
            "task-1",
            "lane-1",
            vec![HepaAdapterUsageEntry {
                adapter_id: "pi".to_string(),
                invocation_id: "attempt-1".to_string(),
                cost_class: HepaUsageCostClass::PaidCloud,
                input_tokens: Some(10),
                output_tokens: Some(5),
                total_tokens: Some(15),
                cost_micros: Some(100),
                currency: Some("USD".to_string()),
                source: HepaUsageSource::AdapterReported,
            }],
            "2026-06-18T00:00:00Z",
        )
        .expect("cost report should validate");
        report.totals.total_tokens = 999;

        let error = report.validate().expect_err("tampered totals must fail");

        assert_eq!(error.field, "totals");
    }

    #[test]
    fn usage_entries_require_currency_for_costs_and_redact_sensitive_refs() {
        let entry = HepaAdapterUsageEntry {
            adapter_id: "adapter-secret".to_string(),
            invocation_id: "attempt-1".to_string(),
            cost_class: HepaUsageCostClass::PaidCloud,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cost_micros: Some(100),
            currency: None,
            source: HepaUsageSource::AdapterReported,
        };

        let error = entry
            .validate()
            .expect_err("sensitive adapter IDs must fail first");
        assert_eq!(error.field, "adapter_id");

        let mut entry = entry;
        entry.adapter_id = "pi".to_string();
        let error = entry.validate().expect_err("costs require a currency");
        assert_eq!(error.field, "currency");
    }
}
