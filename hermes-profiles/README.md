# HEPA Hermes Profiles

Role-specific Hermes profile templates for Hermes-Pi-Automata. These profiles
follow the HOCA/Hawker manager-worker-reviewer souls while adapting execution to
HEPA's Rust contracts, Hermes Kanban state, manager-owned Git lifecycle, and Pi
Coding Agent default worker adapter.

| Profile | Role |
| --- | --- |
| `hepa-manager` | Engineering manager: task clarity, safety policy, arbitration, human-friendly PR body ownership, Git/PR lifecycle |
| `hepa-worker` | Principal engineer: scoped HEPA run briefs and repair briefs; Pi or another adapter performs code edits |
| `hepa-reviewer` | Principal reviewer: QA/security/release-quality review artifact producer |
| `hepa-review-manager` | Review arbiter: settles multi-reviewer disagreements and accepted/downgraded/rejected findings |

Hermes profiles provide identity and defaults; they are not security sandboxes.
HEPA still enforces env allowlists, deterministic monitoring, safe staging,
manager-only credentials, bounded rounds, and no-auto-merge.
