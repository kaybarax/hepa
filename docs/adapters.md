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
- `workdir`, `required_commands`, `required_env`
- `sandbox` (none, agent-native, container)
- `supports_resume`, `supports_json_output`
- `capabilities`, `cost_class`, `resource_weight`, `max_concurrency`

Specs are validated and reject secret-like `required_env` and invalid
placeholders.

## Built-in adapters

| Id | Roles | Sandbox | Notes |
| --- | --- | --- | --- |
| `fake` | worker, reviewer | none | deterministic, used for fixtures |
| `shell-command` | worker | agent-native | shell command adapter |
| `custom` | worker, reviewer | agent-native | user-defined template |
| `user-worker` | worker | agent-native | user worker adapter |
| `user-reviewer` | reviewer | agent-native | user reviewer adapter |
| `local-worker` | worker, reviewer | agent-native | local model worker |
| `external-worker` | worker | agent-native | external status worker |

List them with:

```bash
hepa adapter list
```

## Custom adapter requirements

A custom adapter must:

- Provide a single-line invocation template using only supported placeholders.
- Declare any `required_env` keys (never secret-like); worker/reviewer roles
  still never receive manager-only credentials.
- Emit JSON output with the required fields when `supports_json_output` is set —
  parse failures are classified explicitly, never silently misparsed.
- Never request a host permission-bypass flag.

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
