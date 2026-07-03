# HEPA Adapter Guide

HEPA is agent-agnostic: all execution and review route through the adapter
contract. No feature hard-requires a specific vendor CLI; privileging one vendor
outside the routing/adapter layer is a defect, not a feature.

## Adapter contract

An adapter spec (`HepaAdapterSpec`) declares:

- `id`, `display_name`, `roles` (worker and/or reviewer)
- `mode` (oneshot or interactive)
- `command` / `review_command` invocation templates with placeholders such as
  `{prompt_file}`, `{worktree}`, `{artifact_dir}`, `{output_file}`
- `prompt_transport` (`prompt-file` or `stdin`) and `output_capture`
  (`adapter-file` or `stdout`)
- `workdir`, `required_commands`, `required_env`
- `sandbox` (none, agent-native, container)
- `supports_resume`, `supports_json_output`
- `capabilities`, `cost_class`, `resource_weight`, `max_concurrency`

Specs are validated and reject manager-only `required_env` entries and invalid
placeholders. Provider key names such as `DEEPSEEK_API_KEY` are allowed as
adapter env allowlist entries; manager credentials such as `GITHUB_TOKEN` are
not.

## Built-in adapters

| Id | Roles | Sandbox | Notes |
| --- | --- | --- | --- |
| `pi` | worker, reviewer | none | default Pi Coding Agent harness; prompt on stdin, JSON events on stdout |
| `fake` | worker, reviewer | none | deterministic, used for fixtures |
| `shell-command` | worker | agent-native | shell command adapter |
| `custom` | worker, reviewer | agent-native | user-defined template |
| `user-worker` | worker | agent-native | user worker adapter |
| `user-reviewer` | reviewer | agent-native | user reviewer adapter |
| `local-worker` | worker, reviewer | agent-native | local model worker |
| `aider-local` | worker, reviewer | agent-native | local CLI harness wrapper |
| `opencode-local` | worker, reviewer | agent-native | local CLI harness wrapper |
| `external-worker` | worker | none | external status worker |

List them with:

```bash
hepa adapter list
```

## Pi adapter

Pi is the built-in default harness and namesake, not a hard requirement. It
routes through the same adapter contract as every other adapter.

```bash
hepa adapter install pi
hepa adapter doctor
```

The installer is explicit and version-pinned; HEPA never silently installs Pi
from doctor. The built-in spec composes
`pi --no-approve --no-session --no-extensions --no-skills --no-prompt-templates --no-context-files -p --mode json --model ...`,
feeds the prompt on stdin, captures stdout to the lane output artifact, and
keeps stderr under the deterministic monitor. `--no-approve` keeps
non-interactive Pi runs from trusting project-local Pi settings, packages,
skills, or extensions unless HEPA adds an explicit adapter policy for them.
`--no-session` avoids a second persistent transcript outside HEPA's lane
artifacts, while the `--no-*` discovery flags keep the adapter surface
deterministic. While an adapter runs, stdout and stderr chunks are also appended
to `streams/worker-adapter-stream.jsonl` or
`streams/reviewer-adapter-stream.jsonl` under the lane artifact directory so
parallel lanes have tail-able live logs.

DeepSeek cloud:

```bash
export HEPA_DEFAULT_ADAPTER=pi
export HEPA_PI_MODEL=deepseek/deepseek-chat
export HEPA_PI_REVIEW_MODEL=
export HEPA_PI_PROVIDER_KEY_ENV=DEEPSEEK_API_KEY
export HEPA_PI_BASE_URL=
export DEEPSEEK_API_KEY=...
```

Empty Pi environment values deliberately clear optional `.env` settings. Use
that when switching from a loopback local profile back to a cloud profile so
`hepa adapter doctor` does not keep requiring the stale local base URL.

Local Pi route:

Pi local routes must expose OpenAI-compatible chat completions plus reliable
tool-call semantics. The recommended path is llama.cpp with chat-template and
tool-call support enabled:

```bash
llama-server -m /path/to/model.gguf --host 127.0.0.1 --port 8080 --ctx-size 8192 --jinja

export HEPA_DEFAULT_ADAPTER=pi
export HEPA_PI_MODEL=llama-cpp/<model-id>
export HEPA_PI_PROVIDER_KEY_ENV=
export HEPA_PI_BASE_URL=http://127.0.0.1:8080/v1
```

Known-weak or unverified local endpoints:

exo + Apple MLX can expose local OpenAI-compatible endpoints, but HEPA does not
treat the generic `local/mlx-community/...` route as release-ready unless that
endpoint proves support for `tools`, `tool_choice`, assistant `tool_calls`, and
tool-result messages. `hepa doctor` and live Pi preflight block this route with
an actionable diagnostic instead of letting a heavy run stall or complete with
no repository edits.

Ollama-compatible local:

```bash
export HEPA_DEFAULT_ADAPTER=pi
export HEPA_PI_MODEL=ollama/qwen2.5-coder
export HEPA_PI_PROVIDER_KEY_ENV=
export HEPA_PI_BASE_URL=http://127.0.0.1:11434/v1
```

Cost class is derived from the model/base URL/key surface: llama.cpp,
Ollama/loopback/no-key routes are local, while remote provider routes with keys
are paid-cloud. A local route must also pass the Pi tool-call readiness gate
before it is allowed into release stress runs. The existing `local-only` routing
policy and paid-lane caps enforce the result.

Hybrid Pi runs can use a local worker model and a cloud reviewer model by
setting `HEPA_PI_MODEL` and `HEPA_PI_REVIEW_MODEL` separately. HEPA filters Pi
provider credentials by role: a local worker process does not receive a
reviewer-only cloud key, while the reviewer receives the key required by its
configured provider.

Pi output is newline-delimited JSON events. HEPA parses `agent_end` for the final
assistant message and tool activity; changed files are derived from `git status`
in the lane worktree, not from Pi output. Malformed, truncated, or schema-drifted
streams are explicit parse failures.

Live Pi runs use bounded monitor budgets. Operators may lower
`HEPA_PI_LIVE_TIMEOUT_MS` or `HEPA_PI_LIVE_STALL_MS`, but HEPA clamps live Pi
budgets to the small-task release target so local-provider stalls finish as
blocked diagnostic attempts with `stdout.log`, `stderr.log`, and `attempt.json`
instead of waiting indefinitely.

## Local CLI adapters

HEPA ships multiple local adapter templates for projects that require a fully
local routing policy:

- `local-worker` is the generic local harness template.
- `aider-local` is a local CLI harness wrapper template for Aider-compatible
  local model workflows.
- `opencode-local` is a local CLI harness wrapper template for OpenCode-style
  local model workflows.

All three declare `cost_class=local`, require no provider-key environment
variables, advertise `local-only`, and use adapter-native sandboxing inside the
lane worktree. They are templates for local coding harnesses, not bare model API
calls, so they still satisfy HEPA's adapter contract: the adapter process owns
one coding loop and emits the normalized JSON artifact HEPA validates. A
`local-only` project can route implementation and review fanout across these
adapters while rejecting paid-cloud routes through the same routing validator.

## Custom adapter requirements

A custom adapter must:

- Provide a single-line invocation template using only supported placeholders.
- Declare any `required_env` keys (never secret-like); worker/reviewer roles
  still never receive manager-only credentials.
- Emit JSON output with the required fields when `supports_json_output` is set —
  parse failures are classified explicitly, never silently misparsed.
- Never request a host permission-bypass flag.

## Design-spec routing

Design-first UI work can use a two-stage routing plan. The manager requests a
`design` capability first; that adapter writes a sanitized HTML/CSS design spec
artifact. HEPA then routes the implementation stage, usually to `frontend`, and
the implementation prompt references the approved design artifact rather than
embedding raw design output in command arguments.

The design artifact is validated as `html-css`: it must include non-empty HTML
and CSS, single-line metadata, and no active script content. The implementation
stage remains an ordinary worker adapter run inside the same safety gates,
monitor, env allowlist, worktree confinement, review, staging, and PR lifecycle.
Pi, fake, custom, user-worker, local-worker, and shell-command may advertise
`design`; projects can still route design and implementation to different
adapters through normal capability routes.

## External mode status reporting

`external` mode adapters are for work that runs somewhere HEPA does not own,
such as an external queue or another service. They do not execute the normal
implementation loop and they never own Git lifecycle. HEPA polls their
configured status command, writes a prompt file describing the lane/task, and
requires a JSON status artifact:

```json
{
  "schema_version": 1,
  "adapter_id": "external-worker",
  "external_ref": "queue-item-42",
  "lane_id": "lane-1",
  "status": "running",
  "summary": ["External worker is still running."],
  "updated_at": "2026-06-18T00:00:00Z"
}
```

Allowed statuses are `queued`, `running`, `completed`, `blocked`, and `failed`.
The report is validated for schema, single-line fields, and sensitive-reference
redaction before HEPA treats it as status evidence. The deterministic monitor
still blocks unsafe command templates, secret output, and adapter attempts to
run manager-owned Git lifecycle commands. Hermes cards may display the external
status, but HEPA's lane state remains authoritative.

## Per-lane cost accounting

Adapters that expose model usage may include a `usage` object in their JSON
output. HEPA treats this as optional evidence: absence of usage never blocks an
adapter run, but malformed usage fails loudly for adapters that choose to emit
it.

```json
{
  "status": "completed",
  "usage": {
    "input_tokens": 120,
    "output_tokens": 30,
    "total_tokens": 150,
    "cost_micros": 4200,
    "currency": "USD"
  }
}
```

HEPA normalizes reported usage into a lane `cost.json` artifact with one entry
per adapter invocation, summed token totals, summed micro-unit currency totals,
and an `entries_without_cost` count for local or usage-only routes. Cost class
comes from the adapter spec (`local`, `free-tier`, or `paid-cloud`) rather than
from adapter output, so local-only routing and paid-lane caps stay authoritative.
Provider keys and account identifiers are never accepted in cost fields.

## Version pinning

Known-good invocation templates are pinned per adapter version
(`HepaVersionPinRegistry`). Built-ins are pinned at the baseline version. Custom
pins can be registered for specific versions. Unknown versions are reported as
unpinned, and drift from a pinned template is flagged.

## Doctor troubleshooting

```bash
hepa adapter doctor   # per-adapter availability and auth
hepa doctor           # aggregate adapter + kanban health
```

- **Untested version** — pin a known-good invocation template before relying on
  the adapter.
- **Flag drift** — the actual invocation diverged from its pinned template;
  re-pin and re-validate.
- **Parse failure** — adapter output was not valid JSON or was missing a
  required field (schema drift); fix the adapter output.
- **Unavailable/unauthenticated** — install the CLI or complete authentication;
  adapter-specific routes get documented skips, not silent passes.

CI uses deterministic fake `pi` binaries that emit canned `--mode json` events;
no real network, model, or install is required for tests.
