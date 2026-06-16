use crate::{builtin::builtin_adapter_specs, spec::HepaAdapterSpec};
use hepa_core::config::HepaConfig;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

pub const ADAPTER_REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HepaAdapterRegistryDocument {
    pub schema_version: u32,
    pub adapters: BTreeMap<String, HepaAdapterSpec>,
}

impl Default for HepaAdapterRegistryDocument {
    fn default() -> Self {
        Self {
            schema_version: ADAPTER_REGISTRY_SCHEMA_VERSION,
            adapters: builtin_adapter_specs(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaAdapterRegistry {
    path: PathBuf,
    document: HepaAdapterRegistryDocument,
}

impl HepaAdapterRegistry {
    pub fn load_from_config(config: &HepaConfig) -> Result<Self, HepaAdapterRegistryError> {
        Self::load(Path::new(&config.control_root).join("adapters/registry.json"))
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self, HepaAdapterRegistryError> {
        let path = path.into();
        if !path.exists() {
            return Ok(Self {
                path,
                document: HepaAdapterRegistryDocument::default(),
            });
        }
        let text = fs::read_to_string(&path).map_err(HepaAdapterRegistryError::io)?;
        let document: HepaAdapterRegistryDocument =
            serde_json::from_str(&text).map_err(HepaAdapterRegistryError::serde)?;
        validate_document(&document)?;
        Ok(Self { path, document })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn list(&self) -> Vec<&HepaAdapterSpec> {
        self.document.adapters.values().collect()
    }

    pub fn get(&self, adapter_id: &str) -> Option<&HepaAdapterSpec> {
        self.document.adapters.get(adapter_id)
    }

    pub fn upsert(&mut self, spec: HepaAdapterSpec) -> Result<(), HepaAdapterRegistryError> {
        spec.validate()
            .map_err(|error| HepaAdapterRegistryError::invalid("spec", error.to_string()))?;
        self.document.adapters.insert(spec.id.clone(), spec);
        Ok(())
    }

    pub fn remove(
        &mut self,
        adapter_id: &str,
    ) -> Result<HepaAdapterSpec, HepaAdapterRegistryError> {
        self.document
            .adapters
            .remove(adapter_id)
            .ok_or_else(|| HepaAdapterRegistryError::invalid("id", "adapter is not registered"))
    }

    pub fn save(&self) -> Result<(), HepaAdapterRegistryError> {
        validate_document(&self.document)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(HepaAdapterRegistryError::io)?;
        }
        let mut json = serde_json::to_string_pretty(&self.document)
            .map_err(HepaAdapterRegistryError::serde)?;
        if !json.ends_with('\n') {
            json.push('\n');
        }
        let temp_path = self.path.with_extension("tmp");
        fs::write(&temp_path, json).map_err(HepaAdapterRegistryError::io)?;
        fs::rename(temp_path, &self.path).map_err(HepaAdapterRegistryError::io)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaAdapterRegistryError {
    pub field: String,
    pub message: String,
}

impl HepaAdapterRegistryError {
    fn invalid(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }

    fn io(error: io::Error) -> Self {
        Self::invalid("io", error.to_string())
    }

    fn serde(error: serde_json::Error) -> Self {
        Self::invalid("registry", error.to_string())
    }
}

impl fmt::Display for HepaAdapterRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaAdapterRegistryError {}

fn validate_document(
    document: &HepaAdapterRegistryDocument,
) -> Result<(), HepaAdapterRegistryError> {
    if document.schema_version != ADAPTER_REGISTRY_SCHEMA_VERSION {
        return Err(HepaAdapterRegistryError::invalid(
            "schema_version",
            format!("must be {ADAPTER_REGISTRY_SCHEMA_VERSION}"),
        ));
    }
    for (id, spec) in &document.adapters {
        if id != &spec.id {
            return Err(HepaAdapterRegistryError::invalid(
                "id",
                "registry key must match adapter id",
            ));
        }
        spec.validate()
            .map_err(|error| HepaAdapterRegistryError::invalid("spec", error.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{
        ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterMode, HepaAdapterRole,
        HepaAdapterSandbox,
    };
    use hepa_core::config::{HepaConfig, HepaConfigOverrides};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn registry_crud_round_trips_through_control_root() {
        let root = unique_test_dir("crud");
        let config = HepaConfig::load(
            None,
            &BTreeMap::new(),
            HepaConfigOverrides::isolated_temp_root(root.to_string_lossy()),
        )
        .expect("config should load");
        let mut registry = HepaAdapterRegistry::load_from_config(&config)
            .expect("missing registry should load empty");
        let worker = adapter_spec("worker-primary", "agent --prompt-file {prompt_file}");

        assert_eq!(
            registry.path(),
            Path::new(&config.control_root).join("adapters/registry.json")
        );
        assert_eq!(
            registry.list().len(),
            crate::builtin::BUILTIN_ADAPTER_IDS.len()
        );

        registry.upsert(worker.clone()).expect("create should work");
        registry.save().expect("registry should save");
        let loaded = HepaAdapterRegistry::load_from_config(&config).expect("saved registry loads");
        assert_eq!(
            loaded
                .get("worker-primary")
                .map(|spec| spec.display_name.as_str()),
            Some("worker-primary adapter")
        );

        let mut loaded = loaded;
        let updated = adapter_spec("worker-primary", "agent --json-output {output_file}");
        loaded.upsert(updated).expect("update should work");
        loaded.save().expect("updated registry saves");
        let loaded =
            HepaAdapterRegistry::load_from_config(&config).expect("updated registry loads");
        assert!(
            loaded
                .get("worker-primary")
                .expect("worker exists")
                .command
                .contains("{output_file}")
        );

        let mut loaded = loaded;
        let removed = loaded.remove("worker-primary").expect("delete should work");
        assert_eq!(removed.id, "worker-primary");
        loaded.save().expect("deleted registry saves");
        let loaded =
            HepaAdapterRegistry::load_from_config(&config).expect("deleted registry loads");
        assert!(loaded.get("worker-primary").is_none());
        assert!(loaded.get("fake").is_some());

        remove_test_dir(root);
    }

    #[test]
    fn missing_registry_loads_default_builtins_deterministically() {
        let root = unique_test_dir("defaults");
        let registry_path = root.join("control/adapters/registry.json");

        let registry = HepaAdapterRegistry::load(&registry_path).expect("missing registry loads");
        let ids = registry
            .list()
            .into_iter()
            .map(|spec| spec.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, crate::builtin::BUILTIN_ADAPTER_LIST_ORDER);
        assert_eq!(
            registry
                .get("external-worker")
                .map(|spec| spec.mode.clone()),
            Some(HepaAdapterMode::External)
        );
        assert_eq!(
            registry
                .get("local-worker")
                .map(|spec| spec.cost_class.clone()),
            Some(HepaAdapterCostClass::Local)
        );
        assert!(
            registry
                .list()
                .into_iter()
                .all(|spec| spec.required_env.is_empty())
        );

        remove_test_dir(root);
    }

    #[test]
    fn registry_rejects_invalid_and_mismatched_specs() {
        let root = unique_test_dir("invalid");
        let registry_path = root.join("control/adapters/registry.json");
        let mut registry = HepaAdapterRegistry::load(&registry_path).expect("empty registry");
        let mut invalid = adapter_spec("bad-adapter", "agent --prompt-file {prompt_file}");
        invalid.max_concurrency = 0;

        let error = registry.upsert(invalid).expect_err("invalid specs fail");
        assert_eq!(error.field, "spec");

        let document = HepaAdapterRegistryDocument {
            schema_version: ADAPTER_REGISTRY_SCHEMA_VERSION,
            adapters: BTreeMap::from([(
                "registry-key".to_string(),
                adapter_spec("spec-id", "agent --prompt-file {prompt_file}"),
            )]),
        };
        fs::create_dir_all(registry_path.parent().expect("parent")).expect("parent dir");
        fs::write(
            &registry_path,
            serde_json::to_string_pretty(&document).expect("document serializes"),
        )
        .expect("registry write");

        let error = HepaAdapterRegistry::load(&registry_path).expect_err("mismatched ids fail");
        assert_eq!(error.field, "id");

        remove_test_dir(root);
    }

    fn adapter_spec(id: &str, command: &str) -> HepaAdapterSpec {
        HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: id.to_string(),
            display_name: format!("{id} adapter"),
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: command.to_string(),
            review_command: None,
            workdir: "{worktree}".to_string(),
            required_commands: Vec::new(),
            required_env: Vec::new(),
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: false,
            supports_json_output: true,
            capabilities: vec!["docs".to_string()],
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 1,
        }
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hepa-adapter-registry-{label}-{nonce}"))
    }

    fn remove_test_dir(root: PathBuf) {
        if root.exists() {
            fs::remove_dir_all(root).expect("test dir cleanup");
        }
    }
}
