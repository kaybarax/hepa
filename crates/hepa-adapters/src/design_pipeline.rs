use crate::{
    routing::{
        HepaManagerRoutingOverride, HepaRoutingDecisionArtifact, HepaRoutingDecisionRequest,
        HepaRoutingError, HepaRoutingPolicy,
    },
    spec::{HepaAdapterRole, HepaAdapterSpec},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt};

pub const DESIGN_PIPELINE_SCHEMA_VERSION: u32 = 1;
pub const DESIGN_SPEC_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaDesignSpecPipelineRequest {
    pub task_id: String,
    pub lane_id: String,
    pub design_capability: String,
    pub implementation_capability: String,
    pub design_artifact_ref: String,
    pub manager_design_override: Option<HepaManagerRoutingOverride>,
    pub manager_implementation_override: Option<HepaManagerRoutingOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaDesignSpecPipelinePlan {
    pub schema_version: u32,
    pub task_id: String,
    pub lane_id: String,
    pub design_route: HepaRoutingDecisionArtifact,
    pub implementation_route: HepaRoutingDecisionArtifact,
    pub design_artifact_ref: String,
    pub implementation_prompt: String,
}

impl HepaRoutingPolicy {
    pub fn plan_design_spec_pipeline(
        &self,
        request: HepaDesignSpecPipelineRequest,
        adapter_specs: &BTreeMap<String, HepaAdapterSpec>,
    ) -> Result<HepaDesignSpecPipelinePlan, HepaDesignPipelineError> {
        request.validate()?;
        let design_route = self.decide_adapter(
            HepaRoutingDecisionRequest {
                task_id: request.task_id.clone(),
                lane_id: request.lane_id.clone(),
                requested_capability: Some(request.design_capability.clone()),
                manager_override: request.manager_design_override,
            },
            adapter_specs,
        )?;
        let implementation_route = self.decide_adapter(
            HepaRoutingDecisionRequest {
                task_id: request.task_id.clone(),
                lane_id: request.lane_id.clone(),
                requested_capability: Some(request.implementation_capability.clone()),
                manager_override: request.manager_implementation_override,
            },
            adapter_specs,
        )?;
        require_worker_capability(
            "design_route.selected_adapter",
            &design_route.selected_adapter,
            &request.design_capability,
            adapter_specs,
        )?;
        require_worker_capability(
            "implementation_route.selected_adapter",
            &implementation_route.selected_adapter,
            &request.implementation_capability,
            adapter_specs,
        )?;

        Ok(HepaDesignSpecPipelinePlan {
            schema_version: DESIGN_PIPELINE_SCHEMA_VERSION,
            task_id: request.task_id,
            lane_id: request.lane_id,
            design_route,
            implementation_route,
            implementation_prompt: implementation_prompt(&request.design_artifact_ref),
            design_artifact_ref: request.design_artifact_ref,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaDesignSpecArtifact {
    pub schema_version: u32,
    pub task_id: String,
    pub lane_id: String,
    pub adapter_id: String,
    pub format: HepaDesignSpecFormat,
    pub html: String,
    pub css: String,
    pub notes: Vec<String>,
}

impl HepaDesignSpecArtifact {
    pub fn validate(&self) -> Result<(), HepaDesignPipelineError> {
        require_schema(
            "schema_version",
            self.schema_version,
            DESIGN_SPEC_SCHEMA_VERSION,
        )?;
        require_single_line("task_id", &self.task_id)?;
        require_single_line("lane_id", &self.lane_id)?;
        require_single_line("adapter_id", &self.adapter_id)?;
        require_non_empty("html", &self.html)?;
        require_non_empty("css", &self.css)?;
        reject_active_content("html", &self.html)?;
        reject_active_content("css", &self.css)?;
        for (index, note) in self.notes.iter().enumerate() {
            require_single_line(format!("notes[{index}]"), note)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HepaDesignSpecFormat {
    HtmlCss,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaDesignPipelineError {
    pub field: String,
    pub message: String,
}

impl HepaDesignPipelineError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaDesignPipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaDesignPipelineError {}

impl From<HepaRoutingError> for HepaDesignPipelineError {
    fn from(error: HepaRoutingError) -> Self {
        Self::new(error.field, error.message)
    }
}

impl HepaDesignSpecPipelineRequest {
    fn validate(&self) -> Result<(), HepaDesignPipelineError> {
        require_single_line("task_id", &self.task_id)?;
        require_single_line("lane_id", &self.lane_id)?;
        require_single_line("design_capability", &self.design_capability)?;
        require_single_line("implementation_capability", &self.implementation_capability)?;
        require_artifact_ref("design_artifact_ref", &self.design_artifact_ref)?;
        Ok(())
    }
}

fn require_worker_capability(
    field: &str,
    adapter_id: &str,
    capability: &str,
    adapter_specs: &BTreeMap<String, HepaAdapterSpec>,
) -> Result<(), HepaDesignPipelineError> {
    let Some(spec) = adapter_specs.get(adapter_id) else {
        return Err(HepaDesignPipelineError::new(
            field,
            "missing adapter spec for route target",
        ));
    };
    if !spec.roles.contains(&HepaAdapterRole::Worker) {
        return Err(HepaDesignPipelineError::new(
            field,
            "selected adapter must support worker role",
        ));
    }
    if !spec.capabilities.iter().any(|value| value == capability) {
        return Err(HepaDesignPipelineError::new(
            field,
            format!("selected adapter must declare `{capability}` capability"),
        ));
    }
    Ok(())
}

fn implementation_prompt(design_artifact_ref: &str) -> String {
    format!(
        "Build the implementation from the approved design spec artifact `{design_artifact_ref}`.\n\
         Treat the design artifact as input only: preserve HEPA safety gates, make only scoped code changes, \
         and report any mismatch between the HTML/CSS spec and repository constraints."
    )
}

fn require_schema(field: &str, actual: u32, expected: u32) -> Result<(), HepaDesignPipelineError> {
    if actual == expected {
        Ok(())
    } else {
        Err(HepaDesignPipelineError::new(
            field,
            format!("must be {expected}"),
        ))
    }
}

fn require_artifact_ref(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaDesignPipelineError> {
    let field = field.into();
    require_single_line(field.clone(), value)?;
    if value.starts_with('/') || value.contains("..") {
        return Err(HepaDesignPipelineError::new(
            field,
            "must be a relative artifact reference without traversal",
        ));
    }
    Ok(())
}

fn require_non_empty(field: impl Into<String>, value: &str) -> Result<(), HepaDesignPipelineError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaDesignPipelineError::new(field, "must not be empty"));
    }
    Ok(())
}

fn require_single_line(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaDesignPipelineError> {
    let field = field.into();
    require_non_empty(field.clone(), value)?;
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaDesignPipelineError::new(field, "must be a single line"));
    }
    Ok(())
}

fn reject_active_content(
    field: impl Into<String>,
    value: &str,
) -> Result<(), HepaDesignPipelineError> {
    let field = field.into();
    let lowered = value.to_ascii_lowercase();
    if lowered.contains("<script") || lowered.contains("javascript:") {
        return Err(HepaDesignPipelineError::new(
            field,
            "must not contain active script content",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        routing::{HepaProjectRoutingPolicy, HepaReviewFanout, HepaReviewPassPolicy},
        spec::{
            ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterMode,
            HepaAdapterOutputCapture, HepaAdapterPromptTransport, HepaAdapterSandbox,
        },
    };

    #[test]
    fn design_spec_pipeline_routes_design_then_implementation() {
        let policy = policy();
        let adapters = adapters();

        let plan = policy
            .plan_design_spec_pipeline(
                HepaDesignSpecPipelineRequest {
                    task_id: "task-1".to_string(),
                    lane_id: "lane-1".to_string(),
                    design_capability: "design".to_string(),
                    implementation_capability: "frontend".to_string(),
                    design_artifact_ref: "lanes/lane-1/design/spec.html.json".to_string(),
                    manager_design_override: None,
                    manager_implementation_override: None,
                },
                &adapters,
            )
            .expect("design pipeline should plan");

        assert_eq!(plan.design_route.selected_adapter, "worker-design");
        assert_eq!(
            plan.implementation_route.selected_adapter,
            "worker-frontend"
        );
        assert!(plan.implementation_prompt.contains("spec.html.json"));
        assert_eq!(plan.schema_version, DESIGN_PIPELINE_SCHEMA_VERSION);
    }

    #[test]
    fn design_spec_pipeline_rejects_adapter_without_capability() {
        let mut policy = policy();
        policy
            .capability_routes
            .insert("design".to_string(), "worker-frontend".to_string());
        let adapters = adapters();

        let error = policy
            .plan_design_spec_pipeline(request(), &adapters)
            .expect_err("design route must land on design-capable adapter");

        assert_eq!(error.field, "design_route.selected_adapter");
        assert!(error.message.contains("design"));
    }

    #[test]
    fn design_spec_artifact_validates_html_css_without_script() {
        let artifact = HepaDesignSpecArtifact {
            schema_version: DESIGN_SPEC_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            adapter_id: "worker-design".to_string(),
            format: HepaDesignSpecFormat::HtmlCss,
            html: "<main><h1>Settings</h1></main>".to_string(),
            css: "main { display: grid; }".to_string(),
            notes: vec!["Mobile and desktop layouts included.".to_string()],
        };

        artifact.validate().expect("artifact should validate");
    }

    #[test]
    fn design_spec_artifact_rejects_active_content() {
        let mut artifact = HepaDesignSpecArtifact {
            schema_version: DESIGN_SPEC_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            adapter_id: "worker-design".to_string(),
            format: HepaDesignSpecFormat::HtmlCss,
            html: "<script>alert(1)</script>".to_string(),
            css: "main { display: grid; }".to_string(),
            notes: Vec::new(),
        };

        let error = artifact
            .validate()
            .expect_err("active script content must be rejected");
        assert_eq!(error.field, "html");

        artifact.html = "<main>ok</main>".to_string();
        artifact.css = "a { background: url(javascript:alert(1)); }".to_string();
        let error = artifact
            .validate()
            .expect_err("javascript CSS URLs must be rejected");
        assert_eq!(error.field, "css");
    }

    fn request() -> HepaDesignSpecPipelineRequest {
        HepaDesignSpecPipelineRequest {
            task_id: "task-1".to_string(),
            lane_id: "lane-1".to_string(),
            design_capability: "design".to_string(),
            implementation_capability: "frontend".to_string(),
            design_artifact_ref: "lanes/lane-1/design/spec.html.json".to_string(),
            manager_design_override: None,
            manager_implementation_override: None,
        }
    }

    fn policy() -> HepaRoutingPolicy {
        HepaRoutingPolicy {
            schema_version: crate::routing::ROUTING_POLICY_SCHEMA_VERSION,
            project_id: "project-1".to_string(),
            project_policy: HepaProjectRoutingPolicy::Standard,
            default: "worker-frontend".to_string(),
            capability_routes: BTreeMap::from([
                ("design".to_string(), "worker-design".to_string()),
                ("frontend".to_string(), "worker-frontend".to_string()),
            ]),
            review_fanout: HepaReviewFanout {
                adapters: vec!["reviewer".to_string()],
                pass_policy: HepaReviewPassPolicy::All,
            },
        }
    }

    fn adapters() -> BTreeMap<String, HepaAdapterSpec> {
        [
            ("worker-design", vec!["design"]),
            ("worker-frontend", vec!["frontend"]),
            ("reviewer", vec!["review"]),
        ]
        .into_iter()
        .map(|(id, capabilities)| (id.to_string(), adapter(id, capabilities)))
        .collect()
    }

    fn adapter(id: &str, capabilities: Vec<&str>) -> HepaAdapterSpec {
        HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: id.to_string(),
            display_name: format!("{id} Adapter"),
            roles: if id == "reviewer" {
                vec![HepaAdapterRole::Reviewer]
            } else {
                vec![HepaAdapterRole::Worker]
            },
            mode: HepaAdapterMode::Oneshot,
            command: "agent --prompt-file {prompt_file}".to_string(),
            review_command: Some("agent --prompt-file {review_prompt_file}".to_string()),
            workdir: "{worktree}".to_string(),
            required_commands: vec!["agent".to_string()],
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: capabilities.into_iter().map(str::to_string).collect(),
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 1,
            prompt_transport: HepaAdapterPromptTransport::PromptFile,
            output_capture: HepaAdapterOutputCapture::AdapterFile,
        }
    }
}
