use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, env, error::Error, fmt, fs, path::Path};

pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaConfig {
    pub schema_version: u32,
    pub control_root: String,
    pub worktree_root: String,
    pub archive_root: String,
    pub max_total_rounds: u32,
    pub max_repair_rounds: u32,
    pub notification: HepaNotificationConfig,
    pub default_adapter: String,
    pub routing_file: String,
    pub pi: HepaPiConfig,
    pub hermes: HepaHermesBridgeConfig,
}

impl Default for HepaConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            control_root: ".hepa".to_string(),
            worktree_root: ".hepa/worktrees".to_string(),
            archive_root: ".hepa/archive".to_string(),
            max_total_rounds: 3,
            max_repair_rounds: 2,
            notification: HepaNotificationConfig::default(),
            default_adapter: "fake".to_string(),
            routing_file: ".hepa/routing.yaml".to_string(),
            pi: HepaPiConfig::default(),
            hermes: HepaHermesBridgeConfig::default(),
        }
    }
}

impl HepaConfig {
    pub fn load(
        dotenv_text: Option<&str>,
        environment: &BTreeMap<String, String>,
        overrides: HepaConfigOverrides,
    ) -> Result<Self, HepaConfigError> {
        let mut config = Self::default();
        if let Some(dotenv_text) = dotenv_text {
            apply_values(&mut config, &parse_dotenv(dotenv_text)?)?;
        }
        apply_values(&mut config, environment)?;
        apply_overrides(&mut config, overrides);
        config.validate()?;
        Ok(config)
    }

    pub fn load_from_env_and_dotenv_file(
        dotenv_path: impl AsRef<Path>,
        overrides: HepaConfigOverrides,
    ) -> Result<Self, HepaConfigError> {
        let dotenv_text = match fs::read_to_string(dotenv_path.as_ref()) {
            Ok(value) => Some(value),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(HepaConfigError::new(
                    ".env",
                    format!("failed to read dotenv file: {error}"),
                ));
            }
        };
        let environment = env::vars().collect();
        Self::load(dotenv_text.as_deref(), &environment, overrides)
    }

    pub fn validate(&self) -> Result<(), HepaConfigError> {
        require_schema(self.schema_version)?;
        require_single_line("control_root", &self.control_root)?;
        require_single_line("worktree_root", &self.worktree_root)?;
        require_single_line("archive_root", &self.archive_root)?;
        require_positive("max_total_rounds", self.max_total_rounds)?;
        require_positive("max_repair_rounds", self.max_repair_rounds)?;
        self.notification.validate()?;
        require_single_line("default_adapter", &self.default_adapter)?;
        require_single_line("routing_file", &self.routing_file)?;
        self.pi.validate()?;
        self.hermes.validate()?;
        Ok(())
    }

    pub fn redacted_diagnostics(&self) -> HepaConfigDiagnostics {
        HepaConfigDiagnostics {
            schema_version: self.schema_version,
            control_root: "<CONTROL_ROOT>".to_string(),
            worktree_root: "<WORKTREE_ROOT>".to_string(),
            archive_root: "<ARCHIVE_ROOT>".to_string(),
            max_total_rounds: self.max_total_rounds,
            max_repair_rounds: self.max_repair_rounds,
            notification: HepaNotificationDiagnostics {
                terminal_enabled: self.notification.terminal_enabled,
                command_configured: self.notification.command.is_some(),
            },
            default_adapter: self.default_adapter.clone(),
            routing_file: "<ROUTING_FILE>".to_string(),
            pi: HepaPiDiagnostics {
                model: self.pi.model.clone(),
                review_model_configured: self.pi.review_model.is_some(),
                provider_key_env_configured: self.pi.provider_key_env.is_some(),
                base_url_configured: self.pi.base_url.is_some(),
            },
            hermes: HepaHermesBridgeDiagnostics {
                enabled: self.hermes.enabled,
                endpoint_configured: self.hermes.endpoint.is_some(),
                board_id_configured: self.hermes.board_id.is_some(),
                sync_interval_seconds: self.hermes.sync_interval_seconds,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaPiConfig {
    pub model: String,
    pub review_model: Option<String>,
    pub provider_key_env: Option<String>,
    pub base_url: Option<String>,
}

impl Default for HepaPiConfig {
    fn default() -> Self {
        Self {
            model: "deepseek/deepseek-chat".to_string(),
            review_model: None,
            provider_key_env: Some("DEEPSEEK_API_KEY".to_string()),
            base_url: None,
        }
    }
}

impl HepaPiConfig {
    fn validate(&self) -> Result<(), HepaConfigError> {
        require_single_line("pi.model", &self.model)?;
        if let Some(review_model) = &self.review_model {
            require_single_line("pi.review_model", review_model)?;
        }
        if let Some(provider_key_env) = &self.provider_key_env {
            require_single_line("pi.provider_key_env", provider_key_env)?;
        }
        if let Some(base_url) = &self.base_url {
            require_single_line("pi.base_url", base_url)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaNotificationConfig {
    pub terminal_enabled: bool,
    pub command: Option<String>,
}

impl Default for HepaNotificationConfig {
    fn default() -> Self {
        Self {
            terminal_enabled: true,
            command: None,
        }
    }
}

impl HepaNotificationConfig {
    fn validate(&self) -> Result<(), HepaConfigError> {
        if let Some(command) = &self.command {
            require_single_line("notification.command", command)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaHermesBridgeConfig {
    pub enabled: bool,
    pub endpoint: Option<String>,
    pub board_id: Option<String>,
    pub sync_interval_seconds: u32,
}

impl Default for HepaHermesBridgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            endpoint: None,
            board_id: None,
            sync_interval_seconds: 30,
        }
    }
}

impl HepaHermesBridgeConfig {
    fn validate(&self) -> Result<(), HepaConfigError> {
        if let Some(endpoint) = &self.endpoint {
            require_single_line("hermes.endpoint", endpoint)?;
        }
        if let Some(board_id) = &self.board_id {
            require_single_line("hermes.board_id", board_id)?;
        }
        require_positive("hermes.sync_interval_seconds", self.sync_interval_seconds)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaConfigDiagnostics {
    pub schema_version: u32,
    pub control_root: String,
    pub worktree_root: String,
    pub archive_root: String,
    pub max_total_rounds: u32,
    pub max_repair_rounds: u32,
    pub notification: HepaNotificationDiagnostics,
    pub default_adapter: String,
    pub routing_file: String,
    pub pi: HepaPiDiagnostics,
    pub hermes: HepaHermesBridgeDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaPiDiagnostics {
    pub model: String,
    pub review_model_configured: bool,
    pub provider_key_env_configured: bool,
    pub base_url_configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaNotificationDiagnostics {
    pub terminal_enabled: bool,
    pub command_configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaHermesBridgeDiagnostics {
    pub enabled: bool,
    pub endpoint_configured: bool,
    pub board_id_configured: bool,
    pub sync_interval_seconds: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HepaConfigOverrides {
    pub control_root: Option<String>,
    pub worktree_root: Option<String>,
    pub archive_root: Option<String>,
    pub max_total_rounds: Option<u32>,
    pub max_repair_rounds: Option<u32>,
    pub terminal_notifications: Option<bool>,
    pub notification_command: Option<Option<String>>,
    pub default_adapter: Option<String>,
    pub routing_file: Option<String>,
    pub pi_model: Option<String>,
    pub pi_review_model: Option<Option<String>>,
    pub pi_provider_key_env: Option<Option<String>>,
    pub pi_base_url: Option<Option<String>>,
    pub hermes_enabled: Option<bool>,
    pub hermes_endpoint: Option<Option<String>>,
    pub hermes_board_id: Option<Option<String>>,
    pub hermes_sync_interval_seconds: Option<u32>,
}

impl HepaConfigOverrides {
    pub fn isolated_temp_root(temp_root: impl Into<String>) -> Self {
        let temp_root = temp_root.into();
        Self {
            control_root: Some(join_config_path(&temp_root, "control")),
            worktree_root: Some(join_config_path(&temp_root, "worktrees")),
            archive_root: Some(join_config_path(&temp_root, "archive")),
            routing_file: Some(join_config_path(&temp_root, "routing.yaml")),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaConfigError {
    pub field: String,
    pub message: String,
}

impl HepaConfigError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaConfigError {}

fn apply_values(
    config: &mut HepaConfig,
    values: &BTreeMap<String, String>,
) -> Result<(), HepaConfigError> {
    for (key, value) in values {
        match key.as_str() {
            "HEPA_CONTROL_ROOT" => config.control_root = value.clone(),
            "HEPA_WORKTREE_ROOT" => config.worktree_root = value.clone(),
            "HEPA_ARCHIVE_ROOT" => config.archive_root = value.clone(),
            "HEPA_MAX_TOTAL_ROUNDS" => {
                config.max_total_rounds = parse_u32(key, value)?;
            }
            "HEPA_MAX_REPAIR_ROUNDS" => {
                config.max_repair_rounds = parse_u32(key, value)?;
            }
            "HEPA_NOTIFY_TERMINAL" => {
                config.notification.terminal_enabled = parse_bool(key, value)?;
            }
            "HEPA_NOTIFY_COMMAND" => {
                config.notification.command = parse_optional(value);
            }
            "HEPA_DEFAULT_ADAPTER" => config.default_adapter = value.clone(),
            "HEPA_ROUTING_FILE" => config.routing_file = value.clone(),
            "HEPA_PI_MODEL" => config.pi.model = value.clone(),
            "HEPA_PI_REVIEW_MODEL" => config.pi.review_model = parse_optional(value),
            "HEPA_PI_PROVIDER_KEY_ENV" => config.pi.provider_key_env = parse_optional(value),
            "HEPA_PI_BASE_URL" => config.pi.base_url = parse_optional(value),
            "HEPA_HERMES_ENABLED" => {
                config.hermes.enabled = parse_bool(key, value)?;
            }
            "HEPA_HERMES_ENDPOINT" => {
                config.hermes.endpoint = parse_optional(value);
            }
            "HEPA_HERMES_BOARD_ID" => {
                config.hermes.board_id = parse_optional(value);
            }
            "HEPA_HERMES_SYNC_INTERVAL_SECONDS" => {
                config.hermes.sync_interval_seconds = parse_u32(key, value)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn apply_overrides(config: &mut HepaConfig, overrides: HepaConfigOverrides) {
    if let Some(value) = overrides.control_root {
        config.control_root = value;
    }
    if let Some(value) = overrides.worktree_root {
        config.worktree_root = value;
    }
    if let Some(value) = overrides.archive_root {
        config.archive_root = value;
    }
    if let Some(value) = overrides.max_total_rounds {
        config.max_total_rounds = value;
    }
    if let Some(value) = overrides.max_repair_rounds {
        config.max_repair_rounds = value;
    }
    if let Some(value) = overrides.terminal_notifications {
        config.notification.terminal_enabled = value;
    }
    if let Some(value) = overrides.notification_command {
        config.notification.command = value;
    }
    if let Some(value) = overrides.default_adapter {
        config.default_adapter = value;
    }
    if let Some(value) = overrides.routing_file {
        config.routing_file = value;
    }
    if let Some(value) = overrides.pi_model {
        config.pi.model = value;
    }
    if let Some(value) = overrides.pi_review_model {
        config.pi.review_model = value;
    }
    if let Some(value) = overrides.pi_provider_key_env {
        config.pi.provider_key_env = value;
    }
    if let Some(value) = overrides.pi_base_url {
        config.pi.base_url = value;
    }
    if let Some(value) = overrides.hermes_enabled {
        config.hermes.enabled = value;
    }
    if let Some(value) = overrides.hermes_endpoint {
        config.hermes.endpoint = value;
    }
    if let Some(value) = overrides.hermes_board_id {
        config.hermes.board_id = value;
    }
    if let Some(value) = overrides.hermes_sync_interval_seconds {
        config.hermes.sync_interval_seconds = value;
    }
}

fn parse_dotenv(dotenv_text: &str) -> Result<BTreeMap<String, String>, HepaConfigError> {
    let mut values = BTreeMap::new();
    for (index, raw_line) in dotenv_text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            return Err(HepaConfigError::new(
                format!(".env:{}", index + 1),
                "expected KEY=value",
            ));
        };
        let key = raw_key.trim();
        require_env_key(index + 1, key)?;
        values.insert(key.to_string(), unquote_dotenv_value(raw_value.trim()));
    }
    Ok(values)
}

fn unquote_dotenv_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0];
        let last = bytes[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn parse_optional(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn join_config_path(root: &str, child: &str) -> String {
    format!("{}/{}", root.trim_end_matches('/'), child)
}

fn parse_bool(field: &str, value: &str) -> Result<bool, HepaConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(HepaConfigError::new(field, "expected boolean value")),
    }
}

fn parse_u32(field: &str, value: &str) -> Result<u32, HepaConfigError> {
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| HepaConfigError::new(field, "expected positive integer"))
}

fn require_schema(schema_version: u32) -> Result<(), HepaConfigError> {
    if schema_version == CONFIG_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(HepaConfigError::new(
            "schema_version",
            format!("must be {CONFIG_SCHEMA_VERSION}"),
        ))
    }
}

fn require_positive(field: &str, value: u32) -> Result<(), HepaConfigError> {
    if value == 0 {
        Err(HepaConfigError::new(field, "must be greater than zero"))
    } else {
        Ok(())
    }
}

fn require_env_key(line: usize, key: &str) -> Result<(), HepaConfigError> {
    if key.is_empty()
        || !key.chars().all(|character| {
            character.is_ascii_uppercase() || character == '_' || character.is_ascii_digit()
        })
    {
        return Err(HepaConfigError::new(
            format!(".env:{line}"),
            "expected uppercase environment key",
        ));
    }
    Ok(())
}

fn require_single_line(field: impl Into<String>, value: &str) -> Result<(), HepaConfigError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaConfigError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaConfigError::new(field, "must be a single line"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_conservative() {
        let config = HepaConfig::default();

        assert_eq!(config.control_root, ".hepa");
        assert_eq!(config.worktree_root, ".hepa/worktrees");
        assert_eq!(config.archive_root, ".hepa/archive");
        assert_eq!(config.max_total_rounds, 3);
        assert_eq!(config.max_repair_rounds, 2);
        assert!(config.notification.terminal_enabled);
        assert_eq!(config.default_adapter, "fake");
        assert_eq!(config.routing_file, ".hepa/routing.yaml");
        assert_eq!(config.pi.model, "deepseek/deepseek-chat");
        assert_eq!(
            config.pi.provider_key_env.as_deref(),
            Some("DEEPSEEK_API_KEY")
        );
        assert!(config.hermes.enabled);
    }

    #[test]
    fn config_loads_dotenv_environment_and_explicit_overrides_in_order() {
        let dotenv = r#"
            HEPA_CONTROL_ROOT=.hepa-dotenv
            HEPA_MAX_TOTAL_ROUNDS=4
            HEPA_NOTIFY_TERMINAL=false
            HEPA_DEFAULT_ADAPTER=dotenv-worker
            HEPA_PI_MODEL=ollama/qwen2.5-coder
            HEPA_PI_PROVIDER_KEY_ENV=
            HEPA_HERMES_ENDPOINT="http://hermes.invalid"
        "#;
        let environment = BTreeMap::from([
            ("HEPA_CONTROL_ROOT".to_string(), ".hepa-env".to_string()),
            ("HEPA_MAX_REPAIR_ROUNDS".to_string(), "5".to_string()),
            ("HEPA_HERMES_BOARD_ID".to_string(), "board-1".to_string()),
        ]);
        let overrides = HepaConfigOverrides {
            control_root: Some(".hepa-cli".to_string()),
            default_adapter: Some("cli-worker".to_string()),
            hermes_enabled: Some(false),
            ..HepaConfigOverrides::default()
        };

        let config =
            HepaConfig::load(Some(dotenv), &environment, overrides).expect("config should load");

        assert_eq!(config.control_root, ".hepa-cli");
        assert_eq!(config.max_total_rounds, 4);
        assert_eq!(config.max_repair_rounds, 5);
        assert!(!config.notification.terminal_enabled);
        assert_eq!(config.default_adapter, "cli-worker");
        assert_eq!(config.pi.model, "ollama/qwen2.5-coder");
        assert_eq!(config.pi.provider_key_env, None);
        assert!(!config.hermes.enabled);
        assert_eq!(
            config.hermes.endpoint.as_deref(),
            Some("http://hermes.invalid")
        );
        assert_eq!(config.hermes.board_id.as_deref(), Some("board-1"));
    }

    #[test]
    fn invalid_config_values_fail_with_clear_fields() {
        let environment =
            BTreeMap::from([("HEPA_MAX_TOTAL_ROUNDS".to_string(), "none".to_string())]);

        let error = HepaConfig::load(None, &environment, HepaConfigOverrides::default())
            .expect_err("invalid numeric values must fail");

        assert_eq!(error.field, "HEPA_MAX_TOTAL_ROUNDS");
        assert!(error.message.contains("positive integer"));
    }

    #[test]
    fn invalid_dotenv_lines_fail_loudly() {
        let error = HepaConfig::load(
            Some("HEPA_CONTROL_ROOT\n"),
            &BTreeMap::new(),
            HepaConfigOverrides::default(),
        )
        .expect_err("dotenv lines must contain key and value");

        assert_eq!(error.field, ".env:1");
        assert!(error.message.contains("KEY=value"));
    }

    #[test]
    fn temp_root_overrides_mutable_test_paths() {
        let config = HepaConfig::load(
            None,
            &BTreeMap::new(),
            HepaConfigOverrides::isolated_temp_root("<TEMP_ROOT>/case-1"),
        )
        .expect("temp-root config should load");

        assert_eq!(config.control_root, "<TEMP_ROOT>/case-1/control");
        assert_eq!(config.worktree_root, "<TEMP_ROOT>/case-1/worktrees");
        assert_eq!(config.archive_root, "<TEMP_ROOT>/case-1/archive");
        assert_eq!(config.routing_file, "<TEMP_ROOT>/case-1/routing.yaml");
        assert_eq!(config.default_adapter, "fake");
    }

    #[test]
    fn config_diagnostics_redact_paths_and_sensitive_settings() {
        let config = HepaConfig {
            control_root: "PRIVATE_ROOT/control".to_string(),
            worktree_root: "PRIVATE_ROOT/worktrees".to_string(),
            archive_root: "PRIVATE_ROOT/archive".to_string(),
            notification: HepaNotificationConfig {
                terminal_enabled: true,
                command: Some("notify --target PRIVATE_DESTINATION".to_string()),
            },
            routing_file: "PRIVATE_ROOT/routing.yaml".to_string(),
            pi: HepaPiConfig {
                model: "deepseek/deepseek-chat".to_string(),
                review_model: Some("deepseek/deepseek-chat".to_string()),
                provider_key_env: Some("DEEPSEEK_API_KEY".to_string()),
                base_url: Some("http://127.0.0.1:11434/v1".to_string()),
            },
            hermes: HepaHermesBridgeConfig {
                enabled: true,
                endpoint: Some("https://hermes.invalid/team".to_string()),
                board_id: Some("board-private".to_string()),
                sync_interval_seconds: 15,
            },
            ..HepaConfig::default()
        };

        let diagnostics = config.redacted_diagnostics();
        let json = serde_json::to_string(&diagnostics).expect("diagnostics should serialize");

        assert_eq!(diagnostics.control_root, "<CONTROL_ROOT>");
        assert_eq!(diagnostics.worktree_root, "<WORKTREE_ROOT>");
        assert_eq!(diagnostics.archive_root, "<ARCHIVE_ROOT>");
        assert_eq!(diagnostics.routing_file, "<ROUTING_FILE>");
        assert_eq!(diagnostics.pi.model, "deepseek/deepseek-chat");
        assert!(diagnostics.pi.review_model_configured);
        assert!(diagnostics.pi.provider_key_env_configured);
        assert!(diagnostics.pi.base_url_configured);
        assert!(diagnostics.notification.command_configured);
        assert!(diagnostics.hermes.endpoint_configured);
        assert!(diagnostics.hermes.board_id_configured);
        assert!(!json.contains("PRIVATE_ROOT"));
        assert!(!json.contains("127.0.0.1"));
        assert!(!json.contains("DEEPSEEK_API_KEY"));
        assert!(!json.contains("PRIVATE_DESTINATION"));
        assert!(!json.contains("hermes.invalid"));
        assert!(!json.contains("board-private"));
    }

    #[test]
    fn dotenv_example_documents_conservative_defaults() {
        let dotenv_example = include_str!("../../../.env.example");
        for key in [
            "HEPA_CONTROL_ROOT",
            "HEPA_WORKTREE_ROOT",
            "HEPA_ARCHIVE_ROOT",
            "HEPA_MAX_TOTAL_ROUNDS",
            "HEPA_MAX_REPAIR_ROUNDS",
            "HEPA_NOTIFY_TERMINAL",
            "HEPA_NOTIFY_COMMAND",
            "HEPA_DEFAULT_ADAPTER",
            "HEPA_ROUTING_FILE",
            "HEPA_HERMES_ENABLED",
            "HEPA_HERMES_ENDPOINT",
            "HEPA_HERMES_BOARD_ID",
            "HEPA_HERMES_SYNC_INTERVAL_SECONDS",
        ] {
            assert!(
                dotenv_example.contains(&format!("{key}=")),
                "{key} missing from .env.example"
            );
        }

        let config = HepaConfig::load(
            Some(dotenv_example),
            &BTreeMap::new(),
            HepaConfigOverrides::default(),
        )
        .expect(".env.example should parse");

        assert_eq!(config, HepaConfig::default());
    }
}
