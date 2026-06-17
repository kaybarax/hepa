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

    /// Doctor check: warn when an adapter version has no pinned template.
    pub fn warn_if_untested(&self, adapter_id: &str, version: &str) -> Option<HepaVersionWarning> {
        if self.is_pinned(adapter_id, version) {
            return None;
        }
        Some(HepaVersionWarning {
            adapter_id: adapter_id.to_string(),
            version: version.to_string(),
            kind: HepaVersionWarningKind::UntestedVersion,
            message: format!(
                "adapter {adapter_id} version {version} is untested; pin a known-good \
                 invocation template before relying on it"
            ),
        })
    }

    /// Doctor check: detect invocation flag drift. An untested version warns; a
    /// pinned version whose actual command differs from the pinned template is
    /// flagged as drift so a silently broken invocation is caught.
    pub fn detect_flag_drift(
        &self,
        adapter_id: &str,
        version: &str,
        actual_command: &str,
    ) -> Option<HepaVersionWarning> {
        match self.template_for(adapter_id, version) {
            None => self.warn_if_untested(adapter_id, version),
            Some(pinned) if pinned != actual_command => Some(HepaVersionWarning {
                adapter_id: adapter_id.to_string(),
                version: version.to_string(),
                kind: HepaVersionWarningKind::FlagDrift,
                message: format!(
                    "adapter {adapter_id} version {version} invocation drifted from its pinned \
                     template; re-pin and re-validate before use"
                ),
            }),
            Some(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HepaVersionWarningKind {
    UntestedVersion,
    FlagDrift,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaVersionWarning {
    pub adapter_id: String,
    pub version: String,
    pub kind: HepaVersionWarningKind,
    pub message: String,
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
    fn doctor_warns_on_untested_versions_with_actionable_message() {
        let registry = HepaVersionPinRegistry::with_builtins();

        let warning = registry
            .warn_if_untested("fake", "9.9.9")
            .expect("untested version should warn");
        assert_eq!(warning.kind, HepaVersionWarningKind::UntestedVersion);
        assert!(warning.message.contains("untested"));
        assert!(warning.message.contains("pin a known-good"));

        assert!(
            registry
                .warn_if_untested("fake", BUILTIN_PINNED_VERSION)
                .is_none()
        );
    }

    #[test]
    fn simulated_flag_drift_is_detected() {
        let registry = HepaVersionPinRegistry::with_builtins();
        let pinned = registry
            .template_for("fake", BUILTIN_PINNED_VERSION)
            .expect("fake is pinned")
            .to_string();

        // Matching command: no drift.
        assert!(
            registry
                .detect_flag_drift("fake", BUILTIN_PINNED_VERSION, &pinned)
                .is_none()
        );

        // Drifted command (a flag changed): flagged as drift.
        let drifted = format!("{pinned} --new-unexpected-flag");
        let warning = registry
            .detect_flag_drift("fake", BUILTIN_PINNED_VERSION, &drifted)
            .expect("drift should be detected");
        assert_eq!(warning.kind, HepaVersionWarningKind::FlagDrift);
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
