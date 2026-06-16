use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt};

pub const ROUTING_POLICY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaRoutingPolicy {
    pub schema_version: u32,
    pub project_id: String,
    pub default: String,
    pub capability_routes: BTreeMap<String, String>,
    pub review_fanout: HepaReviewFanout,
}

impl HepaRoutingPolicy {
    pub fn validate(&self) -> Result<(), HepaRoutingError> {
        require_schema(self.schema_version)?;
        require_single_line("project_id", &self.project_id)?;
        require_single_line("default", &self.default)?;
        require_capability_routes(&self.capability_routes)?;
        self.review_fanout.validate()?;
        Ok(())
    }

    pub fn resolve_adapter(&self, capability: Option<&str>) -> Result<&str, HepaRoutingError> {
        self.validate()?;
        let Some(capability) = capability.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(self.default.as_str());
        };
        require_single_line("capability", capability)?;
        self.capability_routes
            .get(capability)
            .map(String::as_str)
            .ok_or_else(|| HepaRoutingError::new("capability", "unknown capability key"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaReviewFanout {
    pub adapters: Vec<String>,
    pub pass_policy: HepaReviewPassPolicy,
}

impl HepaReviewFanout {
    pub fn validate(&self) -> Result<(), HepaRoutingError> {
        if self.adapters.is_empty() {
            return Err(HepaRoutingError::new(
                "review_fanout.adapters",
                "must include at least one reviewer adapter",
            ));
        }
        require_string_list("review_fanout.adapters", &self.adapters)?;
        match self.pass_policy {
            HepaReviewPassPolicy::All | HepaReviewPassPolicy::Any => Ok(()),
            HepaReviewPassPolicy::AtLeast { required } => {
                if required == 0 {
                    return Err(HepaRoutingError::new(
                        "review_fanout.pass_policy.required",
                        "must be greater than zero",
                    ));
                }
                if required as usize > self.adapters.len() {
                    return Err(HepaRoutingError::new(
                        "review_fanout.pass_policy.required",
                        "must not exceed reviewer adapter count",
                    ));
                }
                Ok(())
            }
        }
    }

    pub fn passes(&self, approvals: u32) -> Result<bool, HepaRoutingError> {
        self.validate()?;
        Ok(match self.pass_policy {
            HepaReviewPassPolicy::All => approvals as usize == self.adapters.len(),
            HepaReviewPassPolicy::Any => approvals > 0,
            HepaReviewPassPolicy::AtLeast { required } => approvals >= required,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum HepaReviewPassPolicy {
    All,
    Any,
    AtLeast { required: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaRoutingError {
    pub field: String,
    pub message: String,
}

impl HepaRoutingError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaRoutingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaRoutingError {}

fn require_schema(schema_version: u32) -> Result<(), HepaRoutingError> {
    if schema_version == ROUTING_POLICY_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(HepaRoutingError::new(
            "schema_version",
            format!("must be {ROUTING_POLICY_SCHEMA_VERSION}"),
        ))
    }
}

fn require_capability_routes(routes: &BTreeMap<String, String>) -> Result<(), HepaRoutingError> {
    for (capability, adapter_id) in routes {
        require_single_line("capability_routes.key", capability)?;
        require_single_line(format!("capability_routes.{capability}"), adapter_id)?;
    }
    Ok(())
}

fn require_string_list(field: &str, values: &[String]) -> Result<(), HepaRoutingError> {
    for (index, value) in values.iter().enumerate() {
        require_single_line(format!("{field}[{index}]"), value)?;
    }
    Ok(())
}

fn require_single_line(field: impl Into<String>, value: &str) -> Result<(), HepaRoutingError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaRoutingError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaRoutingError::new(field, "must be a single line"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_policy() -> HepaRoutingPolicy {
        HepaRoutingPolicy {
            schema_version: ROUTING_POLICY_SCHEMA_VERSION,
            project_id: "project-1".to_string(),
            default: "worker-primary".to_string(),
            capability_routes: BTreeMap::from([
                ("design".to_string(), "worker-design".to_string()),
                ("docs".to_string(), "worker-docs".to_string()),
                ("frontend".to_string(), "worker-frontend".to_string()),
                ("local-only".to_string(), "worker-local".to_string()),
            ]),
            review_fanout: HepaReviewFanout {
                adapters: vec![
                    "reviewer-a".to_string(),
                    "reviewer-b".to_string(),
                    "reviewer-c".to_string(),
                ],
                pass_policy: HepaReviewPassPolicy::AtLeast { required: 2 },
            },
        }
    }

    #[test]
    fn routing_policy_contains_default_capabilities_and_review_fanout() {
        let policy = sample_policy();

        policy.validate().expect("sample policy should validate");

        assert_eq!(policy.resolve_adapter(None).unwrap(), "worker-primary");
        assert_eq!(
            policy.resolve_adapter(Some("frontend")).unwrap(),
            "worker-frontend"
        );
        assert_eq!(
            policy.resolve_adapter(Some("local-only")).unwrap(),
            "worker-local"
        );
        assert!(policy.review_fanout.passes(2).unwrap());
        assert!(!policy.review_fanout.passes(1).unwrap());
    }

    #[test]
    fn routing_policy_serializes_capabilities_deterministically() {
        let json = serde_json::to_string(&sample_policy()).expect("routing should serialize");

        let design = json.find("\"design\"").expect("design route is present");
        let docs = json.find("\"docs\"").expect("docs route is present");
        let frontend = json
            .find("\"frontend\"")
            .expect("frontend route is present");

        assert!(design < docs);
        assert!(docs < frontend);
        assert!(json.contains("\"type\":\"at_least\""));
        assert!(json.contains("\"required\":2"));
    }
}
