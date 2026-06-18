# HEPA Security Model

HEPA carries every HOCA safety gate forward unchanged in behavior, and adds an
honest statement of where adapter execution happens.

## Gates carried over (never weakened)

- **Definition of ready** — deterministic checks first; block-with-questions
  beats guessing.
- **Clean-tree requirement** — lanes launch only from a clean repository.
- **Manager-owned Git lifecycle** — only the manager commits, pushes, and opens
  PRs. Worker and reviewer roles are refused at the type level
  (`HepaManagerGitLifecycle`) and by the deterministic monitor.
- **Safe staging** — only an explicit, manager-approved file list is staged via
  `:(literal)` pathspecs. There is no API that stages everything; blind markers
  (`.`, `*`, leading flags/pathspec magic), traversal, runtime artifacts, and
  secret-like paths are refused before any `git add` runs.
- **Secret-path and secret-content rejection** — secret-like filenames,
  suffixes, and credential directories are blocked; secrets are redacted from
  prompts, logs, artifacts, card comments, diagnostics, and PR bodies.
- **Worker/reviewer credential boundaries** — env allowlists are default-deny
  and per-role; `GITHUB_TOKEN` and other manager-only credentials are never
  given to worker/reviewer adapters, even if an adapter declares them.
- **Deterministic output monitor** — blocks command-policy violations, secret
  detection, scope violations, unsafe Git lifecycle attempts, and suspicious
  file paths, mapping each stop to a blocked status with sanitized evidence.
- **Bounded rounds** — repair loops enforce deterministic round/attempt caps.
- **No auto-merge by default**.
- **Full audit trail outside the target repo**.

## Host-execution posture (stated honestly)

CLI agents run on the host by default for speed. Where execution happens depends
on the adapter's declared sandbox and the project's trust:

| Project trust | Adapter sandbox | Active posture |
| --- | --- | --- |
| Trusted | none | host worktree |
| Trusted | adapter-native | adapter-native sandbox (preferred) |
| Trusted | container | container |
| Untrusted | any | container (always) |

The active sandbox posture is recorded in every run's timing artifacts, surfaced
in the PR body, and projected onto the Hermes card.

## Pi division of responsibility

The Pi Coding Agent declares `sandbox=none`: Pi edits files and runs commands
with the permissions of the Pi process. HEPA therefore owns confinement around
Pi runs:

- disposable lane worktrees scoped to the target repository;
- per-role env allowlists that pass provider keys such as `DEEPSEEK_API_KEY`
  while withholding manager credentials;
- deterministic monitoring of commands, stderr/stdout, secret-like output, and
  suspicious paths;
- container mode for untrusted projects.

HEPA composes
`pi --no-approve --no-session --no-extensions --no-skills --no-prompt-templates --no-context-files -p --mode json --model ...`
without `--dangerously-*`, `--yolo`, `--no-sandbox`, or other unrestricted
host-bypass flags. The explicit `--no-approve` posture prevents
non-interactive Pi runs from loading project-local Pi resources outside HEPA's
adapter policy, and `--no-session` keeps HEPA's lane artifact as the single
persistent transcript. Pi is the default harness, not a hard dependency; non-Pi
adapters keep the same safety boundaries.

## Mitigations

- Worktree confinement (repository-scoped lanes, disposable worktrees).
- Per-role, per-adapter env allowlists (default-deny).
- Deterministic monitor policy and hard blockers.
- No worker/reviewer credentials.
- Preferred adapter-native sandboxing when declared.

## Container mode

Container mode is first-class for untrusted projects. HEPA composes a
`docker run` invocation confined to the worktree with no network and **never**
includes a host permission-bypass flag (`--privileged`, `--no-sandbox`,
`--dangerously-skip-permissions`, etc.). No built-in adapter command uses a
bypass flag.

## Local-only policy

Cloud egress is an adapter property. `local-only` routing restricts execution to
local adapters, and the resource governor budgets paid-cloud lanes separately
from local lanes. Pi local-provider routes such as exo + Apple MLX on a loopback
OpenAI-compatible endpoint require no provider-key environment entries and run
under the same worktree confinement, env allowlists, and deterministic monitor.
Built-in local CLI templates such as `local-worker`, `aider-local`, and
`opencode-local` also advertise `local-only` and keep the same safety boundary.

## Version pinning and drift defense

Known-good invocation templates are pinned per adapter version. `hepa doctor`
warns on untested versions, drift from a pinned template is detected, and adapter
output parse failures are classified explicitly rather than silently misparsed.
