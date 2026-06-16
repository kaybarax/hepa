use serde::{Deserialize, Serialize};

pub const ADAPTER_SPEC_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaAdapterSpec {
    pub schema_version: u32,
    pub id: String,
    pub display_name: String,
    pub roles: Vec<HepaAdapterRole>,
    pub mode: HepaAdapterMode,
    pub command: String,
    pub review_command: Option<String>,
    pub workdir: String,
    pub required_commands: Vec<String>,
    pub required_env: Vec<String>,
    pub sandbox: HepaAdapterSandbox,
    pub supports_resume: bool,
    pub supports_json_output: bool,
    pub capabilities: Vec<String>,
    pub cost_class: HepaAdapterCostClass,
    pub resource_weight: u32,
    pub max_concurrency: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaAdapterRole {
    Worker,
    Reviewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaAdapterMode {
    Oneshot,
    Interactive,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HepaAdapterSandbox {
    None,
    AgentNative,
    Container,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HepaAdapterCostClass {
    PaidCloud,
    FreeTier,
    Local,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_adapter_spec_uses_architecture_field_names() {
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "worker-primary".to_string(),
            display_name: "Primary Worker Adapter".to_string(),
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: "{agent_binary} --prompt-file {prompt_file} --json-output {output_file}"
                .to_string(),
            review_command: Some(
                "{review_binary} --prompt-file {review_prompt_file} --json-output {review_output_file}"
                    .to_string(),
            ),
            workdir: "{worktree}".to_string(),
            required_commands: vec!["{agent_binary}".to_string()],
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["frontend".to_string(), "docs".to_string()],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 2,
        };

        let json = serde_json::to_string(&spec).expect("adapter spec should serialize");

        assert!(json.contains("\"id\":\"worker-primary\""));
        assert!(json.contains("\"roles\":[\"worker\",\"reviewer\"]"));
        assert!(json.contains("\"mode\":\"oneshot\""));
        assert!(json.contains("\"sandbox\":\"agent-native\""));
        assert!(json.contains("\"cost_class\":\"paid-cloud\""));
        assert!(json.contains("\"max_concurrency\":2"));
    }
}
