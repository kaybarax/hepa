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
    #[serde(default)]
    pub prompt_transport: HepaAdapterPromptTransport,
    #[serde(default)]
    pub output_capture: HepaAdapterOutputCapture,
}

impl HepaAdapterSpec {
    pub fn validate(&self) -> Result<(), HepaAdapterSpecError> {
        require_schema(self.schema_version)?;
        require_single_line("id", &self.id)?;
        require_non_empty("display_name", &self.display_name)?;
        if self.roles.is_empty() {
            return Err(HepaAdapterSpecError::new(
                "roles",
                "must include at least one role",
            ));
        }
        require_non_empty("command", &self.command)?;
        reject_secret_like("command", &self.command)?;
        validate_template_placeholders("command", &self.command)?;
        if let Some(review_command) = &self.review_command {
            require_non_empty("review_command", review_command)?;
            reject_secret_like("review_command", review_command)?;
            validate_template_placeholders("review_command", review_command)?;
        }
        require_non_empty("workdir", &self.workdir)?;
        reject_secret_like("workdir", &self.workdir)?;
        validate_template_placeholders("workdir", &self.workdir)?;
        require_string_list("required_commands", &self.required_commands)?;
        reject_secret_like_list("required_commands", &self.required_commands)?;
        require_string_list("required_env", &self.required_env)?;
        reject_manager_env_list("required_env", &self.required_env)?;
        require_string_list("capabilities", &self.capabilities)?;
        if self.resource_weight == 0 {
            return Err(HepaAdapterSpecError::new(
                "resource_weight",
                "must be greater than zero",
            ));
        }
        if self.max_concurrency == 0 {
            return Err(HepaAdapterSpecError::new(
                "max_concurrency",
                "must be greater than zero",
            ));
        }
        if self.output_capture == HepaAdapterOutputCapture::Stdout
            && !template_uses_role_output(&self.command, self.review_command.as_deref())
        {
            // Stdout capture intentionally does not require an output placeholder.
        }
        Ok(())
    }

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaAdapterSpecError {
    pub field: String,
    pub message: String,
}

impl HepaAdapterSpecError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaAdapterSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaAdapterSpecError {}

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

fn validate_template_placeholders(field: &str, template: &str) -> Result<(), HepaAdapterSpecError> {
    let context = HepaAdapterTemplateContext {
        prompt_file: "<PROMPT_FILE>".to_string(),
        worktree: "<WORKTREE>".to_string(),
        review_prompt_file: "<REVIEW_PROMPT_FILE>".to_string(),
        output_file: "<OUTPUT_FILE>".to_string(),
        review_output_file: "<REVIEW_OUTPUT_FILE>".to_string(),
        artifact_dir: "<ARTIFACT_DIR>".to_string(),
    };
    render_command_template(template, &context)
        .map(|_| ())
        .map_err(|error| HepaAdapterSpecError::new(field, error.to_string()))
}

fn require_schema(schema_version: u32) -> Result<(), HepaAdapterSpecError> {
    if schema_version == ADAPTER_SPEC_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(HepaAdapterSpecError::new(
            "schema_version",
            format!("must be {ADAPTER_SPEC_SCHEMA_VERSION}"),
        ))
    }
}

fn require_non_empty(field: impl Into<String>, value: &str) -> Result<(), HepaAdapterSpecError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaAdapterSpecError::new(field, "must not be empty"));
    }
    Ok(())
}

fn require_single_line(field: impl Into<String>, value: &str) -> Result<(), HepaAdapterSpecError> {
    let field = field.into();
    require_non_empty(field.clone(), value)?;
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaAdapterSpecError::new(field, "must be a single line"));
    }
    Ok(())
}

fn require_string_list(field: &str, values: &[String]) -> Result<(), HepaAdapterSpecError> {
    for (index, value) in values.iter().enumerate() {
        require_single_line(format!("{field}[{index}]"), value)?;
    }
    Ok(())
}

fn reject_secret_like_list(field: &str, values: &[String]) -> Result<(), HepaAdapterSpecError> {
    for (index, value) in values.iter().enumerate() {
        reject_secret_like(format!("{field}[{index}]"), value)?;
    }
    Ok(())
}

fn reject_manager_env_list(field: &str, values: &[String]) -> Result<(), HepaAdapterSpecError> {
    for (index, value) in values.iter().enumerate() {
        if value == "GITHUB_TOKEN"
            || value.starts_with("HEPA_MANAGER_")
            || value.starts_with("MANAGER_")
        {
            return Err(HepaAdapterSpecError::new(
                format!("{field}[{index}]"),
                "must not contain manager-only credentials",
            ));
        }
    }
    Ok(())
}

fn reject_secret_like(field: impl Into<String>, value: &str) -> Result<(), HepaAdapterSpecError> {
    let lowered = value.to_ascii_lowercase();
    let github_token_prefix = ["ghp", "_"].concat();
    let secret_like = lowered.contains(&github_token_prefix)
        || [
            ".env",
            "api_key",
            "apikey",
            "credential",
            "id_rsa",
            "password",
            "private_key",
            "secret",
            "token",
        ]
        .iter()
        .any(|needle| lowered.contains(needle));
    if secret_like {
        Err(HepaAdapterSpecError::new(
            field,
            "must not contain secret-like values",
        ))
    } else {
        Ok(())
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HepaAdapterPromptTransport {
    #[default]
    PromptFile,
    Stdin,
    PromptArg,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HepaAdapterOutputCapture {
    #[default]
    AdapterFile,
    Stdout,
}

fn template_uses_role_output(command: &str, review_command: Option<&str>) -> bool {
    command.contains("{output_file}")
        || review_command.is_some_and(|value| value.contains("{review_output_file}"))
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
            prompt_transport: HepaAdapterPromptTransport::PromptFile,
            output_capture: HepaAdapterOutputCapture::AdapterFile,
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
            prompt_transport: HepaAdapterPromptTransport::PromptFile,
            output_capture: HepaAdapterOutputCapture::AdapterFile,
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

    #[test]
    fn invalid_adapter_specs_fail_with_clear_errors() {
        let mut spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "worker-primary".to_string(),
            display_name: "Primary Worker Adapter".to_string(),
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: "agent --prompt-file {prompt_file}".to_string(),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: vec!["agent".to_string()],
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["docs".to_string()],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 2,
            prompt_transport: HepaAdapterPromptTransport::PromptFile,
            output_capture: HepaAdapterOutputCapture::AdapterFile,
        };
        spec.max_concurrency = 0;

        let error = spec.validate().expect_err("zero concurrency must fail");

        assert_eq!(error.field, "max_concurrency");
        assert!(error.message.contains("greater than zero"));
    }

    #[test]
    fn invalid_adapter_templates_fail_validation() {
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "worker-primary".to_string(),
            display_name: "Primary Worker Adapter".to_string(),
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: "agent --prompt-file {missing_placeholder}".to_string(),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: vec!["agent".to_string()],
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["docs".to_string()],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 2,
            prompt_transport: HepaAdapterPromptTransport::PromptFile,
            output_capture: HepaAdapterOutputCapture::AdapterFile,
        };

        let error = spec.validate().expect_err("unknown placeholders must fail");

        assert_eq!(error.field, "command");
        assert!(error.message.contains("missing_placeholder"));
    }

    #[test]
    fn manager_env_adapter_spec_values_are_rejected() {
        let secret_env_name = ["GITHUB", "TOKEN"].join("_");
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "worker-primary".to_string(),
            display_name: "Primary Worker Adapter".to_string(),
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: "agent --prompt-file {prompt_file}".to_string(),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: vec!["agent".to_string()],
            required_env: vec![secret_env_name],
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["docs".to_string()],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 2,
            prompt_transport: HepaAdapterPromptTransport::PromptFile,
            output_capture: HepaAdapterOutputCapture::AdapterFile,
        };

        let error = spec.validate().expect_err("manager env names must fail");

        assert_eq!(error.field, "required_env[0]");
        assert!(error.message.contains("manager-only"));
    }

    #[test]
    fn provider_key_env_names_are_allowed_for_worker_adapters() {
        let spec = HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "pi".to_string(),
            display_name: "Pi".to_string(),
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: "pi -p --mode json".to_string(),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: vec!["pi".to_string()],
            required_env: vec!["DEEPSEEK_API_KEY".to_string()],
            sandbox: HepaAdapterSandbox::None,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["docs".to_string()],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 2,
            prompt_transport: HepaAdapterPromptTransport::Stdin,
            output_capture: HepaAdapterOutputCapture::Stdout,
        };

        spec.validate()
            .expect("provider key names are adapter env allowlist entries");
    }
}
