# HEPA Stretch Phase 11

Phase 11 features are now required release hardening work that stay behind
HEPA's existing contracts, safety gates, and CLI/headless fallback. The original
Phase 11 items below have focused test coverage plus benchmark or runtime
evidence recorded in their implementation commits. The Hermes-led orchestration
refinement is newly release-blocking and remains in progress until the runtime
routes live Hermes-present runs through the bundled profiles.

| Stretch item | Evidence |
| --- | --- |
| Design-spec routing pipeline | Design artifact validation and routing tests cover HTML/CSS spec generation, active-content rejection, and design-to-implementation routing. Runtime evidence exercised `cargo test -p hepa-adapters design_pipeline`. |
| External mode status reporting | External status report validation, poller boundary tests, and fake external-worker integration cover queued/running/completed/blocked/failed reporting from work running elsewhere. Runtime evidence exercised `cargo test -p hepa-adapters external_worker`. |
| Per-lane cost accounting | Cost report tests cover usage extraction, tamper rejection, currency requirements, and lane total aggregation. Runtime evidence exercised lane cost reporting through CLI run tests. |
| GitHub issue webhook automation | Webhook tests cover issue import, signature verification, labels, redaction, missing acceptance handling, and non-issue payload rejection. Runtime evidence exercised `hepa github issue-webhook <payload> ...`. |
| Additional local-CLI adapters | Built-in registry, local-only routing, and local-worker tests cover local CLI routes through the same adapter contract. Runtime evidence exercised `hepa adapter list` and `hepa adapter doctor`. |
| Timing trend reports across archived runs | Core and CLI timing trend tests cover archived `timing.json` discovery, medians, per-run summaries, portable refs, and empty archive rejection. Runtime evidence exercised `hepa timing trends <archive-root>` over two archived records. |
| Desktop dashboard packaging | CLI tests cover static HTML and JSON snapshot generation, HTML escaping, scheduler state, card flags, and repo path omission. Runtime evidence exercised `hepa fleet dashboard --output <dashboard-html> --control-root <control-root>`. |

## Required Hermes-Led Refinement

HEPA now treats Hermes as the default orchestration brain when Hermes is
available. Bundled profiles must coordinate task intake, Kanban population,
worker-brief generation, review, arbitration, repair loops, and PR intent:

| Required item | Status |
| --- | --- |
| Bundled `hepa-manager`, `hepa-worker`, `hepa-reviewer`, and `hepa-review-manager` profile contracts | Contract tests passed |
| Manager-authored `HepaHermesPrIntent` contract that rejects generic HEPA validation-template PR bodies | Contract and PR-request tests passed |
| Runtime route from Hermes Kanban task to worker profile brief to coding adapter lane | Required |
| Per-lane live terminal/log streams for parallel Hermes-led runs | Required |
| Hermes reviewer and review-manager arbitration runtime, with Pi limited to code implementation in the default path | Required |
| Manager-authored PR intent wired into live PR creation, with HEPA validating and publishing safely | Intent-file hook tests passed; full Hermes runtime still required |
| Three-round task/work/review cap with human-intervention terminal state | Required |
| Fresh Hermes-present local/hybrid validation evidence after the runtime route lands | Required |

The Phase 11 privacy scan found only pre-existing placeholder examples and
redaction fixtures.

## Release Gate Posture

Phase 11 does not make Hermes availability a hard requirement for every local
CLI/headless operation, and degraded-mode fixtures must keep passing. It does
make Hermes-present orchestration release-blocking: when Hermes is available,
release validation must prove that PR bodies come from manager-authored
project-specific intent, not from HEPA's generic fallback evidence template.

The final Phase 11 gate run passed `cargo fmt --check`, `cargo test`,
`cargo clippy --all-targets --all-features -- -D warnings`, runtime checks for
`hepa timing trends <archive-root>` and
`hepa fleet dashboard --output <dashboard-html> --control-root <control-root>`,
and the repository privacy scan for the original Phase 11 items. The
Hermes-led refinement requires its own fresh verification before v1.0.0 release.
