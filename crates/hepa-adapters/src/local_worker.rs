use crate::spec::{
    ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterMode, HepaAdapterRole,
    HepaAdapterSandbox, HepaAdapterSpec, HepaAdapterSpecError,
};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaLocalWorkerAdapterTemplate {
    pub id: String,
    pub display_name: String,
    pub command: String,
    pub review_command: String,
    pub required_commands: Vec<String>,
    pub capabilities: Vec<String>,
    pub max_concurrency: u32,
}

impl HepaLocalWorkerAdapterTemplate {
    pub fn into_spec(self) -> Result<HepaAdapterSpec, HepaLocalWorkerAdapterError> {
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: self.id,
            display_name: self.display_name,
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: self.command,
            review_command: Some(self.review_command),
            workdir: "{worktree}".to_string(),
            required_commands: self.required_commands,
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: self.capabilities,
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: self.max_concurrency,
            prompt_transport: crate::spec::HepaAdapterPromptTransport::PromptFile,
            output_capture: crate::spec::HepaAdapterOutputCapture::AdapterFile,
        };
        spec.validate().map_err(HepaLocalWorkerAdapterError::from)?;
        Ok(spec)
    }
}

impl Default for HepaLocalWorkerAdapterTemplate {
    fn default() -> Self {
        Self {
            id: "local-worker".to_string(),
            display_name: "Local Worker Adapter".to_string(),
            command: "local-worker --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file}".to_string(),
            review_command: "local-worker --review --prompt-file {review_prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {review_output_file}".to_string(),
            required_commands: vec!["local-worker".to_string()],
            capabilities: vec![
                "frontend".to_string(),
                "backend".to_string(),
                "refactor".to_string(),
                "docs".to_string(),
                "review".to_string(),
                "local-only".to_string(),
            ],
            max_concurrency: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaLocalWorkerAdapterError {
    pub field: String,
    pub message: String,
}

impl fmt::Display for HepaLocalWorkerAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaLocalWorkerAdapterError {}

impl From<HepaAdapterSpecError> for HepaLocalWorkerAdapterError {
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
    fn local_worker_template_is_local_only_and_review_capable() {
        let spec = HepaLocalWorkerAdapterTemplate::default()
            .into_spec()
            .expect("local worker spec should validate");

        assert_eq!(spec.id, "local-worker");
        assert_eq!(
            spec.roles,
            vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer]
        );
        assert_eq!(spec.cost_class, HepaAdapterCostClass::Local);
        assert!(spec.required_env.is_empty());
        assert!(spec.review_command.is_some());
        assert!(spec.capabilities.contains(&"local-only".to_string()));
        assert!(spec.capabilities.contains(&"review".to_string()));
    }

    #[test]
    fn local_worker_template_rejects_cloud_secret_env_by_omission() {
        let spec = HepaLocalWorkerAdapterTemplate::default()
            .into_spec()
            .expect("local worker spec should validate");

        assert!(spec.required_env.is_empty());
        assert_eq!(spec.cost_class, HepaAdapterCostClass::Local);
    }

    #[test]
    fn local_worker_template_rejects_invalid_placeholders() {
        let error = HepaLocalWorkerAdapterTemplate {
            command: "local-worker --raw {task_text}".to_string(),
            ..HepaLocalWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect_err("raw task placeholder must fail");

        assert_eq!(error.field, "command");
        assert!(error.message.contains("task_text"));
    }
}
