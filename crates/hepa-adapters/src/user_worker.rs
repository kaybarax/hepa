use crate::spec::{
    ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterMode, HepaAdapterRole,
    HepaAdapterSandbox, HepaAdapterSpec, HepaAdapterSpecError,
};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaUserWorkerAdapterTemplate {
    pub id: String,
    pub display_name: String,
    pub command: String,
    pub required_commands: Vec<String>,
    pub env_allowlist: Vec<String>,
    pub capabilities: Vec<String>,
    pub max_concurrency: u32,
}

impl HepaUserWorkerAdapterTemplate {
    pub fn into_spec(self) -> Result<HepaAdapterSpec, HepaUserWorkerAdapterError> {
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: self.id,
            display_name: self.display_name,
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: self.command,
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: self.required_commands,
            required_env: self.env_allowlist,
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: self.capabilities,
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: self.max_concurrency,
            prompt_transport: crate::spec::HepaAdapterPromptTransport::PromptFile,
            output_capture: crate::spec::HepaAdapterOutputCapture::AdapterFile,
        };
        spec.validate().map_err(HepaUserWorkerAdapterError::from)?;
        Ok(spec)
    }
}

impl Default for HepaUserWorkerAdapterTemplate {
    fn default() -> Self {
        Self {
            id: "user-worker".to_string(),
            display_name: "User Worker Adapter".to_string(),
            command: "user-worker --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file}".to_string(),
            required_commands: vec!["user-worker".to_string()],
            env_allowlist: Vec::new(),
            capabilities: vec![
                "frontend".to_string(),
                "backend".to_string(),
                "refactor".to_string(),
                "docs".to_string(),
            ],
            max_concurrency: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaUserWorkerAdapterError {
    pub field: String,
    pub message: String,
}

impl fmt::Display for HepaUserWorkerAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaUserWorkerAdapterError {}

impl From<HepaAdapterSpecError> for HepaUserWorkerAdapterError {
    fn from(error: HepaAdapterSpecError) -> Self {
        Self {
            field: error.field,
            message: error.message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_worker_template_builds_provider_neutral_worker_spec() {
        let spec = HepaUserWorkerAdapterTemplate {
            command: "agent-worker --prompt-file {prompt_file} --worktree {worktree} --json-output {output_file}".to_string(),
            required_commands: vec!["agent-worker".to_string()],
            env_allowlist: vec!["AGENT_PROFILE".to_string(), "MODEL_NAME".to_string()],
            ..HepaUserWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect("user worker spec should validate");

        assert_eq!(spec.id, "user-worker");
        assert_eq!(spec.roles, vec![HepaAdapterRole::Worker]);
        assert_eq!(spec.mode, HepaAdapterMode::Oneshot);
        assert_eq!(spec.review_command, None);
        assert_eq!(spec.sandbox, HepaAdapterSandbox::AgentNative);
        assert_eq!(spec.required_env, vec!["AGENT_PROFILE", "MODEL_NAME"]);
        assert!(spec.supports_resume);
        assert_eq!(spec.cost_class, HepaAdapterCostClass::PaidCloud);
        assert!(spec.capabilities.contains(&"docs".to_string()));
    }

    #[test]
    fn user_worker_template_rejects_manager_only_env_allowlist() {
        let error = HepaUserWorkerAdapterTemplate {
            env_allowlist: vec![["GITHUB", "TOKEN"].join("_")],
            ..HepaUserWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect_err("manager-only env must fail");

        assert_eq!(error.field, "required_env[0]");
        assert!(error.message.contains("manager-only"));
    }

    #[test]
    fn user_worker_template_rejects_non_contract_command_placeholders() {
        let error = HepaUserWorkerAdapterTemplate {
            command: "agent-worker --task {task_text}".to_string(),
            ..HepaUserWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect_err("raw task placeholders must fail");

        assert_eq!(error.field, "command");
        assert!(error.message.contains("task_text"));
    }
}
