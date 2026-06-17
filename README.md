# HEPA

HEPA is an independent, Rust-first sibling of HOCA: a Hermes-coordinated,
agent-agnostic engineering automation system. HOCA is used as a **read-only
behavioral reference and parity-test source**, never as a runtime dependency.

Hermes Kanban/dashboard is the default operator surface for HEPA v1.0.0, while
HEPA's deterministic registry, lane records, artifacts, and state machine remain
authoritative. CLI and headless operation keep working when Hermes is
unavailable — board sync degrades and catches up rather than blocking.

## Relationship to HOCA

| Aspect | HOCA | HEPA |
| --- | --- | --- |
| Implementation | Python + OpenHands | Rust-first, OpenHands dropped |
| Coding agents | OpenHands wrappers | agent-agnostic adapter contract |
| Per-attempt loops | 2 agent loops | 1 agent loop per attempt |
| Containers | 2 per round default | 0 default; container mode opt-in |
| Operator surface | optional bridge | default Hermes Kanban/dashboard |

HEPA carries every HOCA safety gate forward unchanged in behavior. Divergences
are recorded in commit messages or these docs.

## Rust Workspace

```text
crates/
  hepa-core/       contracts, config, fleet registry, scheduler, governor,
                   conflict planner, fleet monitor, readiness/done gate,
                   notifications, monitor, env allowlist, redaction, hard
                   blockers
  hepa-adapters/   adapter spec/contract, built-ins, routing, engine, doctor,
                   sandbox/container mode, version pinning
  hepa-git/        worktrees, safe staging, manager-owned commit/PR lifecycle
  hepa-review/     review fanout, parser, arbitration, Ralph-V2 repair
  hepa-kanban/     Hermes card mapping, board sync, transitions, spec import
  hepa-memory/     per-project context packs, learning, reward signals
  hepa-cli/        the `hepa` command surface
```

## Quickstart

```bash
# Build and run the local gate (tests + fmt + clippy).
bin/hepa-check

# Import a spec into tasks/cards.
hepa spec import path/to/spec.md

# Register a project and create a task.
hepa project add app-one /path/to/repo --name "App One"
hepa task create app-one task-1 "Fix login redirect"

# Inspect fleet and scheduler state.
hepa scheduler start
hepa scheduler status
hepa fleet status

# Run one task with the fake adapter (safe defaults).
hepa run /path/to/repo "Fix login redirect" --agent fake

# Inspect adapters and overall health.
hepa adapter list
hepa doctor
```

Fleet commands accept `--control-root <path>` to target an isolated control
root (used throughout the test suite).

## Hermes Kanban Workflow

Import a spec to create draft cards, let the scheduler claim ready tasks into
lanes, and watch board state stay reconciled with HEPA's authoritative lane
state. Board actions are transition *requests*; HEPA validates each before
changing authoritative state. See [docs/hermes-kanban.md](docs/hermes-kanban.md).

## Adapter Setup and Routing

All execution and review route through the adapter contract — no feature
hard-requires a specific vendor CLI. Built-in adapters, custom adapter
requirements, version pinning, and `hepa adapter doctor` troubleshooting are
documented in [docs/adapters.md](docs/adapters.md).

## Safety

HEPA never weakens its safety gates: definition-of-ready, safe staging,
secret-path rejection, manager-owned Git lifecycle, worker/reviewer credential
boundaries, env allowlists, the deterministic monitor, bounded rounds, and
default no-auto-merge. See [docs/security-model.md](docs/security-model.md).

## Fleet Usage

The fleet layer schedules tasks across projects under capacity, cost, adapter,
and conflict constraints, and reconciles drift. See [docs/fleet.md](docs/fleet.md)
and [docs/performance.md](docs/performance.md).

## Development Checks

```bash
bin/hepa-check
```
