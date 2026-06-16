# HOCA Parity Notes

HEPA uses HOCA as a read-only behavioral reference. Runtime implementation is
Rust-first and uses HEPA naming in public contracts, APIs, files, and CLI output.

## Intentional Naming Changes

| HOCA reference name | HEPA v1.0.0 name |
| --- | --- |
| `HocaTaskSpec` | `HepaTaskSpec` |
| `HocaFleetTask` | `HepaFleetTask` |
| `HocaLane` | `HepaLane` |
| `HocaLaneLease` | `HepaLaneLease` |
| `HocaAttemptReport` | `HepaAttemptReport` |
| `HocaValidationReport` | `HepaValidationReport` |
| `HocaReviewReport` | `HepaReviewReport` |
| `HocaReviewFinding` | `HepaReviewFinding` |
| `HocaReviewSignal` | `HepaReviewSignal` |
| `HocaManagerDecision` | `HepaManagerDecision` |
| `HocaMergeReadiness` | `HepaMergeReadiness` |
| `HocaRunFinalState` | `HepaRunFinalState` |
| `HocaNotification` | `HepaNotification` |
| `HocaProjectMemoryEntry` | `HepaProjectMemoryEntry` |
| `HocaAgentAdapterSpec` | `HepaAdapterSpec` |
| `HocaAgentSession` | `HepaAgentSession` |
| `HocaResourceBudget` | `HepaResourceBudget` |
| `HocaSchedulerDecision` | `HepaSchedulerDecision` |
| `hoca-*` CLI/script naming | `hepa` CLI subcommands and `hepa-*` internal records |
| `.hoca-runtime` artifacts | HEPA control and archive roots |

These are naming changes only unless a later implementation commit explicitly
records a behavioral divergence.

## Excluded HOCA Runtime Pieces

HEPA v1.0.0 does not port HOCA's OpenHands wrapper scripts or per-phase Docker
scripts as runtime dependencies. The equivalent HEPA behavior is implemented
through provider-neutral adapter contracts, scoped worktrees, role-scoped
environment allowlists, deterministic monitoring, and optional adapter sandbox
postures.

Container mode remains a HEPA adapter posture for untrusted projects, but HEPA
does not depend on HOCA's OpenHands execution path.
