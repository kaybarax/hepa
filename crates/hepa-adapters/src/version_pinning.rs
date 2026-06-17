use crate::builtin::{BUILTIN_ADAPTER_IDS, builtin_adapter_spec};
use std::collections::BTreeMap;

/// The baseline version every built-in adapter template is pinned at.
pub const BUILTIN_PINNED_VERSION: &str = "1.0.0";

/// A known-good invocation template for a specific adapter version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaAdapterVersionPin {
    pub adapter_id: String,
    pub version: String,
    pub command_template: String,
}

/// Registry of known-good per-version invocation templates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HepaVersionPinRegistry {
    pins: BTreeMap<(String, String), HepaAdapterVersionPin>,
}

impl HepaVersionPinRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry pre-pinned with every built-in adapter's command template at
    /// the baseline version.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        for id in BUILTIN_ADAPTER_IDS {
            let spec = builtin_adapter_spec(id);
            registry.pin(HepaAdapterVersionPin {
                adapter_id: spec.id.clone(),
                version: BUILTIN_PINNED_VERSION.to_string(),
                command_template: spec.command.clone(),
            });
        }
        registry
    }

    pub fn pin(&mut self, pin: HepaAdapterVersionPin) {
        self.pins
            .insert((pin.adapter_id.clone(), pin.version.clone()), pin);
    }

    pub fn template_for(&self, adapter_id: &str, version: &str) -> Option<&str> {
        self.pins
            .get(&(adapter_id.to_string(), version.to_string()))
            .map(|pin| pin.command_template.as_str())
    }

    pub fn is_pinned(&self, adapter_id: &str, version: &str) -> bool {
        self.pins
            .contains_key(&(adapter_id.to_string(), version.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_pinned_with_their_command_templates() {
        let registry = HepaVersionPinRegistry::with_builtins();

        for id in BUILTIN_ADAPTER_IDS {
            assert!(
                registry.is_pinned(id, BUILTIN_PINNED_VERSION),
                "{id} should be pinned"
            );
            let spec = builtin_adapter_spec(id);
            assert_eq!(
                registry.template_for(id, BUILTIN_PINNED_VERSION),
                Some(spec.command.as_str())
            );
        }
    }

    #[test]
    fn unknown_version_is_not_pinned() {
        let registry = HepaVersionPinRegistry::with_builtins();
        assert!(!registry.is_pinned("fake", "9.9.9"));
        assert!(registry.template_for("fake", "9.9.9").is_none());
    }

    #[test]
    fn custom_pins_can_be_registered() {
        let mut registry = HepaVersionPinRegistry::new();
        registry.pin(HepaAdapterVersionPin {
            adapter_id: "claude".to_string(),
            version: "2.1.0".to_string(),
            command_template: "claude run --prompt-file {prompt_file}".to_string(),
        });
        assert!(registry.is_pinned("claude", "2.1.0"));
    }
}
