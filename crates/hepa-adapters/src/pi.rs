use crate::{
    builtin::builtin_adapter_spec,
    spec::HepaAdapterCostClass,
    spec::HepaAdapterSpec,
    version_pinning::{HepaAdapterOutputClassification, HepaAdapterVersionPin},
};
use hepa_core::config::HepaPiConfig;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt};

pub const PI_ADAPTER_ID: &str = "pi";
pub const PI_COMMAND: &str = "pi";
pub const PI_PINNED_VERSION: &str = "0.79.6";
pub const PI_INSTALL_PACKAGE: &str = "@earendil-works/pi-coding-agent";
pub const PI_PINNED_PACKAGE: &str = "@earendil-works/pi-coding-agent@0.79.6";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaPiModelConfig {
    pub provider: String,
    pub model: String,
    pub review_model: Option<String>,
    pub provider_key_env: Option<String>,
    pub base_url: Option<String>,
}

impl HepaPiModelConfig {
    pub fn required_env(&self) -> Vec<String> {
        let mut env = self
            .provider_key_env
            .as_ref()
            .filter(|value| !value.trim().is_empty())
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if self
            .base_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            env.push("HEPA_PI_BASE_URL".to_string());
        }
        env.sort();
        env.dedup();
        env
    }

    pub fn cost_class(&self) -> HepaAdapterCostClass {
        let worker_model = format!("{}/{}", self.provider, self.model);
        let worker_is_local = is_local_model(&worker_model)
            || (self.base_url.as_deref().is_some_and(is_loopback_url)
                && self.provider_key_env.is_none());
        let reviewer_is_local = self
            .review_model
            .as_deref()
            .map(is_local_model)
            .unwrap_or(worker_is_local);
        if self
            .provider_key_env
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && (!worker_is_local || !reviewer_is_local)
        {
            HepaAdapterCostClass::PaidCloud
        } else {
            HepaAdapterCostClass::Local
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HepaPiLocalRouteStatus {
    Cloud,
    ToolCallingReady,
    ToolCallingUnverified,
    ToolCallingUnsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaPiLocalRouteDiagnostic {
    pub status: HepaPiLocalRouteStatus,
    pub detail: String,
    pub action: String,
}

impl HepaPiLocalRouteDiagnostic {
    pub fn is_blocking(&self) -> bool {
        matches!(
            self.status,
            HepaPiLocalRouteStatus::ToolCallingUnsupported
                | HepaPiLocalRouteStatus::ToolCallingUnverified
        )
    }
}

pub fn env_key_for_model(model: &str) -> Option<&'static str> {
    let provider = model.split('/').next().unwrap_or_default();
    match provider {
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "google" | "gemini" => Some("GOOGLE_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "ollama" | "vllm" | "llama-cpp" | "llamacpp" | "local" | "mlx" | "mlx-lm" => None,
        _ => None,
    }
}

pub fn model_config_from_env(
    environment: &std::collections::BTreeMap<String, String>,
) -> HepaPiModelConfig {
    let model = environment
        .get("HEPA_PI_MODEL")
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_default();
    let provider = model
        .split('/')
        .next()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_default()
        .to_string();
    let model = model
        .split_once('/')
        .map(|(_, value)| value.to_string())
        .unwrap_or(model);
    let review_model = environment.get("HEPA_PI_REVIEW_MODEL").cloned();
    let provider_key_env = environment
        .get("HEPA_PI_PROVIDER_KEY_ENV")
        .and_then(|value| (!value.trim().is_empty()).then(|| value.clone()))
        .or_else(|| env_key_for_model(&format!("{provider}/{model}")).map(str::to_string))
        .or_else(|| {
            review_model
                .as_deref()
                .and_then(env_key_for_model)
                .map(str::to_string)
        });
    let base_url = environment.get("HEPA_PI_BASE_URL").cloned();
    HepaPiModelConfig {
        provider,
        model,
        review_model,
        provider_key_env,
        base_url,
    }
}

pub fn pi_local_route_diagnostic(
    environment: &BTreeMap<String, String>,
) -> HepaPiLocalRouteDiagnostic {
    let config = model_config_from_env(environment);
    local_route_diagnostic_from_config(&config)
}

pub fn local_route_diagnostic_from_config(
    config: &HepaPiModelConfig,
) -> HepaPiLocalRouteDiagnostic {
    if config.provider.trim().is_empty() || config.model.trim().is_empty() {
        return HepaPiLocalRouteDiagnostic {
            status: HepaPiLocalRouteStatus::Cloud,
            detail: "pi_model_not_configured".to_string(),
            action: "Set HEPA_PI_MODEL to a tool-call-capable cloud or local model route before running Pi.".to_string(),
        };
    }

    let worker_model = format!("{}/{}", config.provider, config.model);
    let worker_is_local =
        is_local_model(&worker_model) || config.base_url.as_deref().is_some_and(is_loopback_url);
    let reviewer_is_local = config
        .review_model
        .as_deref()
        .map(|model| {
            is_local_model(model) || config.base_url.as_deref().is_some_and(is_loopback_url)
        })
        .unwrap_or(worker_is_local);
    if !worker_is_local && !reviewer_is_local {
        return HepaPiLocalRouteDiagnostic {
            status: HepaPiLocalRouteStatus::Cloud,
            detail: "cloud_route".to_string(),
            action: "No local model action required.".to_string(),
        };
    }

    let provider = config.provider.to_ascii_lowercase();
    let model = config.model.to_ascii_lowercase();
    let base_url = config
        .base_url
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let route = if config.base_url.as_deref().is_some_and(is_loopback_url) {
        format!(
            "{provider}/{} via {}",
            config.model,
            config.base_url.as_deref().unwrap_or("")
        )
    } else {
        format!("{provider}/{}", config.model)
    };

    if provider == "local" && (model.contains("mlx-community") || base_url.contains(":52415")) {
        return HepaPiLocalRouteDiagnostic {
            status: HepaPiLocalRouteStatus::ToolCallingUnsupported,
            detail: format!(
                "local_tool_calling_unsupported:{route}: generic exo/Apple MLX endpoints have not proven the OpenAI tools/tool_choice/tool_calls contract Pi needs for HEPA edits"
            ),
            action: "Serve the local coding model through a tool-call-capable OpenAI-compatible server such as llama.cpp with --jinja, then set HEPA_PI_MODEL=llama-cpp/<model-id> and HEPA_PI_BASE_URL=http://127.0.0.1:8080/v1; keep exo/MLX routes out of release runs until they pass that tool-call contract."
                .to_string(),
        };
    }

    if matches!(
        provider.as_str(),
        "llama-cpp" | "llamacpp" | "ollama" | "vllm"
    ) {
        return HepaPiLocalRouteDiagnostic {
            status: HepaPiLocalRouteStatus::ToolCallingReady,
            detail: format!(
                "local_tool_calling_ready:{route}: route class supports OpenAI-compatible tool calling when the served model/template is tool-capable"
            ),
            action: "Confirm the server is running before the stress run; for llama.cpp use llama-server with chat-template/tool-call support such as --jinja."
                .to_string(),
        };
    }

    HepaPiLocalRouteDiagnostic {
        status: HepaPiLocalRouteStatus::ToolCallingUnverified,
        detail: format!(
            "local_tool_calling_unverified:{route}: HEPA cannot prove this local provider exposes the tool-call semantics Pi needs"
        ),
        action: "Use a known tool-call-capable local route for release validation, preferably llama.cpp with --jinja, or replace this provider with a wrapper that implements OpenAI tools/tool_choice/tool_calls reliably."
            .to_string(),
    }
}

pub fn adapter_spec_from_config(config: &HepaPiConfig) -> HepaAdapterSpec {
    if config.model.trim().is_empty() {
        return builtin_adapter_spec(PI_ADAPTER_ID);
    }

    let (provider, model) = split_provider_model(&config.model);
    let (review_provider, review_model) = config
        .review_model
        .as_deref()
        .map(split_provider_model)
        .unwrap_or_else(|| (provider.clone(), model.clone()));
    let review_model_full = format!("{review_provider}/{review_model}");
    let model_config = HepaPiModelConfig {
        provider,
        model,
        review_model: Some(review_model_full.clone()).filter(|value| !value.trim().is_empty()),
        provider_key_env: config
            .provider_key_env
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| env_key_for_model(&config.model).map(str::to_string))
            .or_else(|| {
                config
                    .review_model
                    .as_deref()
                    .and_then(env_key_for_model)
                    .map(str::to_string)
            }),
        base_url: config.base_url.clone(),
    };
    let mut spec = builtin_adapter_spec(PI_ADAPTER_ID);
    spec.command = format!(
        "pi --no-approve --no-session --no-extensions --no-skills --no-prompt-templates --no-context-files --tools read,edit,write,bash,grep,find,ls -p --mode json --provider {} --model {}",
        model_config.provider, model_config.model
    );
    spec.review_command = Some(format!(
        "pi --no-approve --no-session --no-extensions --no-skills --no-prompt-templates --no-context-files --tools read,edit,write,bash,grep,find,ls -p --mode json --provider {} --model {}",
        review_provider, review_model
    ));
    spec.required_env = model_config.required_env();
    spec.cost_class = model_config.cost_class();
    spec.prompt_transport = crate::spec::HepaAdapterPromptTransport::Stdin;
    spec
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaPiParsedOutput {
    pub final_message: String,
    pub tool_activity: Vec<String>,
}

pub fn parse_pi_json_events(raw: &str) -> Result<HepaPiParsedOutput, HepaPiParseError> {
    let mut final_message = None;
    let mut tool_activity = Vec::new();
    let mut saw_agent_end = false;

    for (line_index, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) if saw_agent_end && error.is_eof() => break,
            Err(error) => {
                return Err(HepaPiParseError::new(format!(
                    "line {} is not valid JSON: {error}",
                    line_index + 1
                )));
            }
        };
        let event_type = value
            .get("type")
            .or_else(|| value.get("event"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                HepaPiParseError::new(format!("line {} missing event type", line_index + 1))
            })?;
        if event_type.contains("tool") {
            tool_activity.push(event_type.to_string());
        }
        if event_type == "agent_end" {
            saw_agent_end = true;
            final_message = extract_final_message(&value);
        }
    }

    if !saw_agent_end {
        return Err(HepaPiParseError::new("missing agent_end event"));
    }
    let final_message = final_message
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| HepaPiParseError::new("agent_end missing final assistant message"))?;
    Ok(HepaPiParsedOutput {
        final_message,
        tool_activity,
    })
}

pub fn classify_pi_output(raw: &str) -> HepaAdapterOutputClassification {
    match parse_pi_json_events(raw) {
        Ok(_) => HepaAdapterOutputClassification::Parsed,
        Err(error) => HepaAdapterOutputClassification::ParseFailed {
            reason: error.to_string(),
        },
    }
}

pub fn pi_version_pin(command_template: String) -> HepaAdapterVersionPin {
    HepaAdapterVersionPin {
        adapter_id: PI_ADAPTER_ID.to_string(),
        version: PI_PINNED_VERSION.to_string(),
        command_template,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaPiInstallPlan {
    pub adapter_id: String,
    pub package: String,
    pub command: Vec<String>,
}

impl HepaPiInstallPlan {
    pub fn npm_global() -> Self {
        Self {
            adapter_id: PI_ADAPTER_ID.to_string(),
            package: PI_PINNED_PACKAGE.to_string(),
            command: vec![
                "npm".to_string(),
                "install".to_string(),
                "-g".to_string(),
                PI_PINNED_PACKAGE.to_string(),
            ],
        }
    }

    pub fn action_line(&self) -> String {
        format!(
            "HEPA will install {} explicitly with: {}",
            self.package,
            self.command.join(" ")
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaPiParseError {
    pub message: String,
}

impl HepaPiParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaPiParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Pi output parse failure: {}", self.message)
    }
}

impl Error for HepaPiParseError {}

fn extract_final_message(value: &serde_json::Value) -> Option<String> {
    value
        .get("message")
        .and_then(extract_message_text)
        .or_else(|| value.get("final_message").and_then(extract_message_text))
        .or_else(|| {
            value
                .get("history")
                .and_then(serde_json::Value::as_array)
                .and_then(|history| {
                    history
                        .iter()
                        .rev()
                        .find_map(extract_assistant_message_text)
                })
        })
        .or_else(|| {
            value
                .get("messages")
                .and_then(serde_json::Value::as_array)
                .and_then(|messages| {
                    messages
                        .iter()
                        .rev()
                        .find_map(extract_assistant_message_text)
                })
        })
}

fn extract_assistant_message_text(value: &serde_json::Value) -> Option<String> {
    let role = value.get("role").and_then(serde_json::Value::as_str);
    if role != Some("assistant") {
        return None;
    }
    extract_message_text(value).filter(|text| !text.trim().is_empty())
}

fn extract_message_text(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    value
        .get("content")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value
                .get("text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            value
                .get("content")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| {
                    let text = items
                        .iter()
                        .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n");
                    (!text.trim().is_empty()).then_some(text)
                })
        })
}

fn is_local_model(model: &str) -> bool {
    matches!(
        model.split('/').next().unwrap_or_default(),
        "ollama" | "vllm" | "local" | "llama-cpp" | "llamacpp" | "mlx" | "mlx-lm"
    )
}

fn is_loopback_url(url: &str) -> bool {
    url.contains("://127.0.0.1")
        || url.contains("://localhost")
        || url.contains("://[::1]")
        || url.starts_with("http://0.0.0.0")
}

fn split_provider_model(value: &str) -> (String, String) {
    match value.split_once('/') {
        Some((provider, model)) if !provider.trim().is_empty() && !model.trim().is_empty() => {
            (provider.to_string(), model.to_string())
        }
        _ => ("provider".to_string(), value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn pi_event_stream_extracts_final_message_and_tools() {
        let raw = r#"{"type":"session","cwd":"/tmp/lane"}
{"type":"agent_start"}
{"type":"tool_call","name":"edit"}
{"type":"agent_end","history":[{"role":"assistant","content":"done from pi"}]}"#;

        let parsed = parse_pi_json_events(raw).expect("valid Pi event stream");

        assert_eq!(parsed.final_message, "done from pi");
        assert_eq!(parsed.tool_activity, vec!["tool_call"]);
        assert_eq!(
            classify_pi_output(raw),
            HepaAdapterOutputClassification::Parsed
        );
    }

    #[test]
    fn pi_event_stream_extracts_current_messages_shape() {
        let raw = r#"{"type":"agent_start"}
{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"hidden"},{"type":"text","text":"{\"status\":\"approved\",\"summary\":[\"ok\"],\"findings\":[]}"}]}]}"#;

        let parsed = parse_pi_json_events(raw).expect("current Pi messages shape should parse");

        assert_eq!(
            parsed.final_message,
            "{\"status\":\"approved\",\"summary\":[\"ok\"],\"findings\":[]}"
        );
    }

    #[test]
    fn pi_event_stream_ignores_truncated_tail_after_agent_end() {
        let raw = r#"{"type":"agent_start"}
{"type":"tool_call","name":"edit"}
{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"done"}]}]}
{"type":"debug","message":"unterminated"#;

        let parsed = parse_pi_json_events(raw).expect("completed Pi stream should parse");

        assert_eq!(parsed.final_message, "done");
        assert_eq!(parsed.tool_activity, vec!["tool_call"]);
    }

    #[test]
    fn pi_event_stream_does_not_treat_user_prompt_as_final_message() {
        let raw = r#"{"type":"agent_start"}
{"type":"agent_end","messages":[{"role":"user","content":[{"type":"text","text":"original prompt"}]},{"role":"assistant","content":[]}]}"#;

        let error = parse_pi_json_events(raw).expect_err("empty assistant output should fail");

        assert!(
            error
                .to_string()
                .contains("agent_end missing final assistant message")
        );
    }

    #[test]
    fn pi_event_stream_classifies_corruption_as_parse_failure() {
        let classified = classify_pi_output("{\"type\":\"agent_start\"}\nnot-json");

        match classified {
            HepaAdapterOutputClassification::ParseFailed { reason } => {
                assert!(reason.contains("not valid JSON"));
            }
            HepaAdapterOutputClassification::Parsed => panic!("corrupt output must not parse"),
        }
    }

    #[test]
    fn model_config_derives_provider_key_and_cost_class() {
        let cloud = model_config_from_env(&BTreeMap::from([(
            "HEPA_PI_MODEL".to_string(),
            "openai/gpt-4.1".to_string(),
        )]));
        assert_eq!(cloud.provider_key_env.as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(cloud.cost_class(), HepaAdapterCostClass::PaidCloud);

        let missing = model_config_from_env(&BTreeMap::new());
        assert_eq!(missing.provider, "");
        assert_eq!(missing.model, "");
        assert_eq!(missing.provider_key_env, None);

        let ollama = model_config_from_env(&BTreeMap::from([(
            "HEPA_PI_MODEL".to_string(),
            "ollama/tool-coder".to_string(),
        )]));
        assert_eq!(ollama.provider_key_env, None);
        assert_eq!(ollama.cost_class(), HepaAdapterCostClass::Local);
    }

    #[test]
    fn pi_local_route_blocks_exo_mlx_without_tool_call_contract() {
        let diagnostic = pi_local_route_diagnostic(&BTreeMap::from([
            (
                "HEPA_PI_MODEL".to_string(),
                "local/mlx-community/reasoning-coder-30b".to_string(),
            ),
            (
                "HEPA_PI_BASE_URL".to_string(),
                "http://127.0.0.1:52415/v1".to_string(),
            ),
        ]));

        assert_eq!(
            diagnostic.status,
            HepaPiLocalRouteStatus::ToolCallingUnsupported
        );
        assert!(diagnostic.is_blocking());
        assert!(diagnostic.action.contains("llama.cpp"));
        assert!(diagnostic.action.contains("--jinja"));
    }

    #[test]
    fn pi_local_route_accepts_llama_cpp_as_tool_call_ready() {
        let diagnostic = pi_local_route_diagnostic(&BTreeMap::from([
            (
                "HEPA_PI_MODEL".to_string(),
                "llama-cpp/reasoning-coder-20b".to_string(),
            ),
            (
                "HEPA_PI_BASE_URL".to_string(),
                "http://127.0.0.1:8080/v1".to_string(),
            ),
        ]));

        assert_eq!(diagnostic.status, HepaPiLocalRouteStatus::ToolCallingReady);
        assert!(!diagnostic.is_blocking());
    }

    #[test]
    fn pi_model_config_marks_llama_cpp_as_local_without_key() {
        let llama = model_config_from_env(&BTreeMap::from([
            (
                "HEPA_PI_MODEL".to_string(),
                "llama-cpp/reasoning-coder-20b".to_string(),
            ),
            ("HEPA_PI_PROVIDER_KEY_ENV".to_string(), "".to_string()),
            (
                "HEPA_PI_BASE_URL".to_string(),
                "http://127.0.0.1:8080/v1".to_string(),
            ),
        ]));

        assert_eq!(llama.provider_key_env, None);
        assert_eq!(llama.cost_class(), HepaAdapterCostClass::Local);
    }

    #[test]
    fn pi_adapter_spec_follows_model_config() {
        let spec = adapter_spec_from_config(&HepaPiConfig {
            model: "ollama/tool-coder".to_string(),
            review_model: Some("ollama/tool-coder-review".to_string()),
            provider_key_env: None,
            base_url: Some("http://127.0.0.1:11434/v1".to_string()),
        });

        assert!(spec.command.contains("--provider ollama"));
        assert!(spec.command.contains("--model tool-coder"));
        assert!(spec.command.contains("--no-approve"));
        assert!(!spec.command.contains("--approve "));
        assert_eq!(
            spec.prompt_transport,
            crate::spec::HepaAdapterPromptTransport::Stdin
        );
        assert!(
            spec.review_command
                .as_deref()
                .unwrap()
                .contains("--provider ollama")
        );
        assert!(
            spec.review_command
                .as_deref()
                .unwrap()
                .contains("--no-approve")
        );
        assert_eq!(spec.required_env, vec!["HEPA_PI_BASE_URL".to_string()]);
        assert_eq!(spec.cost_class, HepaAdapterCostClass::Local);
    }

    #[test]
    fn pi_adapter_spec_allows_local_worker_with_cloud_reviewer() {
        let spec = adapter_spec_from_config(&HepaPiConfig {
            model: "local/mlx-community/reasoning-coder-30b".to_string(),
            review_model: Some("openai/gpt-4.1".to_string()),
            provider_key_env: None,
            base_url: Some("http://127.0.0.1:52415/v1".to_string()),
        });

        assert!(spec.command.contains("--provider local"));
        assert!(
            spec.command
                .contains("--model mlx-community/reasoning-coder-30b")
        );
        assert!(
            spec.review_command
                .as_deref()
                .unwrap()
                .contains("--provider openai")
        );
        assert!(
            spec.review_command
                .as_deref()
                .unwrap()
                .contains("--model gpt-4.1")
        );
        assert_eq!(
            spec.required_env,
            vec!["HEPA_PI_BASE_URL".to_string(), "OPENAI_API_KEY".to_string()]
        );
        assert_eq!(spec.cost_class, HepaAdapterCostClass::PaidCloud);
    }

    #[test]
    fn install_plan_is_explicit_and_pinned() {
        let plan = HepaPiInstallPlan::npm_global();

        assert!(plan.command.contains(&"-g".to_string()));
        assert_eq!(plan.package, PI_PINNED_PACKAGE);
        assert!(plan.package.ends_with(PI_PINNED_VERSION));
        assert!(!plan.command.iter().any(|part| part == "sudo"));
        assert!(plan.action_line().contains("explicitly"));
    }
}
