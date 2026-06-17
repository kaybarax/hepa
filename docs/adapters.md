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
deterministic.

DeepSeek cloud:

```bash
export HEPA_DEFAULT_ADAPTER=pi
export HEPA_PI_MODEL=deepseek/deepseek-chat
export HEPA_PI_PROVIDER_KEY_ENV=DEEPSEEK_API_KEY
export DEEPSEEK_API_KEY=...
```

Ollama local:

```bash
export HEPA_DEFAULT_ADAPTER=pi
export HEPA_PI_MODEL=ollama/qwen2.5-coder
export HEPA_PI_PROVIDER_KEY_ENV=
export HEPA_PI_BASE_URL=http://127.0.0.1:11434/v1
```

Cost class is derived from the model/base URL/key surface: Ollama/loopback/no-key
routes are local, while remote provider routes with keys are paid-cloud. The
existing `local-only` routing policy and paid-lane caps enforce the result.

Pi output is newline-delimited JSON events. HEPA parses `agent_end` for the final
assistant message and tool activity; changed files are derived from `git status`
in the lane worktree, not from Pi output. Malformed, truncated, or schema-drifted
streams are explicit parse failures.

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
