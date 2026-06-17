use crate::spec::{
    ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterMode, HepaAdapterOutputCapture,
    HepaAdapterPromptTransport, HepaAdapterRole, HepaAdapterSandbox, HepaAdapterSpec,
};
use std::collections::BTreeMap;

pub const BUILTIN_ADAPTER_IDS: [&str; 8] = [
    "pi",
    "fake",
    "shell-command",
    "custom",
    "user-worker",
    "user-reviewer",
    "local-worker",
    "external-worker",
];

pub const BUILTIN_ADAPTER_LIST_ORDER: [&str; 8] = [
    "custom",
    "external-worker",
    "fake",
    "local-worker",
    "pi",
    "shell-command",
    "user-reviewer",
    "user-worker",
];

pub fn builtin_adapter_specs() -> BTreeMap<String, HepaAdapterSpec> {
    BUILTIN_ADAPTER_IDS
        .into_iter()
        .map(|id| {
            let spec = builtin_adapter_spec(id);
            (spec.id.clone(), spec)
        })
        .collect()
}

pub fn builtin_adapter_spec(id: &str) -> HepaAdapterSpec {
    match id {
        "fake" => spec(AdapterSpecTemplate {
            id: "fake",
            display_name: "Fake Adapter",
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: "hepa-fake-adapter worker --prompt-file {prompt_file} --json-output {output_file}",
            review_command: Some(
                "hepa-fake-adapter reviewer --prompt-file {review_prompt_file} --json-output {review_output_file}",
            ),
            required_commands: Vec::new(),
            sandbox: HepaAdapterSandbox::None,
            supports_resume: false,
            supports_json_output: true,
            capabilities: vec!["docs", "test", "review"],
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 64,
        }),
        "pi" => HepaAdapterSpec {
            schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
            id: "pi".to_string(),
            display_name: "Pi Coding Agent".to_string(),
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: "pi --no-approve --no-session --no-extensions --no-skills --no-prompt-templates --no-context-files --tools read,edit,write,bash,grep,find,ls -p --mode json --model deepseek/deepseek-chat".to_string(),
            review_command: Some(
                "pi --no-approve --no-session --no-extensions --no-skills --no-prompt-templates --no-context-files --tools read,edit,write,bash,grep,find,ls -p --mode json --model deepseek/deepseek-chat".to_string(),
            ),
            workdir: "{worktree}".to_string(),
            required_commands: vec!["pi".to_string()],
            required_env: vec!["DEEPSEEK_API_KEY".to_string()],
            sandbox: HepaAdapterSandbox::None,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec![
                "frontend".to_string(),
                "backend".to_string(),
                "refactor".to_string(),
                "docs".to_string(),
                "review".to_string(),
                "local-only".to_string(),
            ],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 2,
            prompt_transport: HepaAdapterPromptTransport::PromptArg,
            output_capture: HepaAdapterOutputCapture::Stdout,
        },
        "shell-command" => spec(AdapterSpecTemplate {
            id: "shell-command",
            display_name: "Shell Command Adapter",
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: "hepa-shell-adapter --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file}",
            review_command: None,
            required_commands: vec!["hepa-shell-adapter"],
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: false,
            supports_json_output: true,
            capabilities: vec!["format", "codegen", "docs"],
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 4,
        }),
        "custom" => spec(AdapterSpecTemplate {
            id: "custom",
            display_name: "Custom Adapter Template",
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: "hepa-custom-adapter --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file}",
            review_command: Some(
                "hepa-custom-adapter --review --prompt-file {review_prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {review_output_file}",
            ),
            required_commands: vec!["hepa-custom-adapter"],
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["frontend", "backend", "refactor", "docs", "review"],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 2,
        }),
        "user-worker" => spec(AdapterSpecTemplate {
            id: "user-worker",
            display_name: "User Worker Adapter Template",
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: "hepa-user-worker-adapter --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file}",
            review_command: None,
            required_commands: vec!["hepa-user-worker-adapter"],
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["frontend", "backend", "refactor", "docs"],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 2,
        }),
        "user-reviewer" => spec(AdapterSpecTemplate {
            id: "user-reviewer",
            display_name: "User Reviewer Adapter Template",
            roles: vec![HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: "hepa-user-reviewer-adapter --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file}",
            review_command: Some(
                "hepa-user-reviewer-adapter --prompt-file {review_prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {review_output_file}",
            ),
            required_commands: vec!["hepa-user-reviewer-adapter"],
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["review"],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 3,
        }),
        "local-worker" => spec(AdapterSpecTemplate {
            id: "local-worker",
            display_name: "Local Worker Adapter Template",
            roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
            mode: HepaAdapterMode::Oneshot,
            command: "hepa-local-worker-adapter --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file}",
            review_command: Some(
                "hepa-local-worker-adapter --review --prompt-file {review_prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {review_output_file}",
            ),
            required_commands: vec!["hepa-local-worker-adapter"],
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec![
                "frontend",
                "backend",
                "refactor",
                "docs",
                "review",
                "local-only",
            ],
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 4,
        }),
        "external-worker" => spec(AdapterSpecTemplate {
            id: "external-worker",
            display_name: "External Worker Adapter Template",
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::External,
            command: "hepa-external-worker-adapter --prompt-file {prompt_file} --worktree {worktree} --artifact-dir {artifact_dir} --json-output {output_file}",
            review_command: None,
            required_commands: vec!["hepa-external-worker-adapter"],
            sandbox: HepaAdapterSandbox::None,
            supports_resume: true,
            supports_json_output: true,
            capabilities: vec!["external", "status"],
            cost_class: HepaAdapterCostClass::PaidCloud,
            resource_weight: 1,
            max_concurrency: 8,
        }),
        _ => spec(AdapterSpecTemplate {
            id,
            display_name: "Unknown Adapter Template",
            roles: vec![HepaAdapterRole::Worker],
            mode: HepaAdapterMode::Oneshot,
            command: "hepa-unknown-adapter --prompt-file {prompt_file} --json-output {output_file}",
            review_command: None,
            required_commands: vec!["hepa-unknown-adapter"],
            sandbox: HepaAdapterSandbox::AgentNative,
            supports_resume: false,
            supports_json_output: true,
            capabilities: vec!["docs"],
            cost_class: HepaAdapterCostClass::Local,
            resource_weight: 1,
            max_concurrency: 1,
        }),
    }
}

struct AdapterSpecTemplate<'a> {
    id: &'a str,
    display_name: &'a str,
    roles: Vec<HepaAdapterRole>,
    mode: HepaAdapterMode,
    command: &'a str,
    review_command: Option<&'a str>,
    required_commands: Vec<&'a str>,
    sandbox: HepaAdapterSandbox,
    supports_resume: bool,
    supports_json_output: bool,
    capabilities: Vec<&'a str>,
    cost_class: HepaAdapterCostClass,
    resource_weight: u32,
    max_concurrency: u32,
}

fn spec(template: AdapterSpecTemplate<'_>) -> HepaAdapterSpec {
    HepaAdapterSpec {
        schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
        id: template.id.to_string(),
        display_name: template.display_name.to_string(),
        roles: template.roles,
        mode: template.mode,
        command: template.command.to_string(),
        review_command: template.review_command.map(str::to_string),
        workdir: "{worktree}".to_string(),
        required_commands: template
            .required_commands
            .into_iter()
            .map(str::to_string)
            .collect(),
        required_env: Vec::new(),
        sandbox: template.sandbox,
        supports_resume: template.supports_resume,
        supports_json_output: template.supports_json_output,
        capabilities: template
            .capabilities
            .into_iter()
            .map(str::to_string)
            .collect(),
        cost_class: template.cost_class,
        resource_weight: template.resource_weight,
        max_concurrency: template.max_concurrency,
        prompt_transport: HepaAdapterPromptTransport::PromptFile,
        output_capture: HepaAdapterOutputCapture::AdapterFile,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_adapter_set_matches_architecture_ids() {
        let specs = builtin_adapter_specs();

        assert_eq!(
            specs.keys().map(String::as_str).collect::<Vec<_>>(),
            BUILTIN_ADAPTER_LIST_ORDER
        );
        for id in BUILTIN_ADAPTER_IDS {
            assert!(specs.contains_key(id), "missing built-in adapter {id}");
        }
        for spec in specs.values() {
            spec.validate().expect("built-in spec should validate");
            assert!(
                !spec.required_env.iter().any(|key| key == "GITHUB_TOKEN"
                    || key.starts_with("HEPA_MANAGER_")
                    || key.starts_with("MANAGER_")),
                "built-ins must not require manager credentials"
            );
        }
    }

    #[test]
    fn builtin_modes_roles_and_postures_are_provider_neutral() {
        let specs = builtin_adapter_specs();

        assert_eq!(specs["fake"].mode, HepaAdapterMode::Oneshot);
        assert_eq!(specs["shell-command"].roles, vec![HepaAdapterRole::Worker]);
        assert_eq!(
            specs["user-reviewer"].roles,
            vec![HepaAdapterRole::Reviewer]
        );
        assert_eq!(
            specs["local-worker"].cost_class,
            HepaAdapterCostClass::Local
        );
        assert_eq!(specs["external-worker"].mode, HepaAdapterMode::External);
        assert!(
            specs
                .values()
                .all(|spec| spec.sandbox != HepaAdapterSandbox::Container),
            "container mode is explicit and not a hidden default"
        );
    }
}
