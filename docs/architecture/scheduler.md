# Scheduler Architecture

Scheduled work references existing Cortex agents and dispatches triggers through the standard execution engine.

```text
Schedule (cron / one-shot)
   ↓
Persistent job state (`cron_jobs` in SQLite)
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

## Execution Semantics

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

4. **Concurrency & Overlap Policies**:
   - **`skip`**: If an active run is in progress for the job, the incoming trigger is recorded as `skipped` without interrupting the ongoing run.
   - **`queue`**: If an active run is in progress, the trigger is queued and automatically dispatched once the active run finishes.
   - **`replace`**: If an active run is in progress, its cooperative [`CancellationToken`](crates/cortex-runtime/src/agent/execution.rs) is cancelled, the existing run is marked cancelled, and the new run starts immediately.
