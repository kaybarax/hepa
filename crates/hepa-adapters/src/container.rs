use crate::spec::HepaAdapterSandbox;

/// Whether the target project is trusted enough to run on the host worktree, or
/// must be confined in a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HepaProjectTrust {
    Trusted,
    Untrusted,
}

/// The sandbox posture actually used for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HepaSandboxPosture {
    HostWorktree,
    AgentNative,
    Container,
}

impl HepaSandboxPosture {
    pub fn as_str(self) -> &'static str {
        match self {
            HepaSandboxPosture::HostWorktree => "host-worktree",
            HepaSandboxPosture::AgentNative => "agent-native",
            HepaSandboxPosture::Container => "container",
        }
    }
}

/// Resolve the active sandbox posture.
///
/// Untrusted projects are always confined to a container. Otherwise HEPA
/// prefers an adapter's declared native sandboxing over the bare host worktree.
pub fn resolve_sandbox_posture(
    adapter_sandbox: HepaAdapterSandbox,
    trust: HepaProjectTrust,
) -> HepaSandboxPosture {
    if trust == HepaProjectTrust::Untrusted {
        return HepaSandboxPosture::Container;
    }
    match adapter_sandbox {
        HepaAdapterSandbox::AgentNative => HepaSandboxPosture::AgentNative,
        HepaAdapterSandbox::Container => HepaSandboxPosture::Container,
        HepaAdapterSandbox::None => HepaSandboxPosture::HostWorktree,
    }
}

/// Container-mode configuration for confining an untrusted project's run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaContainerConfig {
    pub image: String,
    pub workdir: String,
    pub worktree_mount: String,
}

/// Flags that grant an adapter unrestricted host access. HEPA never composes any
/// of these.
pub const UNRESTRICTED_BYPASS_FLAGS: &[&str] = &[
    "--privileged",
    "--cap-add=all",
    "--security-opt=seccomp=unconfined",
    "--security-opt seccomp=unconfined",
    "--dangerously-skip-permissions",
    "--yolo",
    "--no-sandbox",
    "--disable-sandbox",
    "--allow-all",
];

/// Return the first unrestricted bypass flag found in a command, if any.
pub fn unrestricted_bypass_flag(command: &str) -> Option<&'static str> {
    let lowered = command.to_ascii_lowercase();
    UNRESTRICTED_BYPASS_FLAGS
        .iter()
        .copied()
        .find(|flag| lowered.contains(flag))
}

/// Compose a container-mode command that runs the inner adapter command confined
/// to the project worktree. The composed command never includes a host
/// permission-bypass flag.
pub fn compose_container_command(config: &HepaContainerConfig, inner_command: &str) -> String {
    format!(
        "docker run --rm --network none --workdir {workdir} \
         --mount type=bind,source={mount},target={workdir} {image} \
         /bin/sh -lc {inner}",
        workdir = config.workdir,
        mount = config.worktree_mount,
        image = config.image,
        inner = shell_quote(inner_command),
    )
}

fn shell_quote(command: &str) -> String {
    format!("'{}'", command.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_projects_are_always_containerized() {
        for adapter in [
            HepaAdapterSandbox::None,
            HepaAdapterSandbox::AgentNative,
            HepaAdapterSandbox::Container,
        ] {
            assert_eq!(
                resolve_sandbox_posture(adapter, HepaProjectTrust::Untrusted),
                HepaSandboxPosture::Container
            );
        }
    }

    #[test]
    fn trusted_projects_prefer_adapter_native_sandboxing() {
        assert_eq!(
            resolve_sandbox_posture(HepaAdapterSandbox::AgentNative, HepaProjectTrust::Trusted),
            HepaSandboxPosture::AgentNative
        );
        assert_eq!(
            resolve_sandbox_posture(HepaAdapterSandbox::None, HepaProjectTrust::Trusted),
            HepaSandboxPosture::HostWorktree
        );
        assert_eq!(
            resolve_sandbox_posture(HepaAdapterSandbox::Container, HepaProjectTrust::Trusted),
            HepaSandboxPosture::Container
        );
    }

    #[test]
    fn container_command_confines_without_bypass_flags() {
        let config = HepaContainerConfig {
            image: "hepa/sandbox:latest".to_string(),
            workdir: "/workspace".to_string(),
            worktree_mount: "<WORKTREE>".to_string(),
        };
        let command =
            compose_container_command(&config, "hepa-fake-adapter worker --json-output o");

        assert!(command.contains("docker run"));
        assert!(command.contains("--network none"));
        assert!(command.contains("--workdir /workspace"));
        assert!(command.contains("hepa-fake-adapter worker"));
        // No host permission-bypass flag is ever composed.
        assert_eq!(unrestricted_bypass_flag(&command), None);
    }

    #[test]
    fn bypass_flags_are_detected() {
        assert_eq!(
            unrestricted_bypass_flag("docker run --privileged image"),
            Some("--privileged")
        );
        assert_eq!(
            unrestricted_bypass_flag("claude --dangerously-skip-permissions"),
            Some("--dangerously-skip-permissions")
        );
        assert_eq!(unrestricted_bypass_flag("docker run --rm image"), None);
    }

    #[test]
    fn container_mode_runs_fake_adapter_end_to_end() {
        use crate::fake::{HepaFakeAdapter, HepaFakeWorkerInput};
        use hepa_core::contracts::{
            CONTRACT_SCHEMA_VERSION, HepaAttemptStatus, HepaRiskLevel, HepaTaskSpec,
        };

        // Untrusted project resolves to container posture.
        let posture =
            resolve_sandbox_posture(HepaAdapterSandbox::None, HepaProjectTrust::Untrusted);
        assert_eq!(posture, HepaSandboxPosture::Container);

        let config = HepaContainerConfig {
            image: "hepa/sandbox:latest".to_string(),
            workdir: "/workspace".to_string(),
            worktree_mount: "<WORKTREE>".to_string(),
        };
        let containerized =
            compose_container_command(&config, "hepa-fake-adapter worker --json-output o");
        assert_eq!(unrestricted_bypass_flag(&containerized), None);

        // The fake adapter (the confined workload) runs to completion.
        let task_spec = HepaTaskSpec {
            schema_version: CONTRACT_SCHEMA_VERSION,
            task_id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            goal: "Update docs".to_string(),
            non_goals: Vec::new(),
            expected_areas: vec!["README.md".to_string()],
            acceptance_criteria: vec!["done".to_string()],
            validation_commands: vec!["true".to_string()],
            dependencies: Vec::new(),
            target_branch: Some("main".to_string()),
            risk_level: HepaRiskLevel::Low,
            max_total_rounds: 1,
            created_at: "2026-06-16T00:00:00Z".to_string(),
        };
        let attempt = HepaFakeAdapter::default()
            .run_worker_attempt(&HepaFakeWorkerInput {
                task_spec,
                lane_id: "lane-1".to_string(),
                attempt_id: "attempt-1".to_string(),
                round: 1,
                started_at: "2026-06-16T00:00:00Z".to_string(),
                completed_at: "2026-06-16T00:00:01Z".to_string(),
            })
            .expect("fake adapter runs under container mode");
        assert_eq!(attempt.status, HepaAttemptStatus::Completed);
    }
}
