# HEPA Stretch Phase 11

Phase 11 features are now required release hardening work that stay behind
HEPA's existing contracts, safety gates, and CLI/headless fallback. The original
Phase 11 items below have focused test coverage plus benchmark or runtime
evidence recorded in their implementation commits. The Hermes-led orchestration
refinement is newly release-blocking and is preserved by runtime-command
validation through the bundled profiles.

| Stretch item | Evidence |
| --- | --- |
| Design-spec routing pipeline | Design artifact validation and routing tests cover HTML/CSS spec generation, active-content rejection, and design-to-implementation routing. Runtime evidence exercised `cargo test -p hepa-adapters design_pipeline`. |
| External mode status reporting | External status report validation, poller boundary tests, and fake external-worker integration cover queued/running/completed/blocked/failed reporting from work running elsewhere. Runtime evidence exercised `cargo test -p hepa-adapters external_worker`. |
| Per-lane cost accounting | Cost report tests cover usage extraction, tamper rejection, currency requirements, and lane total aggregation. Runtime evidence exercised lane cost reporting through CLI run tests. |
| GitHub issue webhook automation | Webhook tests cover issue import, signature verification, labels, redaction, missing acceptance handling, and non-issue payload rejection. Runtime evidence exercised `hepa github issue-webhook <payload> ...`. |
| Additional local-CLI adapters | Built-in registry, local-only routing, and local-worker tests cover local CLI routes through the same adapter contract. Runtime evidence exercised `hepa adapter list` and `hepa adapter doctor`. |
| Timing trend reports across archived runs | Core and CLI timing trend tests cover archived `timing.json` discovery, medians, per-run summaries, portable refs, and empty archive rejection. Runtime evidence exercised `hepa timing trends <archive-root>` over two archived records. |
| Desktop dashboard packaging | CLI tests cover static HTML and JSON snapshot generation, HTML escaping, scheduler state, card flags, and repo path omission. Runtime evidence exercised `hepa fleet dashboard --output <dashboard-html> --control-root <control-root>`. |
| Hermes-first desktop bridge | CLI and card-mapping tests cover spec-to-card ingest, ready/blocked task projection, dry-run card selection, deterministic lane attach commands, `hepa lane attach`, and `hepa fleet watch`. Runtime evidence exercised `cargo test -p hepa-cli fleet::tests::` and `cargo test -p hepa-kanban card_mapping`. |

## Required Hermes-Led Refinement

HEPA now treats Hermes as the default orchestration brain when Hermes is
available. Bundled profiles must coordinate task intake, Kanban population,
worker-brief generation, review, arbitration, repair loops, and human-friendly
manager-authored PR bodies:

| Required item | Status |
| --- | --- |
| Bundled `hepa-manager`, `hepa-worker`, `hepa-reviewer`, and `hepa-review-manager` profile contracts | Contract tests passed |
| Manager-authored `HepaHermesPrIntent` contract that requires human-friendly Summary, Changes, Validation, Review, Risk, and Run Context sections while rejecting generic HEPA validation-template PR bodies | Contract and PR-request tests passed |
| Runtime route from Hermes Kanban task to worker profile brief to coding adapter lane | Manager intake and worker brief command-runtime bridge tests passed |
| Per-lane live terminal/log streams for parallel Hermes-led runs | Adapter stdout/stderr, manager validation/tool-summary JSONL streams with redacted bounded model-visible previews, `hepa lane logs --tail`, and dashboard lane-stream presentation tests passed |
| Hermes reviewer and review-manager arbitration runtime, with Pi limited to code implementation in the default path | Pi reviewer execution is blocked in Hermes-led adapter-review mode; reviewer/review-manager artifact hooks and command-runtime bridge tests passed |
| Manager-authored human-friendly PR body wired into live PR creation, with HEPA validating and publishing safely | Intent-file and manager command-runtime bridge tests passed |
| Explicit Hermes-required mode for release validation | `HEPA_HERMES_REQUIRED=true` blocks headless fallback when worker brief, reviewer artifact, review-manager arbitration for findings, or manager PR intent sources are missing; focused CLI tests passed |
| Three-round task/work/review cap with human-intervention terminal state | Worker brief cap, review-to-worker repair mediation, round-3 allowance, and round-4 human-intervention cap tests passed |
| Headless/degraded fallback labels PR bodies as fallback evidence, not Hermes-authored project intent | Fallback PR body and Hermes intent separation tests passed |
| Hermes-first spec/card/run bridge with live lane visibility | `hepa hermes ingest-spec`, `hepa hermes run-ready`, `hepa hermes run-cards`, local card payloads, `lane_attach_commands`, `hepa lane attach`, and `hepa fleet watch` are documented and covered by focused CLI/card tests |
| Fresh Hermes-present local/hybrid validation evidence after the runtime route lands | Passed with `HEPA_HERMES_REQUIRED=true`: GPT-OSS 20B via llama.cpp completed the pure-local app/docs lanes 2/2 in 74.82 s and the hybrid local-worker / DeepSeek-reviewer lanes 2/2 in 75.69 s; validation PRs were opened, then closed and cleaned |

The Phase 11 privacy scan found only pre-existing placeholder examples and
redaction fixtures.

## Release Gate Posture

Phase 11 does not make Hermes availability a hard requirement for every local
CLI/headless operation, and degraded-mode fixtures must keep passing. It does
make Hermes-present orchestration release-blocking: when Hermes is available,
release validation must prove that PR bodies come from manager-authored
project-specific intent, not from HEPA's generic fallback evidence template.

The original Phase 11 gate run passed `cargo fmt --check`, `cargo test`,
`cargo clippy --all-targets --all-features -- -D warnings`, runtime checks for
`hepa timing trends <archive-root>` and
`hepa fleet dashboard --output <dashboard-html> --control-root <control-root>`,
and the repository privacy scan for the original Phase 11 items. The
Hermes-led refinement now has fresh local/hybrid runtime-command evidence; final
release still requires the normal repository checks and privacy scan after any
later edits.
