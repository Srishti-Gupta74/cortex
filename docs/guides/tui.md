# Terminal UI (TUI) Control Plane

Cortex provides a terminal-native interactive control plane (`cortex-tui`) built with [Ratatui](https://ratatui.rs) and [Crossterm](https://github.com/crossterm-rs/crossterm).

The TUI acts as a completely decoupled observability and inspection interface over Cortex's persistent execution state, connecting directly to SQLite via `RunStore` without duplicating runtime logic or interfering with agent processes.

```
┌────────────────────────────────────────────────────────────────────────┐
│ Cortex Agent Runtime Control Plane                                     │
├────────────────────────────────────────────────────────────────────────┤
│ [1: Dashboard] [2: Agents] [3: Active Run] [4: Events] [5: History]   │
├────────────────────────────────────────────────────────────────────────┤
│                                                                        │
│   ┌───────────────┐ ┌───────────────┐ ┌───────────────┐                │
│   │  Total Runs   │ │   Completed   │ │    Failed     │                │
│   │      12       │ │      11       │ │       1       │                │
│   └───────────────┘ └───────────────┘ └───────────────┘                │
│                                                                        │
│   Execution Log & Event Trace Inspector                                │
│   [#1] 12:00:01  RunStarted        Solve coding task                   │
│   [#2] 12:00:05  ToolStarted       file_read {"path":"src/lib.rs"}     │
│   [#3] 12:00:10  RunCompleted      Solved                              │
│                                                                        │
├────────────────────────────────────────────────────────────────────────┤
│ Tab/1-6: Tabs | ↑/↓: Navigate | Enter: Inspect | r: Refresh | q: Quit   │
└────────────────────────────────────────────────────────────────────────┘
```

## Launching the Control Plane

Launch the interactive control plane directly from any terminal by simply running `cortex`:

```bash
cortex
```

### Custom Database Path

To connect to a specific database (e.g. during testing or multi-environment management):

```bash
cortex --db /path/to/cortex.db
```

Alternatively, set the `CORTEX_DB_PATH` environment variable:

```bash
CORTEX_DB_PATH=/path/to/cortex.db cortex
```

---

## Views and Navigation

The control plane contains 8 primary views accessible via numeric keys (`1` through `8`) or `Tab` / `Shift+Tab`:

### 1. Dashboard (`1`)
- **Metric Tiles**: Total runs, completed runs, failure/abort counts, and active worker count.
- **Recent Executions**: Quick view of latest agent tasks with duration and status badges.
- **System Status**: SQLite connection status and runtime engine health.

### 2. Agents (`2`)
- **Agent Roster**: Configured agent personas, roles, and status.
- **Policy Inspector**: Operational policy definitions, safety constraints, and assigned capabilities for the selected agent.

### 3. Active Run (`3`)
- **Run Overview**: Detailed execution metadata for the selected run (Run ID, status, start/finish timestamps, duration).
- **Resource Usage**: Token consumption breakdown (prompt, completion, total) and estimated inference cost (USD).
- **Execution Outcome**: Error logs, stack traces, and final answers.

### 4. Events (`4`)
- **Event Trace**: Chronological sequence table of all events emitted during the run (`RunStarted`, `ToolStarted`, `ToolCompleted`, `ToolFailed`, `ModelRequest`, `ModelResponse`, `RunCompleted`).
- **Structured Payload Inspector**: Formatted JSON inspector showing exact arguments, outputs, and parameters passed to tools and models.

### 5. History (`5`)
- **Runs Table**: Complete historical log of execution runs recorded in the persistent SQLite database.
- Navigate rows with `↑`/`↓` and press `Enter` to switch to the Active Run and Event views for that execution.

### 6. Tasks & Bench (`6`)
- **Curated Tasks**: Interactive browser of benchmark evaluation tasks across suites (`coding`, `refactor`, `cli`).
- Inspect task prompts, test files, and verification commands.

### 7. Agent Chat (`7`)
- **Interactive Autonomous Agent Dispatch**: Directly submit instructions and task prompts from the terminal UI.
- **Real-Time Stream**: Live view of tool invocations, model reasoning phases, and final answers as the agent operates in the background.
- **Live Cancellation**: Press `Esc` while an agent is executing to gracefully terminate the run.

### 8. Model Portals (`8`)
- **Provider & Model Configuration**: Manage inference endpoints including OpenAI (`gpt-4o-mini`, `gpt-4o`), Anthropic Claude (`claude-3-5-sonnet`, `claude-3-5-haiku`), local Ollama (`ollama/llama3.1`, `ollama/qwen2.5-coder`), DeepSeek (`deepseek-chat`), and custom OpenAI-compatible endpoints.
- **Credential & Endpoint Management**: Press `e` to set custom API keys, `b` to configure base endpoint URLs, `a` to create new portals, and `Space` / `Enter` to activate the target model for agent execution.
- **Validation**: Press `t` to test configuration and verify connectivity.

### 9. Cron Scheduler & Job Monitor (`9`)
- **Summary Metrics**: High-level telemetry displaying total registered background jobs, active triggers, paused jobs, completed one-shots, and the earliest upcoming trigger countdown.
- **Structured Job Table**: Columns displaying:
  - **Job Name & ID**: Human-readable identifier and permanent JobId.
  - **Schedule Expression**: Standard cron expression (e.g. `*/15 * * * *`, `0 2 * * *`) or `@once <timestamp>` one-shot specification.
  - **Next Fire Countdown**: Human-friendly relative countdown (`in 14m 20s`, `in 2h 15m`, `Due now`, `Paused`, `Completed`).
  - **Target Agent**: The resolved worker agent bound to execute the task (e.g. `coder-01`, `eval-01`, or default agent).
  - **Overlap Policy**: Concurrency boundary (`Skip`, `Queue`, `Replace`).
  - **Status**: Live lifecycle badge (`[ACTIVE]`, `[PAUSED]`, `[DONE]`).
- **Interactive Execution Run Drawer**:
  - Press `Enter` on any scheduled job to open/close the **Execution Runs Drawer**.
  - Inspect recent execution attempts, correlated execution Run IDs (`runs.id`), duration, start timestamps, and output previews.
  - Navigate execution attempts with `↑`/`↓` (`j`/`k`) to inspect detailed error traces and final answer summaries.
  - Press `Esc` to collapse the drawer and return to the main table.
- **Operator Actions**:
  - `p`: Pause or resume the selected scheduled job in SQLite.
  - `t`: Manually trigger the selected job immediately.
  - `r`: Refresh jobs and execution runs directly from SQLite `RunStore`.

### Slash Commands: `/cron` and `/scheduler`

From the interactive Chat harness, background scheduled jobs can be inspected without leaving the prompt:
- `/cron` (or `/scheduler`): Displays an ASCII card of all registered jobs, countdowns, agents, and overlap policies.
- `/cron view`: One-step shortcut jumping directly to the interactive Scheduler control plane (Tab 9).
- `/cron runs <id>`: Prints recent execution records and outputs for the specified job.

---

## Keyboard Controls

| Key | Context | Action |
|---|---|---|
| `Tab` | Global | Cycle to next tab |
| `Shift+Tab` / `BackTab` | Global | Cycle to previous tab |
| `1` ..= `9` | Global / Normal | Jump directly to tab 1 through 9 |
| `↑` / `k` | Normal | Move selection up in list/table |
| `↓` / `j` | Normal | Move selection down in list/table |
| `Enter` | Normal | Inspect selected run / activate portal / toggle drawer |
| `r` | Normal | Refresh state from SQLite database |
| `Esc` | Normal | Close drawer / return to Dashboard |
| `q` / `Ctrl+C` | Global | Cleanly exit TUI |
| `Enter` | Chat Tab | Dispatch agent with current input prompt |
| `Esc` | Chat Tab | Cancel running agent / clear input prompt |
| `PageUp` / `PageDown` | Chat Tab | Scroll chat message log |
| `Space` / `Enter` | Portals Tab | Set selected portal as active for agent dispatch |
| `a` | Portals Tab | Start interactive wizard to add a new portal |
| `e` | Portals Tab | Edit custom API key for selected portal |
| `b` | Portals Tab | Edit base endpoint URL for selected portal |
| `d` | Portals Tab | Delete selected portal |
| `t` | Portals Tab | Test portal credentials and configuration |
| `Enter` | Scheduler Tab | Toggle Execution Runs Drawer for selected job |
| `Esc` | Scheduler Tab | Close Execution Runs Drawer / navigate to Dashboard |
| `p` | Scheduler Tab | Pause or resume the selected scheduled job |
| `t` | Scheduler Tab | Immediately trigger selected job execution |

---

## Terminal Safety and Panic Recovery

The TUI is engineered to prevent terminal corruption:

1. **RAII Terminal Guard**: `TerminalGuard` automatically restores the terminal out of raw mode, leaves the alternate screen buffer, and restores the cursor on drop.
2. **Panic Hook Restoration**: If an unhandled panic occurs anywhere in the process, a custom panic hook restores standard terminal mode before printing the panic unwinding message, preventing terminal freezing or invisible typing.
3. **Headless Test Backend**: Automated testing utilizes `ratatui::backend::TestBackend` to verify all views and keyboard transitions without spawning terminal sessions or requiring interactive input.
