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
loops, manager passes, worker-profile LLM calls, reviewer passes, install
events, and container count, plus the active sandbox posture.

## Pi runs

Pi is the default one-loop harness. HEPA invokes
`pi --no-approve --no-session --no-extensions --no-skills --no-prompt-templates --no-context-files -p --mode json --model ...`
once per worker or reviewer attempt, feeds the prompt on stdin, and captures the
JSON event stream from stdout into the lane artifact. DeepSeek and other cloud
routes count as paid-cloud lanes; Ollama/loopback/no-key routes count as local
lanes and can satisfy `local-only` projects. Container count remains zero for
trusted host-worktree runs and becomes one only when container mode is required,
such as untrusted projects.

## Live matrix evidence

The 2026-06-18 release stress matrix exercised HEPA's Pi adapter across multiple
validation repositories and multiple concurrent tasks from initial task dispatch
through manager-owned PR creation, review, evidence capture, and cleanup.

| Configuration | Repos/jobs | Max concurrency | Result | Wall time | Max RSS | Peak footprint | PR lifecycle |
| --- | ---: | ---: | --- | ---: | ---: | ---: | --- |
| Pi + DeepSeek worker/reviewer | 3 | 3 | 3 succeeded / 0 failed | 24.43 s | 191.8 MiB | 15.5 MiB | PRs opened, then closed/cleaned |
| Pi + local Qwen worker/reviewer | 2 | 2 | 2 succeeded / 0 failed | 928.41 s | 177.1 MiB | 3.2 MiB | PRs opened, then closed/cleaned |
| Pi + local Qwen worker / DeepSeek reviewer | 2 | 2 | 2 succeeded / 0 failed | 1209.18 s | 191.4 MiB | 7.1 MiB | PRs opened, then closed/cleaned |

Wall time is elapsed clock time for the whole fleet run, not the sum of
per-lane durations. Because lanes run concurrently, it represents what an
operator waits while HEPA schedules, executes, validates, reviews, stages, opens
PRs, records evidence, and reaches terminal state for the fleet batch.

Interpretation:

- DeepSeek-only completed three parallel validation jobs across the three repo
  shapes in under one minute for the tested workload.
- Local Qwen required serialized local Pi model generation after earlier
  timeout/stall evidence, then passed with two active lanes.
- The hybrid route proved local-worker/cloud-reviewer operation, but it was
  slower than local-only in this sample because it combined local worker latency
  with cloud review.
- Local Qwen used the same local-provider class as Ollama / LM Studio / vLLM;
  literal Ollama runtime coverage is represented by config, routing, and docs
  tests rather than this live daemon.

These numbers are release evidence for the tested validation tasks and
environment. Larger changes, slow dependency installs, long test suites, CI
waiting, provider rate limits, or stricter review policies can increase wall
time.

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
worker-profile calls on the happy path, bounded manager passes, install skip on
unchanged lockfile, no container starts in default mode) so a regression that
reintroduces overhead fails CI.

Timing trend reports scan archived run artifacts under `archive:runs/...`, read
each lane `timing.json`, validate the timing schema, and report portable archive
refs rather than local filesystem paths. The report includes per-run total
duration and loop counters plus median totals, loop counts, reviewer passes,
container counts, and per-phase min/median/max samples.
