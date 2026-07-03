use hepa_adapters::builtin::{BUILTIN_ADAPTER_IDS, builtin_adapter_spec};
use hepa_adapters::container::unrestricted_bypass_flag;
use hepa_adapters::doctor::unsupported_hepa_flags;
use hepa_adapters::spec::HepaAdapterRole;
use hepa_core::env_allowlist::{HepaEnvAllowlist, HepaEnvRole, MANAGER_ONLY_CREDENTIALS};
use std::collections::BTreeMap;

fn env_role(role: &HepaAdapterRole) -> HepaEnvRole {
    match role {
        HepaAdapterRole::Worker => HepaEnvRole::Worker,
        HepaAdapterRole::Reviewer => HepaEnvRole::Reviewer,
    }
}

fn host_env_with_credentials() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("PATH".to_string(), "/usr/bin".to_string()),
        ("GITHUB_TOKEN".to_string(), "ghp_example".to_string()),
        ("GH_TOKEN".to_string(), "gho_example".to_string()),
        ("HEPA_GITHUB_TOKEN".to_string(), "ghp_example2".to_string()),
        ("OPENAI_API_KEY".to_string(), "sk-example".to_string()),
    ])
}

/// Every built-in adapter, in every role it declares, must produce a worker or
/// reviewer environment that contains no manager-only credentials.
#[test]
fn every_builtin_adapter_role_env_excludes_manager_credentials() {
    for id in BUILTIN_ADAPTER_IDS {
        let spec = builtin_adapter_spec(id);
        for role in &spec.roles {
            let allowlist = HepaEnvAllowlist::for_role(env_role(role), &spec.required_env);
            let filtered = allowlist.filter(&host_env_with_credentials());

            for credential in MANAGER_ONLY_CREDENTIALS {
                assert!(
                    !filtered.contains_key(*credential),
                    "adapter {id} ({role:?}) leaked manager credential {credential}"
                );
            }
            // Default-deny also drops other secret-like host keys.
            assert!(
                !filtered.contains_key("OPENAI_API_KEY"),
                "adapter {id} ({role:?}) leaked an undeclared host secret"
            );
        }
    }
}

/// No built-in adapter's HEPA-composed command grants unrestricted host access.
#[test]
fn no_builtin_adapter_command_uses_bypass_flags() {
    for id in BUILTIN_ADAPTER_IDS {
        let spec = builtin_adapter_spec(id);
        assert_eq!(
            unrestricted_bypass_flag(&spec.command),
            None,
            "adapter {id} worker command uses a bypass flag"
        );
        if let Some(review_command) = &spec.review_command {
            assert_eq!(
                unrestricted_bypass_flag(review_command),
                None,
                "adapter {id} review command uses a bypass flag"
            );
        }
    }
}

#[test]
fn pi_command_boundary_rejects_bypass_flags() {
    let mut spec = builtin_adapter_spec("pi");
    assert_eq!(unsupported_hepa_flags(&spec.command), Vec::<String>::new());
    assert!(spec.command.contains("--no-approve"));
    assert!(!spec.command.contains("--approve "));

    spec.command.push_str(" --no-sandbox");

    assert_eq!(
        unsupported_hepa_flags(&spec.command),
        vec!["--no-sandbox".to_string()]
    );
}

#[test]
fn pi_command_boundary_rejects_project_trust_approval() {
    assert_eq!(
        unsupported_hepa_flags("pi --approve -p --mode json --model openai/gpt-4.1"),
        vec!["--approve".to_string()]
    );
}
