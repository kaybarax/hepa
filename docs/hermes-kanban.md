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

## GitHub issue webhooks

GitHub `issues` webhook payloads can be converted into the same draft task
records as markdown specs:

```bash
hepa github issue-webhook payload.json \
  --project project-1 \
  --delivery 00000000-0000-0000-0000-000000000000 \
  --event issues \
  --signature-256 sha256=<digest> \
  --secret-env HEPA_GITHUB_WEBHOOK_SECRET
```

When `--secret-env` is set, HEPA verifies `X-Hub-Signature-256` with HMAC-SHA256
before parsing the payload. Issue bodies may include `Acceptance:`,
`Validation:`, `Dependencies:`, and `Questions:` sections; missing acceptance
criteria creates a blocked draft task with a clarification question. Pull
request mirror payloads and non-task issue actions are ignored. GitHub labels
can set HEPA metadata such as `hepa:priority=7`, `hepa:risk=medium`, and
`hepa:area=crates/hepa-kanban`, but labels never bypass definition-of-ready.

## Hermes profile orchestration

Hermes-led runs use bundled profiles as the engineering brain:

| Profile | Owns |
| --- | --- |
| `hepa-manager` | project/task intake, Kanban population, assignment, review mediation, bounded retries, and project-specific PR intent |
| `hepa-worker` | task breakdown, finite HEPA run briefs, and repair briefs |
| `hepa-reviewer` | QA/review artifacts from task brief, validation output, and git diff |
| `hepa-review-manager` | arbitration when multiple reviewers disagree or findings need manager judgment |

The default coding path is Hermes manager/worker orchestration plus the Pi
coding adapter. Pi performs code implementation only in this path; review is
owned by Hermes reviewer profiles. HEPA validates each profile output before it
changes authoritative state.

During the runtime transition, a Hermes worker can hand HEPA a finite run brief
by setting `HEPA_HERMES_RUN_BRIEF_FILE` to a JSON `HepaHermesRunBrief` file.
The brief must be authored by `hepa-worker`, match the active task/lane, include
acceptance criteria, and cap the task at one to three rounds. HEPA persists the
consumed brief into the lane artifacts before invoking the coding adapter.

When review passes, the manager profile writes PR intent: title, body, audit
summary, and human-review requirement. HEPA validates that the intent came from
`hepa-manager`, rejects generic validation-template bodies, appends the HEPA
audit section, and performs the manager-owned GitHub operation. The PR remains
for human review; HEPA does not auto-merge.

Hermes reviewer profiles can hand HEPA review output by setting
`HEPA_HERMES_REVIEW_ARTIFACT_FILE` to a JSON `HepaHermesReviewArtifact` file.
The artifact must be authored by `hepa-reviewer` and contain one or more review
signals labeled with Hermes reviewer profile IDs. HEPA then applies the same
pass policy, finding aggregation, arbitration, and staging gates it uses for
headless review fallback.

During the runtime transition, a Hermes manager can hand HEPA an intent artifact
by setting `HEPA_HERMES_PR_INTENT_FILE` to a JSON `HepaHermesPrIntent` file.
When this variable is present, live PR creation uses that validated intent; when
it is absent, HEPA keeps the headless fallback body so degraded CLI runs still
work.

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
blocking local operation. Headless PR bodies are fallback evidence artifacts; a
Hermes-present release run must use manager-authored PR intent for
project-specific PR content. All board payloads, comments, and diagnostics pass
the same redaction and privacy rules as run artifacts and PR bodies.

For desktop review during degraded Hermes access, package the local fleet state
as a static dashboard snapshot:

```bash
hepa fleet dashboard --output .hepa/dashboard/index.html
```

The generated HTML and sibling JSON are read-only views over HEPA registry and
scheduler state, including project/task counts, task statuses, priorities,
lanes, scheduler run state, and dashboard-visible wait reasons.
