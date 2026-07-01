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

Post-hardening Hermes-present local-route rerun, after Pi stdin transport,
stdout/stderr capture, partial-output retention, and live Pi monitor clamping:

| Configuration | Repos/jobs | Max concurrency | Result | Wall time | Max RSS | Peak footprint | PR lifecycle |
| --- | ---: | ---: | --- | ---: | ---: | ---: | --- |
| Pi + local Qwen worker/reviewer via exo + Apple MLX | 2 | 2 | interrupted after one zero-output local response and one non-terminal lane | 1191.03 s | 171.2 MiB | 1.9 MiB | no PRs opened; worktrees cleaned |
| Pi + local Qwen worker via exo + Apple MLX / DeepSeek reviewer | 2 | 2 | interrupted after local worker stage failed to produce terminal attempt results | 619.70 s | 173.0 MiB | 1.8 MiB | no PRs opened; worktrees cleaned |

Fixed-local-serving rerun with the supplied exo + Apple MLX Qwen endpoint
(`HEPA_PI_BASE_URL=<LOCAL_LOOPBACK_OPENAI_V1>`, local model path redacted):

| Configuration | Repos/jobs | Max concurrency | Result | Wall time | Max RSS | Peak footprint | PR lifecycle |
| --- | ---: | ---: | --- | ---: | ---: | ---: | --- |
| Pi + local Qwen worker/reviewer via exo + Apple MLX | 2 | 2 | 1 succeeded / 1 blocked | 210.23 s | 198.7 MiB | 10.6 MiB | docs PR opened, then closed/cleaned |
| Pi + local Qwen worker via exo + Apple MLX / DeepSeek reviewer | 2 | 2 | 1 succeeded / 1 blocked | 212.67 s | 191.5 MiB | 9.9 MiB | docs PR opened, then closed/cleaned |

Replacement-local-model rerun with Devstral Small 2 24B Q4_K_M served by
llama.cpp (`HEPA_PI_MODEL=llama-cpp/<DEVSTRAL_24B_GGUF>`,
`HEPA_PI_BASE_URL=http://127.0.0.1:8080/v1`, local model id redacted):

| Configuration | Repos/jobs | Max concurrency | Result | Wall time | Max RSS | Peak footprint | External model RSS | PR lifecycle |
| --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | --- |
| Pi + Devstral via llama.cpp, ctx4096 | 2 | 2 | 1 succeeded / 1 blocked | 89.82 s | 198.0 MiB | 8.6 MiB | ~20.6 GiB peak observed | docs PR opened, then closed/cleaned |
| Pi + Devstral via llama.cpp, ctx8192 | 2 | 2 | 1 succeeded / 1 blocked | 137.70 s | 202.5 MiB | 10.3 MiB | ~24.8 GiB peak observed | docs PR opened, then closed/cleaned |
| Pi + Devstral via llama.cpp, ctx16384 | 2 | 2 | 1 succeeded / 1 blocked | 300.19 s | 650.0 MiB | 29.9 MiB | ~20.1 GiB peak observed | docs PR opened, then closed/cleaned |

Final Devstral/llama.cpp reruns after local-provider hardening, manager-owned
Yarn validation, Pi role-scoped credential filtering, and unique live-matrix
lane IDs:

| Configuration | Repos/jobs | Max concurrency | Result | Wall time | Max RSS | Peak footprint | External model RSS | PR lifecycle |
| --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | --- |
| Pi + Devstral worker/reviewer via llama.cpp, ctx16384 | 2 | 2 | 2 succeeded / 0 failed | 207.11 s | 462.1 MiB | 19.6 MiB | ~18.6 GiB stable observed | PRs opened, then closed/cleaned |
| Pi + Devstral worker via llama.cpp, ctx16384 / DeepSeek reviewer | 2 | 2 | 2 succeeded / 0 failed | 175.48 s | 461.8 MiB | 19.5 MiB | ~17.4-20.0 GiB stable observed | PRs opened, then closed/cleaned |

Fresh Hermes-present runtime-command rerun after the bundled profile bridges
were wired into worker brief, review, review-manager arbitration, and manager
PR intent:

| Configuration | Repos/jobs | Max concurrency | Result | Wall time | Max RSS | Peak footprint | External model RSS | PR lifecycle |
| --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | --- |
| Pi + Devstral 24B via Apple MLX loopback worker/reviewer | 2 | 2 | 0 succeeded / 2 blocked | 24.93 s | 234.1 MiB | 2.7 MiB | ~6.6 GiB stable observed | no PRs opened; worktrees cleaned |
| Pi + Devstral 24B via Apple MLX loopback worker / DeepSeek reviewer configured | 2 | 2 | 0 succeeded / 2 blocked | 24.80 s | 234.2 MiB | 2.7 MiB | ~6.6 GiB stable observed | no PRs opened; worktrees cleaned |

Final Hermes-required runtime-command reruns with GPT-OSS 20B served by
llama.cpp (`HEPA_HERMES_REQUIRED=true`, local model id redacted):

| Configuration | Repos/jobs | Max concurrency | Result | Wall time | Max RSS | Peak footprint | External model RSS | PR lifecycle |
| --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | --- |
| Pi + GPT-OSS 20B worker/reviewer via llama.cpp, ctx8192 | 2 | 2 | 2 succeeded / 0 failed | 74.82 s | 323.9 MiB | 23.5 MiB | ~12.0 GiB peak observed | PRs opened, then closed/cleaned |
| Pi + GPT-OSS 20B worker via llama.cpp, ctx8192 / DeepSeek reviewer | 2 | 2 | 2 succeeded / 0 failed | 75.69 s | 321.9 MiB | 49.6 MiB | ~12.0 GiB peak observed | PRs opened, then closed/cleaned |

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
- The post-hardening local-route rerun improved diagnostics but did not clear
  the release blocker. In both pure-local and hybrid configs, the docs-only lane
  captured `stdout.log`, `stderr.log`, and `attempt.json` for a zero-output
  local Qwen response from the exo/MLX route. The app-starter lane did not reach
  a terminal attempt record before operator interruption. The hybrid run did
  not reach meaningful DeepSeek review because the local worker leg failed
  first.
- The fixed-local-serving rerun proved HEPA's new hardening behavior under the
  same failure class. The docs-only lane completed in both pure-local and
  hybrid configs, opened validation PRs, and those PRs were closed and cleaned.
  The app-starter lane was terminally marked `blocked` in both configs with
  `local_provider_empty_or_malformed_response: Pi output parse failure:
  agent_end missing final assistant message`. The captured stdout shows local
  Qwen repeatedly found files under `src/` and then tried to read them without
  the `src/` prefix, hit `ENOENT`, then the stream ended without
  `finish_reason` and the local server stopped accepting connections. HEPA
  preserved stdout/stderr/attempt/final-report evidence, emitted a fleet
  summary, skipped validation for the blocked lane, closed validation PRs from
  the successful lane, and cleaned validation worktrees.
- The first Devstral/llama.cpp replacement attempts proved the model server was
  memory-stable, but exposed HEPA hardening gaps: local workers could spend
  context on validation, app validation lacked a manager-owned dependency
  install preflight, hybrid Pi workers could receive reviewer-only cloud
  credentials, and live-matrix reruns reused remote branch names. The final
  reruns cleared those gaps. Pure-local Devstral completed the app-starter and
  docs lanes in 207.11 s, with manager-owned `yarn install --frozen-lockfile`,
  `yarn test:e2e`, `yarn build`, and `git diff --check` all passing. Hybrid
  Devstral-worker/DeepSeek-reviewer completed the same lanes in 175.48 s with
  role-scoped credential filtering and unique lane branches. Validation PRs were
  opened only by the manager, then closed and cleaned as validation evidence.
- The fresh Hermes runtime-command rerun after the profile bridges landed did
  not clear the release blocker for the Apple MLX local route. HEPA invoked the
  Hermes worker bridge before Pi, then the local Pi worker completed with a
  final text message but no tool events and no changed files in both validation
  lanes. HEPA now terminalizes that case before validation/review/PR creation as
  `local_provider_no_tool_activity_or_changes: Pi completed without tool calls
  or changed files`. The same worker-leg blocker appears in the hybrid
  configuration before the DeepSeek reviewer leg can matter. During this rerun
  HEPA also fixed an ordering bug where its own `.hepa/` runtime artifacts could
  make an otherwise clean source repo fail lane allocation.
- The final Hermes-required GPT-OSS 20B llama.cpp reruns cleared the
  release-blocking local/hybrid validation item for the approved app-starter and
  docs lanes. Pure-local completed 2/2 jobs in 74.82 s and hybrid local-worker /
  DeepSeek-reviewer completed 2/2 jobs in 75.69 s. Both runs used Hermes worker
  briefs, Hermes reviewer artifacts, review-manager arbitration, and
  manager-authored PR intent before manager-owned staging and PR creation.
  Per-lane timing recorded one worker adapter loop, one manager pass, one
  reviewer pass, zero containers, host-worktree posture, and passed validation
  commands (`yarn install --frozen-lockfile`; `yarn build`; `git diff --check`).
  Validation PRs were closed, branches deleted, runtime dirs removed, and
  validation repos returned to clean main worktrees.
- exo exposes local OpenAI/Ollama-compatible APIs and uses MLX as an inference
  backend, so it exercises the same HEPA local-provider class as other loopback
  local servers while accurately representing the runtime used in this test.
- Max RSS and peak footprint are measured for the HEPA manager process. External
  model-serving memory and compute for exo/MLX or llama.cpp live outside that
  process and should be measured separately when sizing a local deployment.
- Follow-up hardening now sends Pi prompts through stdin, persists per-attempt
  stdout/stderr logs for live adapters, retains partial stdout/stderr on monitor
  stops, clamps live Pi monitor budgets to the small-task release target, bounds
  local Pi generation-permit waits, terminalizes worker/reviewer adapter errors
  with blocked final reports and cleanup, keeps local-provider workers out of
  validation command loops, runs manager-owned Yarn validation with bounded
  timeouts, scopes Pi provider credentials by role, and gives live-matrix reruns
  unique lane/branch IDs. Weak local providers still fail deterministically with
  evidence, while the tested Devstral/llama.cpp local and hybrid routes now pass
  the app-starter and docs validation lanes.

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
worker-profile wrappers inside coding attempts, bounded manager passes, install
skip on unchanged lockfile, no container starts in default mode) so a regression
that reintroduces hidden overhead fails CI.

Timing trend reports scan archived run artifacts under `archive:runs/...`, read
each lane `timing.json`, validate the timing schema, and report portable archive
refs rather than local filesystem paths. The report includes per-run total
duration and loop counters plus median totals, loop counts, reviewer passes,
container counts, and per-phase min/median/max samples.
