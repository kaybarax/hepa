# HEPA Adapter Lessons

## RS-2 Pi And Cross-Harness Lessons

- Pi local OpenAI-compatible providers should be configured in Pi's user
  `models.json` with `api=openai-completions`; provider keys for loopback
  routes can be inert placeholders and should not become manager credentials.
- Local reasoning-model endpoints, including exo + Apple MLX routes, can be
  reachable and still fail live editing when they stream only hidden reasoning.
  HEPA now appends a Pi-only, local reasoning-model no-think prompt boundary so
  the worker produces content before the deterministic stall monitor fires.
- Non-fake adapters must always route through live adapter execution. A CLI
  dispatch path that treats only Pi as live is an agnosticism defect, even if
  fake tests continue to pass.
- Cross-harness parity should compare artifact shape and state transitions, not
  just terminal success. In RS-2, Pi cloud, Pi local, and custom all produced
  live worker, validation, review, staging/PR, timing, and final-report records.
- Board sync was unavailable in RS-2, so the correct evidence shape is an
  explicit no-card record plus clean validation cleanup rather than an invented
  Hermes card history.
