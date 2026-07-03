# HEPA Performance

HEPA's performance story is the one-loop model: a single agent loop per attempt,
no default containers, and at most two short manager passes on the happy path.

## One-loop model

| Metric | HOCA reference | HEPA target |
| --- | --- | --- |
| Agent loops per attempt | 2 | 1 |
| Containers per round | 2 default | 0 default; container mode opt-in |
| Dependency installs per round | up to 3 | 0–1 with shared cache |
| Orchestration overhead | full wrapper sessions | at most 2 short manager passes |
| Small task, capable adapter, idea→PR | tens of minutes | under 10 minutes, overhead < 10% |
| Human notifications per task | per-run/noisy | exactly 1 terminal done/block |
| Board observability | optional bridge | default Hermes card/dashboard |

Every run records timing telemetry: per-phase durations and counters for agent
loops, manager passes, worker-profile calls inside the adapter attempt loop,
reviewer passes, install events, and container count, plus the active sandbox
posture. Hermes Worker run briefs are pre-run artifacts and should not appear as
hidden per-attempt wrappers in this counter.

## Pi runs

Pi is the default one-loop harness. HEPA invokes
`pi --no-approve --no-session --no-extensions --no-skills --no-prompt-templates --no-context-files -p --mode json --model ...`
once per worker or reviewer attempt, feeds the prompt on stdin, and captures the
JSON event stream from stdout into the lane artifact. Cloud model routes count
as paid-cloud lanes; tool-call-capable loopback endpoints such as
llama.cpp, Ollama, vLLM, and no-key routes count as local lanes and can satisfy
`local-only` projects once they pass the Pi tool-call readiness gate. Container
count remains zero for trusted host-worktree runs and becomes one only when
container mode is required, such as untrusted projects.

## Live matrix evidence

The v1.0.0 release stress matrix exercised HEPA's Pi adapter through Hermes
Kanban task intake, concurrent scheduling, manager-owned staging, PR creation,
review evidence capture, and cleanup. Validation repository names, local
machine paths, branch names, and operator identifiers are intentionally omitted.

| Configuration | Scope | Max concurrency | Result | Wall time | Max RSS | Peak footprint | PR lifecycle |
| --- | ---: | ---: | --- | ---: | ---: | ---: | --- |
| Pi + configured cloud worker/reviewer | 3 jobs | 3 | 3 succeeded / 0 failed | 24.43 s | 191.8 MiB | 15.5 MiB | PRs opened, then closed/cleaned |
| Pi + configured cloud worker/reviewer, Hermes-present | 3 jobs | 3 | 2 succeeded / 1 safety-blocked | 97.97 s | 321.3 MiB | 52.2 MiB | PRs opened for successful lanes, then closed/cleaned |
| Pi + configured cloud worker/reviewer, multi-project/multi-task | 2 repos / 4 jobs | 4 | 4 succeeded / 0 failed | 19.54 s | 196.1 MiB | 14.7 MiB | PRs opened, then closed/cleaned |

Wall time is elapsed clock time for the whole fleet run, not the sum of
per-lane durations. Because lanes run concurrently, it represents what an
operator waits while HEPA schedules, executes, validates, reviews, stages, opens
PRs, records evidence, and reaches terminal state for the fleet batch.

Interpretation:

- The configured cloud route is the v1.0.0 release-gated route. It completed the
  multi-job cloud matrix and the Hermes-present multi-project/multi-task stress
  run, with one safety-blocked lane correctly stopped by the deterministic
  secret monitor.
- Each successful lane recorded one worker adapter loop, one manager pass, one
  Hermes reviewer pass, zero containers in trusted host-worktree mode,
  manager-owned staging/commit/PR creation, and validation cleanup.
- HEPA preserves the user's configured Git identity for manager-owned commits;
  it does not set a product-specific author or committer identity.
- Local and hybrid Pi routes remain supported by the adapter contract, routing
  policy, doctor checks, live stdout/stderr capture, and deterministic blocked
  final reports. Heavy local-model-only stress is deferred to post-v1.0.0
  hardening because release testing showed some loopback endpoints can complete
  without reliable tool calls or changed files.
- exo can expose local OpenAI/Ollama-compatible APIs backed by Apple MLX, but a
  generic exo/MLX endpoint is not treated as release-grade Pi evidence until it
  proves the OpenAI tool-call contract: `tools`, `tool_choice`, assistant
  `tool_calls`, and tool-result messages. Weak local providers fail preflight or
  attempt checks with evidence instead of hanging silently.
- Max RSS and peak footprint are measured for the HEPA manager process. External
  model-serving memory and compute for local endpoints live outside that process
  and should be measured separately when sizing a local deployment.

These numbers are release evidence for the tested validation tasks and
environment. Larger changes, slow dependency installs, long test suites, CI
waiting, provider rate limits, weaker local providers, or stricter review
policies can increase wall time.

## Targets

Validated against the Phase 0.4 HOCA reference baseline on the same task and
hardware. Every performance claim requires benchmark evidence — never memory or
estimates.

## Benchmark harness usage

```bash
# Summarize a single run's timing record.
hepa bench --timing path/to/timing.json

# Summarize timing trends across archived runs.
hepa timing trends .hepa/archive
```

The benchmark harness reads timing artifacts, aggregates medians, and compares
against the HOCA reference baselines (Phase 10). Structural performance budget
tests assert the one-loop invariants (zero per-attempt wrapper spawns, zero
worker-profile wrappers inside coding attempts, bounded manager passes, install
skip on unchanged lockfile, no container starts in default mode) so a regression
that reintroduces hidden overhead fails CI.

Timing trend reports scan archived run artifacts under `archive:runs/...`, read
each lane `timing.json`, validate the timing schema, and report portable archive
refs rather than local filesystem paths. The report includes per-run total
duration and loop counters plus median totals, loop counts, reviewer passes,
container counts, and per-phase min/median/max samples.
