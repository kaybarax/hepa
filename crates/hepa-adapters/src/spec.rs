use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt};

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

impl HepaAdapterSpec {
    pub fn render_worker_command(
        &self,
        context: &HepaAdapterTemplateContext,
    ) -> Result<String, HepaAdapterTemplateError> {
        render_command_template(&self.command, context)
    }

    pub fn render_review_command(
        &self,
        context: &HepaAdapterTemplateContext,
    ) -> Result<Option<String>, HepaAdapterTemplateError> {
        self.review_command
            .as_deref()
            .map(|template| render_command_template(template, context))
            .transpose()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaAdapterTemplateContext {
    pub prompt_file: String,
    pub worktree: String,
    pub review_prompt_file: String,
    pub output_file: String,
    pub review_output_file: String,
    pub artifact_dir: String,
}

impl HepaAdapterTemplateContext {
    fn placeholders(&self) -> BTreeMap<&'static str, &str> {
        BTreeMap::from([
            ("artifact_dir", self.artifact_dir.as_str()),
            ("output_file", self.output_file.as_str()),
            ("prompt_file", self.prompt_file.as_str()),
            ("review_output_file", self.review_output_file.as_str()),
            ("review_prompt_file", self.review_prompt_file.as_str()),
            ("worktree", self.worktree.as_str()),
        ])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaAdapterTemplateError {
    pub placeholder: String,
    pub message: String,
}

impl HepaAdapterTemplateError {
    fn new(placeholder: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            placeholder: placeholder.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaAdapterTemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.placeholder, self.message)
    }
}

impl Error for HepaAdapterTemplateError {}

pub fn render_command_template(
    template: &str,
    context: &HepaAdapterTemplateContext,
) -> Result<String, HepaAdapterTemplateError> {
    let placeholders = context.placeholders();
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find('{') {
        rendered.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('}') else {
            return Err(HepaAdapterTemplateError::new(
                rest[start..].to_string(),
                "unterminated placeholder",
            ));
        };
        let name = &after_start[..end];
        let Some(value) = placeholders.get(name) else {
            return Err(HepaAdapterTemplateError::new(
                name,
                "unknown adapter command placeholder",
            ));
        };
        rendered.push_str(value);
        rest = &after_start[end + 1..];
    }
    if rest.contains('}') {
        return Err(HepaAdapterTemplateError::new(
            "}",
            "unmatched closing brace",
        ));
    }
    rendered.push_str(rest);
    Ok(rendered)
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

    #[test]
    fn command_templates_render_only_allowed_placeholders() {
        let context = HepaAdapterTemplateContext {
            prompt_file: "<RUN_DIR>/prompt.md".to_string(),
            worktree: "<WORKTREE>".to_string(),
            review_prompt_file: "<RUN_DIR>/review.md".to_string(),
            output_file: "<RUN_DIR>/worker.json".to_string(),
            review_output_file: "<RUN_DIR>/review.json".to_string(),
            artifact_dir: "<RUN_DIR>".to_string(),
        };

        let rendered = render_command_template(
            "{agent_binary} --prompt-file {prompt_file} --workdir {worktree} --json-output {output_file}",
            &context,
        )
        .expect_err("agent_binary is user command text, not a HEPA path placeholder");
        assert_eq!(rendered.placeholder, "agent_binary");

        let rendered = render_command_template(
            "agent --prompt-file {prompt_file} --workdir {worktree} --json-output {output_file}",
            &context,
        )
        .expect("known placeholders should render");

        assert_eq!(
            rendered,
            "agent --prompt-file <RUN_DIR>/prompt.md --workdir <WORKTREE> --json-output <RUN_DIR>/worker.json"
        );
    }

    #[test]
    fn command_templates_reject_malformed_placeholders() {
        let context = HepaAdapterTemplateContext {
            prompt_file: "<RUN_DIR>/prompt.md".to_string(),
            worktree: "<WORKTREE>".to_string(),
            review_prompt_file: "<RUN_DIR>/review.md".to_string(),
            output_file: "<RUN_DIR>/worker.json".to_string(),
            review_output_file: "<RUN_DIR>/review.json".to_string(),
            artifact_dir: "<RUN_DIR>".to_string(),
        };

        let error = render_command_template("agent --prompt-file {prompt_file", &context)
            .expect_err("unterminated placeholders must fail");

        assert!(error.message.contains("unterminated"));
    }

    #[test]
    fn raw_task_text_is_not_a_supported_command_placeholder() {
        let context = HepaAdapterTemplateContext {
            prompt_file: "<RUN_DIR>/prompt.md".to_string(),
            worktree: "<WORKTREE>".to_string(),
            review_prompt_file: "<RUN_DIR>/review.md".to_string(),
            output_file: "<RUN_DIR>/worker.json".to_string(),
            review_output_file: "<RUN_DIR>/review.json".to_string(),
            artifact_dir: "<RUN_DIR>".to_string(),
        };
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "unsafe-template".to_string(),
            display_name: "Unsafe Template".to_string(),
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: "agent --task {task_text} --prompt-file {prompt_file}".to_string(),
            review_command: Some(
                "reviewer --task {raw_task} --prompt-file {review_prompt_file}".to_string(),
            ),
            workdir: "{worktree}".to_string(),
            required_commands: vec!["agent".to_string()],
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: false,
            supports_json_output: true,
            capabilities: vec!["docs".to_string()],
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 1,
        };

        let worker_error = spec
            .render_worker_command(&context)
            .expect_err("raw task placeholders must not render");
        let review_error = spec
            .render_review_command(&context)
            .expect_err("raw task placeholders must not render in review commands");

        assert_eq!(worker_error.placeholder, "task_text");
        assert_eq!(review_error.placeholder, "raw_task");
    }
}
