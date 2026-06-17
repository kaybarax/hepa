# HEPA Hermes Kanban Guide

Hermes Kanban/dashboard is HEPA's default operator surface, but HEPA's
deterministic registry, lane records, artifacts, and state machine remain
authoritative. Board actions are transition **requests**, not state truth — HEPA
validates each transition before applying it.

## Board setup

Configure Hermes access and select a workspace and board. Check connectivity:

```bash
hepa kanban doctor
```

When Hermes is unavailable the doctor reports a degraded status with actionable
remediations (install CLI/API, configure access, authenticate, select
workspace/board). CLI and headless operation continue regardless.

## Spec import

Import a markdown spec to create draft tasks and Hermes cards:

```bash
hepa spec import path/to/spec.md
```

Imported tasks stay draft and not-ready until readiness passes. Each card carries
the task id, dependencies, lane states, acceptance criteria, validation commands,
risk, timing counters, sandbox postures, and arbitration/repair status.

## Card transitions

As a task progresses, its lane state advances and the card status is projected
from the authoritative lane state. A card cannot show done unless the HEPA done
gate passes — board requests to mark a card done are rejected while readiness
fails.

```bash
hepa task sync-kanban   # push task records to Hermes (degrades if unavailable)
hepa kanban sync        # reconcile cards
```

## Drift recovery

The fleet monitor detects card drift — any lane whose board status contradicts
the authoritative lane state — and reconcile emits repair actions for stale
leases, missing cards, orphaned worktrees, and terminal lanes:

```bash
hepa fleet reconcile
```

## Headless fallback

If Hermes is unavailable, board sync degrades and catches up later rather than
blocking local operation. All board payloads, comments, and diagnostics pass the
same redaction and privacy rules as run artifacts and PR bodies.
