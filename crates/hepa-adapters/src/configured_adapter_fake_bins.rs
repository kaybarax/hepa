use crate::{
    custom::HepaCustomAdapterTemplate,
    engine::{HepaOneshotAdapterExecutor, HepaOneshotAdapterInvocation},
    external_worker::HepaExternalWorkerAdapterTemplate,
    local_worker::HepaLocalWorkerAdapterTemplate,
    spec::{
        ADAPTER_SPEC_SCHEMA_VERSION, HepaAdapterCostClass, HepaAdapterMode, HepaAdapterRole,
        HepaAdapterSandbox, HepaAdapterSpec, HepaAdapterTemplateContext,
    },
    user_reviewer::HepaUserReviewerAdapterTemplate,
    user_worker::HepaUserWorkerAdapterTemplate,
};
use hepa_core::monitor::HepaMonitorPolicy;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn configured_oneshot_adapters_pass_fake_binary_invocation_output_status_and_artifacts() {
    let root = unique_test_dir("configured-oneshot");
    let worktree = root.join("worktree");
    let artifact_dir = root.join("artifacts");
    fs::create_dir_all(&worktree).expect("worktree dir");
    fs::create_dir_all(&artifact_dir).expect("artifact dir");
    let fake_bin = root.join("fake-adapter");
    write_fake_adapter(&fake_bin);

    let cases = vec![
        (
            shell_command_spec(&fake_bin),
            HepaAdapterRole::Worker,
            "shell-command",
        ),
        (
            HepaCustomAdapterTemplate {
                command: command_template(&fake_bin, false),
                review_command: Some(command_template(&fake_bin, true)),
                required_commands: vec![fake_bin.to_string_lossy().to_string()],
                roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
                ..HepaCustomAdapterTemplate::default()
            }
            .into_spec()
            .expect("custom spec"),
            HepaAdapterRole::Worker,
            "custom",
        ),
        (
            HepaUserWorkerAdapterTemplate {
                command: command_template(&fake_bin, false),
                required_commands: vec![fake_bin.to_string_lossy().to_string()],
                ..HepaUserWorkerAdapterTemplate::default()
            }
            .into_spec()
            .expect("user worker spec"),
            HepaAdapterRole::Worker,
            "user-worker",
        ),
        (
            HepaUserReviewerAdapterTemplate {
                review_command: command_template(&fake_bin, true),
                required_commands: vec![fake_bin.to_string_lossy().to_string()],
                ..HepaUserReviewerAdapterTemplate::default()
            }
            .into_spec()
            .expect("user reviewer spec"),
            HepaAdapterRole::Reviewer,
            "user-reviewer",
        ),
        (
            HepaLocalWorkerAdapterTemplate {
                command: command_template(&fake_bin, false),
                review_command: command_template(&fake_bin, true),
                required_commands: vec![fake_bin.to_string_lossy().to_string()],
                ..HepaLocalWorkerAdapterTemplate::default()
            }
            .into_spec()
            .expect("local worker spec"),
            HepaAdapterRole::Reviewer,
            "local-worker",
        ),
    ];

    for (spec, role, label) in cases {
        let output_file = artifact_dir.join(format!("{label}.json"));
        let invocation = HepaOneshotAdapterInvocation {
            spec,
            role,
            context: template_context(&worktree, &artifact_dir, &output_file),
            prompt: format!("fake binary task for {label}"),
            environment: BTreeMap::new(),
            monitor_policy: HepaMonitorPolicy::default(),
        };

        let result = HepaOneshotAdapterExecutor::new()
            .run(&invocation)
            .expect("fake adapter should run");

        assert_eq!(result.exit_code, Some(0), "{label} status maps to success");
        assert!(result.stdout.contains("fake stdout"));
        assert!(result.stderr.contains("fake stderr"));
        assert!(
            fs::read_to_string(&output_file)
                .expect("artifact exists")
                .contains(label),
            "{label} artifact is collected"
        );
    }

    remove_test_dir(root);
}

#[test]
fn external_worker_passes_fake_binary_status_reporting_and_artifact_collection() {
    let root = unique_test_dir("configured-external");
    let worktree = root.join("worktree");
    let artifact_dir = root.join("artifacts");
    fs::create_dir_all(&worktree).expect("worktree dir");
    fs::create_dir_all(&artifact_dir).expect("artifact dir");
    let fake_bin = root.join("fake-adapter");
    write_fake_adapter(&fake_bin);
    let output_file = artifact_dir.join("external-worker.json");
    let spec = HepaExternalWorkerAdapterTemplate {
        status_command: command_template(&fake_bin, false),
        required_commands: vec![fake_bin.to_string_lossy().to_string()],
        ..HepaExternalWorkerAdapterTemplate::default()
    }
    .into_spec()
    .expect("external worker spec");
    let command = spec
        .render_worker_command(&template_context(&worktree, &artifact_dir, &output_file))
        .expect("external command renders");
    let output = run_split_command(&command);

    assert!(output.status.success());
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(external_status(output.status.code()), "reported");
    assert!(String::from_utf8_lossy(&output.stdout).contains("fake stdout"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("fake stderr"));
    assert!(
        fs::read_to_string(&output_file)
            .expect("artifact exists")
            .contains("external-worker")
    );

    remove_test_dir(root);
}

#[test]
fn configured_adapters_render_without_git_lifecycle_actions() {
    let root = unique_test_dir("configured-no-git-lifecycle");
    let worktree = root.join("worktree");
    let artifact_dir = root.join("artifacts");
    fs::create_dir_all(&worktree).expect("worktree dir");
    fs::create_dir_all(&artifact_dir).expect("artifact dir");
    let fake_bin = root.join("fake-adapter");
    write_fake_adapter(&fake_bin);
    let output_file = artifact_dir.join("output.json");
    let context = template_context(&worktree, &artifact_dir, &output_file);
    let policy = HepaMonitorPolicy::default();

    for (spec, role, label) in configured_adapter_specs(&fake_bin) {
        let command = match role {
            HepaAdapterRole::Worker => spec.render_worker_command(&context),
            HepaAdapterRole::Reviewer => spec
                .render_review_command(&context)
                .map(|command| command.expect("review command should render")),
        }
        .unwrap_or_else(|error| panic!("{label} command should render: {error}"));

        policy
            .check_command(&command)
            .unwrap_or_else(|error| panic!("{label} must not render Git lifecycle: {error}"));
    }

    remove_test_dir(root);
}

#[test]
fn configured_adapter_executor_blocks_git_lifecycle_before_spawn() {
    let root = unique_test_dir("configured-blocks-git-lifecycle");
    let worktree = root.join("worktree");
    let artifact_dir = root.join("artifacts");
    fs::create_dir_all(&worktree).expect("worktree dir");
    fs::create_dir_all(&artifact_dir).expect("artifact dir");
    let fake_bin = root.join("fake-adapter");
    write_fake_adapter(&fake_bin);
    let output_file = artifact_dir.join("blocked.json");
    let spec = HepaCustomAdapterTemplate {
        command: format!(
            "{} --prompt-file {{prompt_file}} --json-output {{output_file}} && git -C {{worktree}} push",
            fake_bin.display()
        ),
        required_commands: vec![fake_bin.to_string_lossy().to_string()],
        ..HepaCustomAdapterTemplate::default()
    }
    .into_spec()
    .expect("custom spec");
    let invocation = HepaOneshotAdapterInvocation {
        spec,
        role: HepaAdapterRole::Worker,
        context: template_context(&worktree, &artifact_dir, &output_file),
        prompt: "blocked lifecycle task".to_string(),
        environment: BTreeMap::new(),
        monitor_policy: HepaMonitorPolicy::default(),
    };

    let error = HepaOneshotAdapterExecutor::new()
        .run(&invocation)
        .expect_err("adapter Git lifecycle must be blocked before spawn");

    assert_eq!(error.field, "monitor");
    assert_eq!(error.status.as_deref(), Some("blocked"));
    assert!(error.message.contains("command_policy"));
    assert!(!PathBuf::from(&invocation.context.prompt_file).exists());
    assert!(!output_file.exists());

    remove_test_dir(root);
}

#[test]
fn configured_adapter_executor_blocks_unrestricted_host_bypass_before_spawn() {
    let root = unique_test_dir("configured-blocks-host-bypass");
    let worktree = root.join("worktree");
    let artifact_dir = root.join("artifacts");
    fs::create_dir_all(&worktree).expect("worktree dir");
    fs::create_dir_all(&artifact_dir).expect("artifact dir");
    let output_file = artifact_dir.join("blocked.json");
    let spec = HepaCustomAdapterTemplate {
        command: "hepa-custom-adapter --prompt-file {prompt_file} --json-output {output_file} --dangerously-skip-permissions"
            .to_string(),
        required_commands: vec!["hepa-custom-adapter".to_string()],
        ..HepaCustomAdapterTemplate::default()
    }
    .into_spec()
    .expect("custom spec");
    let invocation = HepaOneshotAdapterInvocation {
        spec,
        role: HepaAdapterRole::Worker,
        context: template_context(&worktree, &artifact_dir, &output_file),
        prompt: "blocked host bypass task".to_string(),
        environment: BTreeMap::new(),
        monitor_policy: HepaMonitorPolicy::default(),
    };

    let error = HepaOneshotAdapterExecutor::new()
        .run(&invocation)
        .expect_err("unrestricted host bypass must be blocked before spawn");

    assert_eq!(error.field, "invocation_template");
    assert_eq!(error.status.as_deref(), Some("blocked"));
    assert!(error.message.contains("--dangerously-skip-permissions"));
    assert!(!PathBuf::from(&invocation.context.prompt_file).exists());
    assert!(!output_file.exists());

    remove_test_dir(root);
}

fn shell_command_spec(fake_bin: &Path) -> HepaAdapterSpec {
    HepaAdapterSpec {
        schema_version: ADAPTER_SPEC_SCHEMA_VERSION,
        id: "shell-command".to_string(),
        display_name: "Shell Command".to_string(),
        roles: vec![HepaAdapterRole::Worker],
        mode: HepaAdapterMode::Oneshot,
        command: command_template(fake_bin, false),
        review_command: None,
        workdir: "{worktree}".to_string(),
        required_commands: vec![fake_bin.to_string_lossy().to_string()],
        required_env: Vec::new(),
        sandbox: HepaAdapterSandbox::AgentNative,
        supports_resume: false,
        supports_json_output: true,
        capabilities: vec!["docs".to_string(), "smoke".to_string()],
        cost_class: HepaAdapterCostClass::Local,
        resource_weight: 1,
        max_concurrency: 1,
    }
}

fn configured_adapter_specs(
    fake_bin: &Path,
) -> Vec<(HepaAdapterSpec, HepaAdapterRole, &'static str)> {
    vec![
        (
            shell_command_spec(fake_bin),
            HepaAdapterRole::Worker,
            "shell-command",
        ),
        (
            HepaCustomAdapterTemplate {
                command: command_template(fake_bin, false),
                review_command: Some(command_template(fake_bin, true)),
                required_commands: vec![fake_bin.to_string_lossy().to_string()],
                roles: vec![HepaAdapterRole::Worker, HepaAdapterRole::Reviewer],
                ..HepaCustomAdapterTemplate::default()
            }
            .into_spec()
            .expect("custom spec"),
            HepaAdapterRole::Worker,
            "custom",
        ),
        (
            HepaUserWorkerAdapterTemplate {
                command: command_template(fake_bin, false),
                required_commands: vec![fake_bin.to_string_lossy().to_string()],
                ..HepaUserWorkerAdapterTemplate::default()
            }
            .into_spec()
            .expect("user worker spec"),
            HepaAdapterRole::Worker,
            "user-worker",
        ),
        (
            HepaUserReviewerAdapterTemplate {
                review_command: command_template(fake_bin, true),
                required_commands: vec![fake_bin.to_string_lossy().to_string()],
                ..HepaUserReviewerAdapterTemplate::default()
            }
            .into_spec()
            .expect("user reviewer spec"),
            HepaAdapterRole::Reviewer,
            "user-reviewer",
        ),
        (
            HepaLocalWorkerAdapterTemplate {
                command: command_template(fake_bin, false),
                review_command: command_template(fake_bin, true),
                required_commands: vec![fake_bin.to_string_lossy().to_string()],
                ..HepaLocalWorkerAdapterTemplate::default()
            }
            .into_spec()
            .expect("local worker spec"),
            HepaAdapterRole::Reviewer,
            "local-worker",
        ),
        (
            HepaExternalWorkerAdapterTemplate {
                status_command: command_template(fake_bin, false),
                required_commands: vec![fake_bin.to_string_lossy().to_string()],
                ..HepaExternalWorkerAdapterTemplate::default()
            }
            .into_spec()
            .expect("external worker spec"),
            HepaAdapterRole::Worker,
            "external-worker",
        ),
    ]
}

fn command_template(fake_bin: &Path, review: bool) -> String {
    let prompt = if review {
        "{review_prompt_file}"
    } else {
        "{prompt_file}"
    };
    let output = if review {
        "{review_output_file}"
    } else {
        "{output_file}"
    };
    format!(
        "{} --prompt-file {prompt} --artifact-dir {{artifact_dir}} --json-output {output}",
        fake_bin.display()
    )
}

fn template_context(
    worktree: &Path,
    artifact_dir: &Path,
    output_file: &Path,
) -> HepaAdapterTemplateContext {
    HepaAdapterTemplateContext {
        prompt_file: artifact_dir.join("prompt.md").to_string_lossy().to_string(),
        worktree: worktree.to_string_lossy().to_string(),
        review_prompt_file: artifact_dir.join("review.md").to_string_lossy().to_string(),
        output_file: output_file.to_string_lossy().to_string(),
        review_output_file: output_file.to_string_lossy().to_string(),
        artifact_dir: artifact_dir.to_string_lossy().to_string(),
    }
}

fn write_fake_adapter(path: &Path) {
    fs::write(
        path,
        r#"#!/bin/sh
output_file=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --json-output) output_file="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf 'fake stdout'
printf 'fake stderr' >&2
printf '{"adapter":"%s","status":"completed"}\n' "$(basename "$output_file" .json)" > "$output_file"
"#,
    )
    .expect("fake adapter write");
    let mut permissions = fs::metadata(path)
        .expect("fake adapter metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).expect("fake adapter permissions");
}

fn run_split_command(command: &str) -> std::process::Output {
    let parts = command.split_whitespace().collect::<Vec<_>>();
    let (program, args) = parts.split_first().expect("command has program");
    Command::new(program)
        .args(args)
        .output()
        .expect("fake external command should run")
}

fn external_status(exit_code: Option<i32>) -> &'static str {
    if exit_code == Some(0) {
        "reported"
    } else {
        "blocked"
    }
}

fn unique_test_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("hepa-configured-adapter-{label}-{nonce}"))
}

fn remove_test_dir(root: PathBuf) {
    if root.exists() {
        fs::remove_dir_all(root).expect("test dir cleanup");
    }
}
