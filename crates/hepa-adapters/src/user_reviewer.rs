use crate::spec::{
    ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterMode, HepaAdapterRole,
    HepaAdapterSandbox, HepaAdapterSpec, HepaAdapterSpecError,
};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaUserReviewerAdapterTemplate {
    pub id: String,
    pub display_name: String,
    pub review_command: String,
    pub required_commands: Vec<String>,
    pub env_allowlist: Vec<String>,
    pub max_concurrency: u32,
}

impl HepaUserReviewerAdapterTemplate {
    pub fn into_spec(self) -> Result<HepaAdapterSpec, HepaUserReviewerAdapterError> {
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: self.id,
            display_name: self.display_name,
            roles: vec![HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: self.review_command.clone(),
            review_command: Some(self.review_command),
            workdir: "{worktree}".to_string(),
            required_commands: self.required_commands,
            required_env: self.env_allowlist,
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["review".to_string()],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: self.max_concurrency,
        };
        spec.validate()
            .map_err(HepaUserReviewerAdapterError::from)?;
        Ok(spec)
    }
}

impl Default for HepaUserReviewerAdapterTemplate {
    fn default() -> Self {
        Self {
            id: "user-reviewer".to_string(),
            display_name: "User Reviewer Adapter".to_string(),
            review_command: "user-reviewer --prompt-file {review_prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {review_output_file}".to_string(),
            required_commands: vec!["user-reviewer".to_string()],
            env_allowlist: Vec::new(),
            max_concurrency: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaUserReviewerAdapterError {
    pub field: String,
    pub message: String,
}

impl fmt::Display for HepaUserReviewerAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaUserReviewerAdapterError {}

impl From<HepaAdapterSpecError> for HepaUserReviewerAdapterError {
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
    fn user_reviewer_template_builds_provider_neutral_reviewer_spec() {
        let spec = HepaUserReviewerAdapterTemplate {
            review_command: "agent-reviewer --prompt-file {review_prompt_file} --worktree {worktree} --json-output {review_output_file}".to_string(),
            required_commands: vec!["agent-reviewer".to_string()],
            env_allowlist: vec!["REVIEW_PROFILE".to_string(), "MODEL_NAME".to_string()],
            ..HepaUserReviewerAdapterTemplate::default()
        }
        .into_spec()
        .expect("user reviewer spec should validate");

        assert_eq!(spec.id, "user-reviewer");
        assert_eq!(spec.roles, vec![HepaAdapterRole::Reviewer]);
        assert_eq!(spec.mode, HepaAdapterMode::Oneshot);
        assert!(spec.review_command.is_some());
        assert_eq!(spec.required_env, vec!["REVIEW_PROFILE", "MODEL_NAME"]);
        assert_eq!(spec.capabilities, vec!["review"]);
        assert_eq!(spec.cost_class, HepaAdapterCostClass::PaidCloud);
        assert!(
            spec.render_review_command(&context())
                .expect("review command renders")
                .is_some()
        );
    }

    #[test]
    fn user_reviewer_template_rejects_manager_only_env_allowlist() {
        let error = HepaUserReviewerAdapterTemplate {
            env_allowlist: vec![["GITHUB", "TOKEN"].join("_")],
            ..HepaUserReviewerAdapterTemplate::default()
        }
        .into_spec()
        .expect_err("manager-only env must fail");

        assert_eq!(error.field, "required_env[0]");
        assert!(error.message.contains("secret-like"));
    }

    #[test]
    fn user_reviewer_template_rejects_raw_task_placeholders() {
        let error = HepaUserReviewerAdapterTemplate {
            review_command: "agent-reviewer --task {raw_task}".to_string(),
            ..HepaUserReviewerAdapterTemplate::default()
        }
        .into_spec()
        .expect_err("raw task placeholders must fail");

        assert_eq!(error.field, "command");
        assert!(error.message.contains("raw_task"));
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
