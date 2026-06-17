# HEPA Stretch Phase 11

Phase 11 features are post-gate extensions that stay behind HEPA's existing
contracts, safety gates, and CLI/headless fallback. Each item below has focused
test coverage plus benchmark or runtime evidence recorded in its implementation
commit.

| Stretch item | Evidence |
| --- | --- |
| Design-spec routing pipeline | Design artifact validation and routing tests cover HTML/CSS spec generation, active-content rejection, and design-to-implementation routing. Runtime evidence exercised `cargo test -p hepa-adapters design_pipeline`. |
| External mode status reporting | External status report validation, poller boundary tests, and fake external-worker integration cover queued/running/completed/blocked/failed reporting from work running elsewhere. Runtime evidence exercised `cargo test -p hepa-adapters external_worker`. |
| Per-lane cost accounting | Cost report tests cover usage extraction, tamper rejection, currency requirements, and lane total aggregation. Runtime evidence exercised lane cost reporting through CLI run tests. |
| GitHub issue webhook automation | Webhook tests cover issue import, signature verification, labels, redaction, missing acceptance handling, and non-issue payload rejection. Runtime evidence exercised `hepa github issue-webhook <payload> ...`. |
| Additional local-CLI adapters | Built-in registry, local-only routing, and local-worker tests cover local CLI routes through the same adapter contract. Runtime evidence exercised `hepa adapter list` and `hepa adapter doctor`. |
| Timing trend reports across archived runs | Core and CLI timing trend tests cover archived `timing.json` discovery, medians, per-run summaries, portable refs, and empty archive rejection. Runtime evidence exercised `hepa timing trends <archive-root>` over two archived records. |
| Desktop dashboard packaging | CLI tests cover static HTML and JSON snapshot generation, HTML escaping, scheduler state, card flags, and repo path omission. Runtime evidence exercised `hepa fleet dashboard --output <dashboard-html> --control-root <control-root>`. |

The Phase 11 privacy scan found only pre-existing placeholder examples and
redaction fixtures.
