use crate::spec::{
    ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterMode, HepaAdapterRole,
    HepaAdapterSandbox, HepaAdapterSpec, HepaAdapterSpecError,
};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaCustomAdapterTemplate {
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

impl HepaCustomAdapterTemplate {
    pub fn into_spec(self) -> Result<HepaAdapterSpec, HepaCustomAdapterError> {
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: self.id,
            display_name: self.display_name,
            roles: self.roles,
            mode: self.mode,
            command: self.command,
            review_command: self.review_command,
            workdir: self.workdir,
            required_commands: self.required_commands,
            required_env: self.required_env,
            sandbox: self.sandbox,
            supports_resume: self.supports_resume,
            supports_json_output: self.supports_json_output,
            capabilities: self.capabilities,
            cost_class: self.cost_class,
            resource_weight: self.resource_weight,
            max_concurrency: self.max_concurrency,
        };
        spec.validate().map_err(HepaCustomAdapterError::from)?;
        Ok(spec)
    }
}

impl Default for HepaCustomAdapterTemplate {
    fn default() -> Self {
        Self {
            id: "custom".to_string(),
            display_name: "Custom Adapter".to_string(),
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: "custom-adapter --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file}".to_string(),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: vec!["custom-adapter".to_string()],
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: false,
            supports_json_output: true,
            capabilities: vec!["docs".to_string()],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaCustomAdapterError {
    pub field: String,
    pub message: String,
}

impl fmt::Display for HepaCustomAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaCustomAdapterError {}

impl From<HepaAdapterSpecError> for HepaCustomAdapterError {
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
    use crate::spec::HepaAdapterTemplateContext;

    #[test]
    fn custom_adapter_template_builds_contract_conforming_spec() {
        let spec = HepaCustomAdapterTemplate {
            id: "custom-docs".to_string(),
            display_name: "Custom Docs Adapter".to_string(),
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            command: "custom-docs --prompt-file {prompt_file} --json-output {output_file}"
                .to_string(),
            review_command: Some(
                "custom-review --prompt-file {review_prompt_file} --json-output {review_output_file}"
                    .to_string(),
            ),
            required_commands: vec!["custom-docs".to_string(), "custom-review".to_string()],
            required_env: vec!["CUSTOM_PROFILE".to_string()],
            capabilities: vec!["docs".to_string(), "review".to_string()],
            ..HepaCustomAdapterTemplate::default()
        }
        .into_spec()
        .expect("custom adapter should validate");

        assert_eq!(spec.id, "custom-docs");
        assert_eq!(
            spec.roles,
            vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer]
        );
        assert_eq!(spec.workdir, "{worktree}");
        assert!(spec.supports_json_output);
        spec.render_worker_command(&context())
            .expect("worker command renders");
        assert!(
            spec.render_review_command(&context())
                .expect("review command renders")
                .is_some()
        );
    }

    #[test]
    fn custom_adapter_template_rejects_unknown_placeholders() {
        let error = HepaCustomAdapterTemplate {
            command: "custom --raw-task {task_text}".to_string(),
            ..HepaCustomAdapterTemplate::default()
        }
        .into_spec()
        .expect_err("unknown placeholders must fail");

        assert_eq!(error.field, "command");
        assert!(error.message.contains("task_text"));
    }

    #[test]
    fn custom_adapter_template_rejects_secret_like_env() {
        let secret_env_name = ["GITHUB", "TOKEN"].join("_");
        let error = HepaCustomAdapterTemplate {
            required_env: vec![secret_env_name],
            ..HepaCustomAdapterTemplate::default()
        }
        .into_spec()
        .expect_err("manager credentials must fail");

        assert_eq!(error.field, "required_env[0]");
        assert!(error.message.contains("secret-like"));
    }

    fn context() -> HepaAdapterTemplateContext {
        HepaAdapterTemplateContext {
            prompt_file: "<PROMPT_FILE>".to_string(),
            worktree: "<WORKTREE>".to_string(),
            review_prompt_file: "<REVIEW_PROMPT_FILE>".to_string(),
            output_file: "<OUTPUT_FILE>".to_string(),
            review_output_file: "<REVIEW_OUTPUT_FILE>".to_string(),
            artifact_dir: "<ARTIFACT_DIR>".to_string(),
        }
    }
}
