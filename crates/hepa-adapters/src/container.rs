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
}
