use crate::spec::{
    ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterMode, HepaAdapterRole,
    HepaAdapterSandbox, HepaAdapterSpec, HepaAdapterSpecError,
};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaExternalWorkerAdapterTemplate {
    pub id: String,
    pub display_name: String,
    pub status_command: String,
    pub required_commands: Vec<String>,
    pub env_allowlist: Vec<String>,
    pub max_concurrency: u32,
}

impl HepaExternalWorkerAdapterTemplate {
    pub fn into_spec(self) -> Result<HepaAdapterSpec, HepaExternalWorkerAdapterError> {
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: self.id,
            display_name: self.display_name,
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::External,
            command: self.status_command,
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: self.required_commands,
            required_env: self.env_allowlist,
            sandbox: HepaAdapterSandbox::None,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["external".to_string(), "status".to_string()],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: self.max_concurrency,
            prompt_transport: crate::spec::HepaAdapterPromptTransport::PromptFile,
            output_capture: crate::spec::HepaAdapterOutputCapture::AdapterFile,
        };
        spec.validate()
            .map_err(HepaExternalWorkerAdapterError::from)?;
        Ok(spec)
    }
}

impl Default for HepaExternalWorkerAdapterTemplate {
    fn default() -> Self {
        Self {
            id: "external-worker".to_string(),
            display_name: "External Worker Adapter".to_string(),
            status_command: "external-worker --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file}".to_string(),
            required_commands: vec!["external-worker".to_string()],
            env_allowlist: Vec::new(),
            max_concurrency: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaExternalWorkerAdapterError {
    pub field: String,
    pub message: String,
}

impl fmt::Display for HepaExternalWorkerAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaExternalWorkerAdapterError {}

impl From<HepaAdapterSpecError> for HepaExternalWorkerAdapterError {
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
    fn external_worker_template_builds_external_status_spec() {
        let spec = HepaExternalWorkerAdapterTemplate {
            status_command: "external-status --prompt-file {prompt_file} --artifact-dir {artifact_dir} --json-output {output_file}".to_string(),
            required_commands: vec!["external-status".to_string()],
            env_allowlist: vec!["EXTERNAL_QUEUE".to_string()],
            ..HepaExternalWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect("external worker spec should validate");

        assert_eq!(spec.id, "external-worker");
        assert_eq!(spec.roles, vec![HepaAdapterRole::Worker]);
        assert_eq!(spec.mode, HepaAdapterMode::External);
        assert_eq!(spec.sandbox, HepaAdapterSandbox::None);
        assert_eq!(spec.capabilities, vec!["external", "status"]);
        assert_eq!(spec.required_env, vec!["EXTERNAL_QUEUE"]);
        assert!(spec.supports_resume);
    }

    #[test]
    fn external_worker_template_rejects_manager_only_env_allowlist() {
        let error = HepaExternalWorkerAdapterTemplate {
            env_allowlist: vec![["GITHUB", "TOKEN"].join("_")],
            ..HepaExternalWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect_err("manager-only env must fail");

        assert_eq!(error.field, "required_env[0]");
        assert!(error.message.contains("manager-only"));
    }

    #[test]
    fn external_worker_template_rejects_invalid_status_placeholders() {
        let error = HepaExternalWorkerAdapterTemplate {
            status_command: "external-status --raw {task_text}".to_string(),
            ..HepaExternalWorkerAdapterTemplate::default()
        }
        .into_spec()
        .expect_err("raw task placeholder must fail");

        assert_eq!(error.field, "command");
        assert!(error.message.contains("task_text"));
    }
}
