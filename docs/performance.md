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
routes count as paid-cloud lanes; exo + Apple MLX, Ollama, other loopback
endpoints, and no-key routes count as local lanes and can satisfy `local-only`
projects. Container count remains zero for trusted host-worktree runs and
becomes one only when container mode is required, such as untrusted projects.

## Live matrix evidence

The 2026-06-18 release stress matrix exercised HEPA's Pi adapter across multiple
validation repositories and multiple concurrent tasks from initial task dispatch
through manager-owned PR creation, review, evidence capture, and cleanup.

Initial release-gate run:

| Configuration | Repos/jobs | Max concurrency | Result | Wall time | Max RSS | Peak footprint | PR lifecycle |
| --- | ---: | ---: | --- | ---: | ---: | ---: | --- |
| Pi + DeepSeek worker/reviewer | 3 | 3 | 3 succeeded / 0 failed | 24.43 s | 191.8 MiB | 15.5 MiB | PRs opened, then closed/cleaned |
| Pi + local Qwen worker/reviewer via exo + Apple MLX | 2 | 2 | 2 succeeded / 0 failed | 928.41 s | 177.1 MiB | 3.2 MiB | PRs opened, then closed/cleaned |
| Pi + local Qwen worker via exo + Apple MLX / DeepSeek reviewer | 2 | 2 | 2 succeeded / 0 failed | 1209.18 s | 191.4 MiB | 7.1 MiB | PRs opened, then closed/cleaned |

Hermes-present rerun, with Hermes Kanban/dashboard available as the operator
surface and HEPA's deterministic run records remaining authoritative:

| Configuration | Repos/jobs | Max concurrency | Result | Wall time | Max RSS | Peak footprint | PR lifecycle |
| --- | ---: | ---: | --- | ---: | ---: | ---: | --- |
| Pi + DeepSeek worker/reviewer | 3 | 3 | 2 succeeded / 1 safety-blocked | 97.97 s | 321.3 MiB | 52.2 MiB | PRs opened, then closed/cleaned |
| Pi + local Qwen worker/reviewer via exo + Apple MLX | 2 | 2 | timeout / no final summary | 613.89 s | 9.7 MiB | 1.7 MiB | no PRs opened; worktrees cleaned |
| Pi + local Qwen worker via exo + Apple MLX / DeepSeek reviewer | 2 | 2 | timeout / no final summary | 645.54 s | 9.7 MiB | 1.7 MiB | no PRs opened; worktrees cleaned |

Wall time is elapsed clock time for the whole fleet run, not the sum of
per-lane durations. Because lanes run concurrently, it represents what an
operator waits while HEPA schedules, executes, validates, reviews, stages, opens
PRs, records evidence, and reaches terminal state for the fleet batch.

Interpretation:

- DeepSeek-only completed three parallel validation jobs across the three repo
  shapes in under two minutes in the Hermes-present rerun. The third job was
  blocked by the deterministic secret monitor on the validation monorepo's
  existing secret-shaped fixtures, which is a safety-gate success rather than
  an adapter crash.
- Local Qwen was served by exo on Apple MLX through HEPA's `local/...`
  loopback-provider route. The initial release-gate sample eventually passed
  after long local generation, but the Hermes-present rerun exceeded the
  bounded small-task monitor without producing final summaries; this is
  recorded as a local-route operational limitation for the tested hardware and
  model route.
- The hybrid route proved local-worker/cloud-reviewer operation in the initial
  sample, but the Hermes-present rerun also timed out before final summaries
  because it still depends on the local worker leg.
- exo exposes local OpenAI/Ollama-compatible APIs and uses MLX as an inference
  backend, so it exercises the same HEPA local-provider class as other loopback
  local servers while accurately representing the runtime used in this test.
- Max RSS and peak footprint are measured for the HEPA manager process. External
  model-serving memory and compute for exo/MLX live outside that process and
  should be measured separately when sizing a local deployment.
- Follow-up hardening now sends Pi prompts through stdin, persists per-attempt
  stdout/stderr logs for live adapters, retains partial stdout/stderr on monitor
  stops, and clamps live Pi monitor budgets to the small-task release target so
  future local-route stalls become terminal diagnostic artifacts instead of
  silent waits. The local and hybrid routes remain release blockers until a
  Hermes-present rerun completes inside the target.

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
