# Scheduler Architecture

The scheduler engine manages autonomous background job triggers and recurring tasks in Cortex. Scheduled work references existing Cortex agents and dispatches triggers through the standard execution engine, backed by SQLite storage (`RunStore`).

```text
Schedule (cron / one-shot)
   ↓
Persistent job state (`cron_jobs` in SQLite)
   ↓
Scheduler Engine: tick() / dispatch_job()
   ↓
Target Agent Resolution (`AgentManager` lookup or fallback)
   ↓
Standard Execution Run (`AgentContext` + `AgentLoop`)
   ↓
Structured Tracing & Event Stream (`runs` + `events` in `RunStore`)
   ↓
Correlated Trace Linkage (`cron_job_runs.run_id` -> `runs.id`)
```

Every scheduled execution uses the same runtime, permission, tracing, and tool sandbox systems as interactive executions.

## Execution Semantics & Agent Dispatch

1. **Target Agent Resolution**:
   - When a scheduled job configures an `agent_id`, the scheduler resolves the agent definition and operational manifests through [`AgentManager`](crates/cortex-runtime/src/agent/manager.rs).
   - If `agent_id` is not configured or cannot be resolved, the scheduler falls back to the first registered agent in `AgentManager`.
   - If no agents are registered, it applies the standard coding agent operational policy (`CODING_AGENT_POLICY`) with auto-accept permissions.

2. **Standard Execution Run**:
   - Each trigger allocates a fresh [`RunId`](crates/cortex-core/src/id.rs).
   - Initializes an [`AgentContext`](crates/cortex-runtime/src/agent/execution.rs) populated with the task prompt, target workspace boundary, tool capabilities, and permissions.
   - Executes through [`AgentLoop`](crates/cortex-runtime/src/agent/execution.rs), iteratively interacting with the configured [`ModelProvider`](crates/cortex-runtime/src/model.rs) and tool registry.

3. **Structured Tracing & Correlated Linkage**:
   - Full SQLite event tracing is captured in `RunStore`, recording granular events (`RunStarted`, `ModelRequest`, `ModelResponse`, `ToolStarted`, `ToolCompleted`, `RunCompleted`, `RunFailed`, `RunCancelled`).
   - High-level cron execution records in `cron_job_runs` link directly to the underlying execution trace via `cron_job_runs.run_id = runs.id`.

## Schedule Definitions

Jobs declare their execution timing through the `Schedule` model in `crates/cortex-runtime/src/scheduler/cron.rs`:

1. **Standard 5-field cron expressions**:
   - Fields: `minute` (0-59), `hour` (0-23), `day_of_month` (1-31), `month` (1-12), and `day_of_week` (0-7, where both 0 and 7 represent Sunday).
   - Wildcards (`*`), steps (`*/15`), lists (`1,15,30`), ranges (`1-5`), and named tokens (`MON-FRI`, `JAN-MAR`).
   - Macros: `@yearly`, `@monthly`, `@weekly`, `@daily`, `@midnight`, `@hourly`.
2. **One-shot timers**:
   - Prefix syntax: `@once <ISO-8601-TIMESTAMP>` or `@at <ISO-8601-TIMESTAMP>`.
   - Fires once at the designated instant, transitions job status to `Completed`, and clears `next_run_at`.

## Overlap Policies & Concurrency

When a scheduled trigger fires while a previous execution run is still active, the engine applies the configured `OverlapPolicy`:

- **`skip`** (default): Drops the current trigger and records a skipped run record in `cron_job_runs`. Advances `next_run_at` to the next future scheduled interval, preventing overlapping runs without building up backlogs.
- **`queue`**: Increments the job's queued execution backlog. When the currently running execution finishes via `finish_job_run`, the engine automatically consumes the queue and starts the next execution.
- **`replace`**: Cancels the active run (`cancel_cron_run`), records its status as `Cancelled` with an explanation, and immediately starts a new execution run.

## Downtime Recovery Behavior

When the Cortex process is offline across multiple scheduled intervals, the engine avoids thundering-herd cascades upon restart:

1. **Catch-up evaluation**: During startup or the first `engine.tick(now)` after downtime, any active job where `next_run_at <= now` triggers immediately.
2. **Next run calculation**: The next execution time is computed strictly relative to the current wall-clock time (`sched.next_run_after(now)`), rather than replaying every missed historical point in time.
3. **Skip policy behavior**: Jobs configured with `OverlapPolicy::Skip` trigger once at recovery time and advance directly to the next future interval. Missed intervals in the downtime window do not accumulate.
4. **Queue policy catch-up**: Jobs configured with `OverlapPolicy::Queue` queue successive triggers and drain them sequentially upon each run completion.

## Cancellation & Deletion Behavior

- **Deletion during active runs**: When `engine.delete_job(id)` is called, the job is removed from `cron_jobs`, all associated run records in `cron_job_runs` are removed via database cascade, and any queued trigger counters are evicted from memory. Orphaned runs that complete after job deletion are handled gracefully without panics.
- **Replacement cancellation**: When `OverlapPolicy::Replace` cancels an active execution, the cancelled run is recorded with `JobRunStatus::Cancelled` and an explicit error explanation in SQLite before the replacement run starts.

## Timezone & Calendar Boundary Transitions

All internal scheduling calculations operate strictly in `DateTime<Utc>`:

- **Timezone offsets**: Timestamps with arbitrary UTC offsets (such as `+05:30` or `-08:00`) are normalized to UTC upon parsing.
- **Month-end transitions**: Transitions across month boundaries (such as May 31 to June 1) accurately account for months with 28, 29, 30, or 31 days.
- **Leap years**: Daily schedules correctly advance from February 28 to February 29 in leap years (such as 2028), and from February 28 to March 1 in non-leap years (such as 2027). Schedules targeting February 29 specifically (`0 0 29 2 *`) skip non-leap years and resolve to the next leap year.
- **Daylight saving shifts**: Because expressions evaluate against UTC timestamps without local timezone ambiguity, transitions across DST shifts occur predictably without duplicate or skipped hours.
