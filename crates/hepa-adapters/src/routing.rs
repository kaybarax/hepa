use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt};

use crate::spec::{HepaAdapterCostClass, HepaAdapterSpec};

pub const ROUTING_POLICY_SCHEMA_VERSION: u32 = 1;
pub const ROUTING_DECISION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaRoutingPolicy {
    pub schema_version: u32,
    pub project_id: String,
    pub project_policy: HepaProjectRoutingPolicy,
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

    pub fn validate_against_adapters(
        &self,
        adapter_specs: &BTreeMap<String, HepaAdapterSpec>,
    ) -> Result<(), HepaRoutingError> {
        self.validate()?;
        require_adapter_allowed("default", &self.default, self.project_policy, adapter_specs)?;
        for (capability, adapter_id) in &self.capability_routes {
            require_adapter_allowed(
                format!("capability_routes.{capability}"),
                adapter_id,
                self.project_policy,
                adapter_specs,
            )?;
        }
        for (index, adapter_id) in self.review_fanout.adapters.iter().enumerate() {
            require_adapter_allowed(
                format!("review_fanout.adapters[{index}]"),
                adapter_id,
                self.project_policy,
                adapter_specs,
            )?;
        }
        Ok(())
    }

    pub fn resolve_adapter_spec<'a>(
        &self,
        capability: Option<&str>,
        adapter_specs: &'a BTreeMap<String, HepaAdapterSpec>,
    ) -> Result<&'a HepaAdapterSpec, HepaRoutingError> {
        self.validate_against_adapters(adapter_specs)?;
        let adapter_id = self.resolve_adapter(capability)?;
        adapter_specs.get(adapter_id).ok_or_else(|| {
            HepaRoutingError::new(
                format!("adapter_specs.{adapter_id}"),
                "missing adapter spec for route target",
            )
        })
    }

    pub fn decide_adapter(
        &self,
        request: HepaRoutingDecisionRequest,
        adapter_specs: &BTreeMap<String, HepaAdapterSpec>,
    ) -> Result<HepaRoutingDecisionArtifact, HepaRoutingError> {
        self.validate_against_adapters(adapter_specs)?;
        request.validate()?;

        let requested_capability = request
            .requested_capability
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let (selected_adapter, route_source, manager_override) =
            if let Some(manager_override) = request.manager_override {
                require_adapter_allowed(
                    "manager_override.adapter_id",
                    &manager_override.adapter_id,
                    self.project_policy,
                    adapter_specs,
                )?;
                (
                    manager_override.adapter_id.clone(),
                    HepaRoutingDecisionSource::ManagerOverride,
                    Some(manager_override),
                )
            } else {
                let selected_adapter = self.resolve_adapter(requested_capability)?.to_string();
                let route_source = if requested_capability.is_some() {
                    HepaRoutingDecisionSource::Capability
                } else {
                    HepaRoutingDecisionSource::Default
                };
                (selected_adapter, route_source, None)
            };

        Ok(HepaRoutingDecisionArtifact {
            schema_version: ROUTING_DECISION_SCHEMA_VERSION,
            task_id: request.task_id,
            lane_id: request.lane_id,
            project_id: self.project_id.clone(),
            project_policy: self.project_policy,
            requested_capability: requested_capability.map(str::to_string),
            selected_adapter,
            route_source,
            manager_override,
            routing_table: self.capability_routes.clone(),
            default_adapter: self.default.clone(),
            review_fanout: self.review_fanout.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HepaProjectRoutingPolicy {
    Standard,
    LocalOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaRoutingDecisionRequest {
    pub task_id: String,
    pub lane_id: String,
    pub requested_capability: Option<String>,
    pub manager_override: Option<HepaManagerRoutingOverride>,
}

impl HepaRoutingDecisionRequest {
    fn validate(&self) -> Result<(), HepaRoutingError> {
        require_single_line("task_id", &self.task_id)?;
        require_single_line("lane_id", &self.lane_id)?;
        if let Some(capability) = &self.requested_capability {
            require_single_line("requested_capability", capability)?;
        }
        if let Some(manager_override) = &self.manager_override {
            manager_override.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaRoutingDecisionArtifact {
    pub schema_version: u32,
    pub task_id: String,
    pub lane_id: String,
    pub project_id: String,
    pub project_policy: HepaProjectRoutingPolicy,
    pub requested_capability: Option<String>,
    pub selected_adapter: String,
    pub route_source: HepaRoutingDecisionSource,
    pub manager_override: Option<HepaManagerRoutingOverride>,
    pub routing_table: BTreeMap<String, String>,
    pub default_adapter: String,
    pub review_fanout: HepaReviewFanout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaRoutingDecisionSource {
    Default,
    Capability,
    ManagerOverride,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaManagerRoutingOverride {
    pub adapter_id: String,
    pub reason: String,
}

impl HepaManagerRoutingOverride {
    fn validate(&self) -> Result<(), HepaRoutingError> {
        require_single_line("manager_override.adapter_id", &self.adapter_id)?;
        require_single_line("manager_override.reason", &self.reason)?;
        Ok(())
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

fn require_adapter_allowed(
    field: impl Into<String>,
    adapter_id: &str,
    project_policy: HepaProjectRoutingPolicy,
    adapter_specs: &BTreeMap<String, HepaAdapterSpec>,
) -> Result<(), HepaRoutingError> {
    let field = field.into();
    let Some(adapter_spec) = adapter_specs.get(adapter_id) else {
        return Err(HepaRoutingError::new(
            field,
            "missing adapter spec for route target",
        ));
    };
    adapter_spec
        .validate()
        .map_err(|error| HepaRoutingError::new(field.clone(), error.to_string()))?;
    if project_policy == HepaProjectRoutingPolicy::LocalOnly
        && adapter_spec.cost_class != HepaAdapterCostClass::Local
    {
        return Err(HepaRoutingError::new(
            field,
            "local-only project policy requires cost_class: local",
        ));
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
            project_policy: HepaProjectRoutingPolicy::Standard,
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

    fn sample_adapter(id: &str, cost_class: HepaAdapterCostClass) -> HepaAdapterSpec {
        HepaAdapterSpec {
            schema_version: crate::spec::ADAPTER_SPEC_SCHEMA_VERSION,
            id: id.to_string(),
            display_name: format!("{id} Adapter"),
            roles: vec![crate::spec::HepaAdapterRole::Worker],
            mode: crate::spec::HepaAdapterMode::Oneshot,
            command: "agent --prompt-file {prompt_file}".to_string(),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: vec!["agent".to_string()],
            required_env: Vec::new(),
            sandbox: crate::spec::HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["docs".to_string()],
            cost_class,
            resource_weight: 1,
            max_concurrency: 1,
            prompt_transport: crate::spec::HepaAdapterPromptTransport::PromptFile,
            output_capture: crate::spec::HepaAdapterOutputCapture::AdapterFile,
        }
    }

    fn sample_adapters(cost_class: HepaAdapterCostClass) -> BTreeMap<String, HepaAdapterSpec> {
        [
            "reviewer-a",
            "reviewer-b",
            "reviewer-c",
            "worker-design",
            "worker-docs",
            "worker-frontend",
            "worker-local",
            "worker-primary",
        ]
        .into_iter()
        .map(|id| (id.to_string(), sample_adapter(id, cost_class.clone())))
        .collect()
    }

    fn sample_decision_request(requested_capability: Option<&str>) -> HepaRoutingDecisionRequest {
        HepaRoutingDecisionRequest {
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            requested_capability: requested_capability.map(str::to_string),
            manager_override: None,
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

    #[test]
    fn local_only_project_policy_rejects_cloud_adapters() {
        let mut policy = sample_policy();
        policy.project_policy = HepaProjectRoutingPolicy::LocalOnly;
        let adapters = sample_adapters(HepaAdapterCostClass::PaidCloud);

        let error = policy
            .validate_against_adapters(&adapters)
            .expect_err("local-only projects must reject cloud adapters");

        assert_eq!(error.field, "default");
        assert!(error.message.contains("cost_class: local"));
    }

    #[test]
    fn local_only_project_policy_allows_local_adapters() {
        let mut policy = sample_policy();
        policy.project_policy = HepaProjectRoutingPolicy::LocalOnly;
        let adapters = sample_adapters(HepaAdapterCostClass::Local);

        let resolved = policy
            .resolve_adapter_spec(Some("docs"), &adapters)
            .expect("local-only projects may route to local adapters");

        assert_eq!(resolved.id, "worker-docs");
    }

    #[test]
    fn routing_decision_artifact_records_selected_route() {
        let policy = sample_policy();
        let adapters = sample_adapters(HepaAdapterCostClass::PaidCloud);

        let artifact = policy
            .decide_adapter(sample_decision_request(Some("frontend")), &adapters)
            .expect("routing decisions should produce artifacts");

        assert_eq!(artifact.task_id, "task-1");
        assert_eq!(artifact.requested_capability.as_deref(), Some("frontend"));
        assert_eq!(artifact.selected_adapter, "worker-frontend");
        assert_eq!(artifact.route_source, HepaRoutingDecisionSource::Capability);
        assert!(artifact.manager_override.is_none());
        assert_eq!(
            artifact.routing_table.get("frontend").map(String::as_str),
            Some("worker-frontend")
        );
    }

    #[test]
    fn routing_decision_resolves_deterministically_for_same_inputs() {
        let policy = sample_policy();
        let adapters = sample_adapters(HepaAdapterCostClass::PaidCloud);
        let first = policy
            .decide_adapter(sample_decision_request(Some("docs")), &adapters)
            .expect("first routing decision should resolve");
        let second = policy
            .decide_adapter(sample_decision_request(Some("docs")), &adapters)
            .expect("second routing decision should resolve");

        assert_eq!(first, second);
        assert_eq!(first.selected_adapter, "worker-docs");
    }

    #[test]
    fn unknown_capability_keys_fail_loudly() {
        let policy = sample_policy();

        let error = policy
            .resolve_adapter(Some("backend"))
            .expect_err("unknown capabilities must fail");

        assert_eq!(error.field, "capability");
        assert!(error.message.contains("unknown capability key"));
    }

    #[test]
    fn manager_override_requires_and_records_reason() {
        let policy = sample_policy();
        let adapters = sample_adapters(HepaAdapterCostClass::PaidCloud);
        let mut request = sample_decision_request(Some("docs"));
        request.manager_override = Some(HepaManagerRoutingOverride {
            adapter_id: "worker-design".to_string(),
            reason: "manager selected stronger design adapter".to_string(),
        });

        let artifact = policy
            .decide_adapter(request, &adapters)
            .expect("manager override should produce a routing artifact");

        assert_eq!(artifact.selected_adapter, "worker-design");
        assert_eq!(
            artifact.route_source,
            HepaRoutingDecisionSource::ManagerOverride
        );
        assert_eq!(
            artifact
                .manager_override
                .as_ref()
                .map(|entry| entry.reason.as_str()),
            Some("manager selected stronger design adapter")
        );
    }

    #[test]
    fn manager_override_without_reason_fails() {
        let policy = sample_policy();
        let adapters = sample_adapters(HepaAdapterCostClass::PaidCloud);
        let mut request = sample_decision_request(Some("docs"));
        request.manager_override = Some(HepaManagerRoutingOverride {
            adapter_id: "worker-design".to_string(),
            reason: " ".to_string(),
        });

        let error = policy
            .decide_adapter(request, &adapters)
            .expect_err("manager overrides must explain the reason");

        assert_eq!(error.field, "manager_override.reason");
    }
}
