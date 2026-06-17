# HEPA Fleet

The fleet layer schedules tasks across projects and reconciles drift, all backed
by a deterministic, temp-root-safe registry.

## Projects and tasks

```bash
hepa project add app-one /path/to/repo --name "App One" --max-parallel 4
hepa project list
hepa task create app-one task-1 "Fix login redirect"
hepa task block task-1
hepa task resume task-1
hepa task prioritize task-1 9
```

Projects carry repo path, display name, default branch, routing policy, max
parallel tasks, memory/cost policy, and Hermes board metadata. Tasks carry
dependencies and readiness state. Invalid repo paths and secret-like fields are
rejected.

## Scheduler

```bash
hepa scheduler start
hepa scheduler status
hepa scheduler tick
hepa scheduler stop
```

The scheduler selects the highest-priority ready task whose dependencies are met
and admits it only when resource and conflict rules allow, then atomically claims
exactly one ready task into one lane. A task can never be double-claimed.

## Resource and conflict rules

Admission enforces, with a recorded wait reason for every block:

- overall lane capacity
- paid-cloud lane caps (the Nth cloud lane waits while local lanes proceed)
- per-adapter concurrency caps
- file-area reservations (overlapping work serializes)
- conflict groups (one active lane per group)
- serialize-on-lockfile (one lockfile-touching lane at a time)

## Monitor, reconcile, and cleanup

```bash
hepa fleet status
hepa fleet report
hepa fleet reconcile
hepa fleet cleanup
```

The monitor refreshes process liveness, branch/PR status, validation/review
state, resource samples, and card drift. Reconcile repairs stale leases, missing
cards, orphaned worktrees, and terminal lanes. Cleanup removes only
HEPA-created runtime state and preserves unrelated user changes.
