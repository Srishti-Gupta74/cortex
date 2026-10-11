//! # cortex-tui
//!
//! Terminal user interface and control plane for Cortex.

#![deny(missing_docs)]

pub mod app;
pub mod commands;
pub mod event;
pub mod indexer;
pub mod markdown;
pub mod session;
pub mod terminal;
pub mod theme;
pub mod ui;

use app::App;
use cortex_core::Result;
use crossterm::event::{Event, KeyEventKind};
use ratatui::backend::Backend;
use ratatui::Terminal;
use std::path::Path;
use std::sync::Arc;
use terminal::TerminalGuard;

/// Current semantic version of the Cortex TUI crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Launch the interactive terminal control plane connected to the specified SQLite database path.
pub fn run_tui(db_path: &Path) -> Result<()> {
    let store = match cortex_runtime::RunStore::open(db_path) {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            tracing::warn!("Failed to open RunStore at {:?}: {}", db_path, e);
            None
        }
    };

    let mut app = App::new(store);
    app.set_tab(app::ActiveTab::Chat);
    let (mut terminal, _guard) = TerminalGuard::init()?;
    run_app(&mut terminal, app)
}

/// Run the TUI event loop on an initialized [`Terminal`] instance.
pub fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App) -> Result<()> {
    loop {
        app.poll_chat_updates();

        terminal
            .draw(|frame| ui::render(frame, &app))
            .map_err(|e| {
                cortex_core::CortexError::Internal(format!("terminal draw error: {}", e))
            })?;

        if app.should_quit {
            break;
        }

        if crossterm::event::poll(std::time::Duration::from_millis(100))
            .map_err(|e| cortex_core::CortexError::Internal(format!("event poll error: {}", e)))?
        {
            if let Event::Key(key) = crossterm::event::read().map_err(|e| {
                cortex_core::CortexError::Internal(format!("event read error: {}", e))
            })? {
                if key.kind == KeyEventKind::Press {
                    event::handle_key(&mut app, key);
                }
            }
        }
    }

    app.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use app::{ActiveTab, ChatMessageItem, ChatRole};
    use cortex_core::{EventRecord, ExecutionEvent, RunId};
    use cortex_runtime::storage::RunSummary;
    use ratatui::backend::TestBackend;

    #[test]
    fn test_headless_rendering_all_tabs_empty() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        let mut app = App::new(None);

        for tab in ActiveTab::all() {
            app.set_tab(*tab);
            terminal
                .draw(|f| ui::render(f, &app))
                .expect("draw tab in test backend");
        }
    }

    #[test]
    fn test_welcome_screen_renders_ascii_logo() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        let app = App::new(None);

        terminal
            .draw(|f| ui::render(f, &app))
            .expect("draw welcome screen in test backend");

        let buffer = terminal.backend().buffer();
        let content: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        // Verify CORTEX block glyphs are present in rendered buffer
        assert!(content.contains("██████"));

        // Verify active model name appears exactly once (in bottom status bar, never duplicated)
        let active_model = app.active_portal().model_name.as_str();
        let occurrences = content.matches(active_model).count();
        assert_eq!(
            occurrences, 1,
            "Model name should only appear once on screen, found {}",
            occurrences
        );

        // Also test narrow window fallback (< 56 columns)
        let narrow_backend = TestBackend::new(45, 30);
        let mut narrow_terminal = Terminal::new(narrow_backend).expect("create narrow terminal");
        narrow_terminal
            .draw(|f| ui::render(f, &app))
            .expect("draw narrow welcome screen");
    }

    #[test]
    fn test_headless_rendering_with_populated_data() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        let mut app = App::new(None);

        let run_id = RunId::generate();
        app.runs = vec![RunSummary {
            id: run_id.clone(),
            task: "Solve coding puzzle".to_string(),
            status: "completed".to_string(),
            started_at: "2026-10-07T12:00:00Z".to_string(),
            finished_at: Some("2026-10-07T12:01:00Z".to_string()),
            duration_ms: Some(60000),
            tokens_prompt: 1500,
            tokens_completion: 400,
            tokens_total: 1900,
            estimated_cost_usd: 0.0035,
            error: None,
        }];

        app.events = vec![
            EventRecord::new(
                1,
                ExecutionEvent::RunStarted {
                    run_id: run_id.clone(),
                    task: "Solve coding puzzle".to_string(),
                    workspace_root: None,
                },
            ),
            EventRecord::new(
                2,
                ExecutionEvent::ToolStarted {
                    run_id: run_id.clone(),
                    tool_name: "file_read".to_string(),
                    arguments: serde_json::json!({"path": "src/main.rs"}),
                },
            ),
            EventRecord::new(
                3,
                ExecutionEvent::RunCompleted {
                    run_id,
                    final_answer: "Solved".to_string(),
                    iterations: 2,
                    duration_ms: 60000,
                },
            ),
        ];

        app.chat_messages = vec![
            ChatMessageItem {
                role: ChatRole::User,
                content: "Run test suite".to_string(),
                timestamp: "12:00:00".to_string(),
                is_expanded: false,
            },
            ChatMessageItem {
                role: ChatRole::Thinking,
                content: "Analyzing workspace crates and locating tests".to_string(),
                timestamp: "12:00:02".to_string(),
                is_expanded: true,
            },
            ChatMessageItem {
                role: ChatRole::Tool,
                content: "Executed shell: cargo test".to_string(),
                timestamp: "12:00:05".to_string(),
                is_expanded: false,
            },
            ChatMessageItem {
                role: ChatRole::Assistant,
                content: "All tests pass successfully.".to_string(),
                timestamp: "12:00:10".to_string(),
                is_expanded: false,
            },
        ];

        for tab in ActiveTab::all() {
            app.set_tab(*tab);
            terminal
                .draw(|f| ui::render(f, &app))
                .expect("draw populated tab in test backend");
        }
    }

    #[test]
    fn test_run_app_terminates_on_should_quit() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        let mut app = App::new(None);
        app.should_quit = true;

        let res = run_app(&mut terminal, app);
        assert!(res.is_ok());
    }

    #[test]
    fn test_multi_agent_coordination_headless_rendering_across_terminal_sizes() {
        use app::{StageStatus, TeamAgentStatus};

        let terminal_sizes = [(160, 50), (120, 40), (80, 24), (60, 20)];

        for (width, height) in terminal_sizes {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("create test terminal");
            let mut app = App::new(None);

            // Configure team topology with live status badges per Issue #53
            if let Some(manager) = app
                .coordination
                .team_agents
                .iter_mut()
                .find(|a| a.id == "manager")
            {
                manager.status = TeamAgentStatus::Ready;
            }
            if let Some(researcher) = app
                .coordination
                .team_agents
                .iter_mut()
                .find(|a| a.id == "researcher")
            {
                researcher.status = TeamAgentStatus::Running;
            }
            if let Some(coder) = app
                .coordination
                .team_agents
                .iter_mut()
                .find(|a| a.id == "coder")
            {
                coder.status = TeamAgentStatus::Ready;
            }
            if let Some(reviewer) = app
                .coordination
                .team_agents
                .iter_mut()
                .find(|a| a.id == "reviewer")
            {
                reviewer.status = TeamAgentStatus::Paused;
            }

            // Configure stages: research (Completed) -> implementation (Running) -> review (Pending)
            if let Some(s) = app
                .coordination
                .pipeline_stages
                .iter_mut()
                .find(|s| s.id == "research")
            {
                s.status = StageStatus::Completed;
            }
            if let Some(s) = app
                .coordination
                .pipeline_stages
                .iter_mut()
                .find(|s| s.id == "implementation")
            {
                s.status = StageStatus::Running;
            }
            if let Some(s) = app
                .coordination
                .pipeline_stages
                .iter_mut()
                .find(|s| s.id == "review")
            {
                s.status = StageStatus::Pending;
            }

            // Record sequenced inter-agent messages
            app.record_inter_agent_message(
                "msg-1",
                "manager",
                "researcher",
                "team.research",
                "10:14:22",
                "Analyze auth controller tests",
            );
            app.record_inter_agent_message(
                "msg-2",
                "researcher",
                "coder",
                "team.code",
                "10:14:28",
                "Found 2 failing mocks in auth_test.rs",
            );
            app.record_inter_agent_message(
                "msg-3",
                "coder",
                "reviewer",
                "team.review",
                "10:15:02",
                "Applied patch in auth.rs; please verify",
            );

            // Ensure viewer is open and draw
            app.coordination.show_viewer = true;
            terminal
                .draw(|f| ui::render(f, &app))
                .unwrap_or_else(|e| panic!("Failed drawing on {}x{}: {}", width, height, e));

            // Verify content present on standard desktop size
            if width == 120 && height == 40 {
                let buffer = terminal.backend().buffer().clone();
                let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
                assert!(text.contains("Topology"), "Buffer missing Team Topology");
                assert!(text.contains("manager"), "Buffer missing manager");
                assert!(text.contains("researcher"), "Buffer missing researcher");
                assert!(text.contains("coder"), "Buffer missing coder");
                assert!(text.contains("reviewer"), "Buffer missing reviewer");
                assert!(text.contains("Pipeline"), "Buffer missing Pipeline");
                assert!(text.contains("research"), "Buffer missing research stage");
                assert!(
                    text.contains("implementation"),
                    "Buffer missing implementation stage"
                );
                assert!(text.contains("review"), "Buffer missing review stage");
                assert!(text.contains("Analyze auth"), "Buffer missing message 1");
                assert!(text.contains("Found 2 failing"), "Buffer missing message 2");
                assert!(text.contains("Applied patch"), "Buffer missing message 3");
            }
        }
    }

    #[test]
    fn test_multi_agent_filter_controls_and_slash_commands() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = App::new(None);
        app.record_inter_agent_message(
            "msg-1",
            "manager",
            "researcher",
            "team.research",
            "10:14:22",
            "Analyze auth controller tests",
        );
        app.record_inter_agent_message(
            "msg-2",
            "researcher",
            "coder",
            "team.code",
            "10:14:28",
            "Found 2 failing mocks in auth_test.rs",
        );
        app.record_inter_agent_message(
            "msg-3",
            "coder",
            "reviewer",
            "team.review",
            "10:15:02",
            "Applied patch in auth.rs; please verify",
        );

        assert_eq!(app.coordination.filtered_messages().len(), 3);

        // Filter by manager (should match msg-1)
        app.coordination.agent_filter = Some("manager".to_string());
        assert_eq!(app.coordination.filtered_messages().len(), 1);
        assert_eq!(app.coordination.filtered_messages()[0].id, "msg-1");

        // Filter by coder (should match msg-2 and msg-3)
        app.coordination.agent_filter = Some("coder".to_string());
        assert_eq!(app.coordination.filtered_messages().len(), 2);

        // Test SlashCommand execution
        let out = crate::commands::execute_command(
            &mut app,
            crate::commands::SlashCommand::Team {
                args: vec!["filter".to_string(), "reviewer".to_string()],
            },
        );
        assert!(out.contains("reviewer"));
        assert_eq!(app.coordination.filtered_messages().len(), 1);

        // Test clear filter
        let out_clear = crate::commands::execute_command(
            &mut app,
            crate::commands::SlashCommand::Team {
                args: vec!["clear".to_string()],
            },
        );
        assert!(out_clear.contains("Cleared"));
        assert_eq!(app.coordination.filtered_messages().len(), 3);

        // Test keybinding dispatch (Ctrl+M toggles viewer)
        let prior_show = app.coordination.show_viewer;
        event::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.coordination.show_viewer, !prior_show);
    }

    #[test]
    fn test_multi_agent_heavy_message_volume_zero_lag() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        let mut app = App::new(None);
        app.coordination.show_viewer = true;

        // Ingest 500 messages rapidly
        let start = std::time::Instant::now();
        for i in 0..500 {
            app.record_inter_agent_message(
                &format!("stress-msg-{}", i),
                if i % 2 == 0 { "manager" } else { "coder" },
                if i % 2 == 0 { "coder" } else { "reviewer" },
                "team.stress",
                "11:00:00",
                &format!("stress test payload payload {}", i),
            );
        }
        let ingest_elapsed = start.elapsed();
        assert!(
            ingest_elapsed < std::time::Duration::from_millis(150),
            "Ingestion took too long: {:?}",
            ingest_elapsed
        );

        // Render under heavy message volume
        let render_start = std::time::Instant::now();
        terminal
            .draw(|f| ui::render(f, &app))
            .expect("draw under heavy volume");
        let render_elapsed = render_start.elapsed();

        assert!(
            render_elapsed < std::time::Duration::from_millis(200),
            "Rendering under heavy volume took too long: {:?}",
            render_elapsed
        );
    }

    #[test]
    fn test_execution_event_inter_agent_message_ingestion_in_poll_chat() {
        let mut app = App::new(None);

        let event = ExecutionEvent::InterAgentMessage {
            run_id: RunId::from("coord-run-01"),
            message_id: "evt-msg-123".to_string(),
            sender: cortex_core::AgentId::from("manager"),
            recipient: cortex_core::AgentId::from("researcher"),
            routing_key: "team.research".to_string(),
            timestamp: "2026-10-10T10:14:22Z".to_string(),
            payload: serde_json::json!({
                "instructions": "Investigate security boundary test failures"
            }),
        };

        app.events.push(EventRecord::new(1, event));
        app.poll_chat_updates();

        assert!(app.coordination.show_viewer);
        assert_eq!(app.coordination.message_feed.len(), 1);
        let recorded = &app.coordination.message_feed[0];
        assert_eq!(recorded.id, "evt-msg-123");
        assert_eq!(recorded.sender, "manager");
        assert_eq!(recorded.recipient, "researcher");
        assert!(recorded.content.contains("Investigate security boundary"));
    }

    #[test]
    fn test_headless_rendering_scheduler_empty() {
        for (w, h) in [(120, 40), (80, 24), (50, 15)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).expect("create test terminal");
            let mut app = App::new(None);
            app.set_tab(ActiveTab::Scheduler);

            terminal
                .draw(|f| ui::render(f, &app))
                .expect("draw scheduler empty");

            let buffer = terminal.backend().buffer();
            let content: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
            assert!(content.contains("Cron Scheduler Monitor"));
            assert!(content.contains("Scheduled Jobs (0)"));
        }
    }

    #[test]
    fn test_headless_rendering_scheduler_populated() {
        use chrono::Utc;
        use cortex_runtime::{JobRunRecord, JobRunStatus, JobStatus, OverlapPolicy, ScheduledJob};

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        let mut app = App::new(None);
        app.set_tab(ActiveTab::Scheduler);

        let now = Utc::now();
        let job1_id = cortex_core::JobId::from("job-nightly-01");
        let job2_id = cortex_core::JobId::from("job-oneshot-02");

        let job1 = ScheduledJob {
            id: job1_id.clone(),
            name: "Nightly Evaluator".to_string(),
            schedule: "0 2 * * *".to_string(),
            prompt: "@eval-01 Run full benchmark matrix".to_string(),
            overlap_policy: OverlapPolicy::Skip,
            status: JobStatus::Active,
            next_run_at: Some(now + chrono::Duration::hours(4)),
            last_run_at: Some(now - chrono::Duration::hours(20)),
            created_at: now - chrono::Duration::days(2),
            updated_at: now - chrono::Duration::hours(20),
        };

        let job2 = ScheduledJob {
            id: job2_id.clone(),
            name: "One-shot Migration".to_string(),
            schedule: "@once 2026-10-15T00:00:00Z".to_string(),
            prompt: "@coder-01 Apply schema patch".to_string(),
            overlap_policy: OverlapPolicy::Replace,
            status: JobStatus::Paused,
            next_run_at: Some(now + chrono::Duration::days(3)),
            last_run_at: None,
            created_at: now - chrono::Duration::days(1),
            updated_at: now - chrono::Duration::days(1),
        };

        app.cron_jobs = vec![job1, job2];

        // Populate execution runs for job 1
        app.cron_job_runs = vec![
            JobRunRecord {
                id: "run-exec-001".to_string(),
                job_id: job1_id.clone(),
                status: JobRunStatus::Completed,
                started_at: now - chrono::Duration::minutes(50),
                finished_at: Some(now - chrono::Duration::minutes(48)),
                duration_ms: Some(120_000),
                output: Some("Evaluations completed with 100% pass rate".to_string()),
                error: None,
            },
            JobRunRecord {
                id: "run-exec-002".to_string(),
                job_id: job1_id,
                status: JobRunStatus::Failed,
                started_at: now - chrono::Duration::minutes(30),
                finished_at: Some(now - chrono::Duration::minutes(29)),
                duration_ms: Some(60_000),
                output: None,
                error: Some("Context timeout exceeded".to_string()),
            },
        ];

        // 1. Render with drawer closed
        app.cron_drawer_open = false;
        terminal
            .draw(|f| ui::render(f, &app))
            .expect("draw populated scheduler drawer closed");

        let content_closed: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content_closed.contains("Nightly Evaluator"));
        assert!(content_closed.contains("One-shot Migration"));
        assert!(content_closed.contains("0 2 * * *"));
        assert!(content_closed.contains("[ACTIVE]"));
        assert!(content_closed.contains("[PAUSED]"));
        assert!(content_closed.contains("eval-01"));

        // 2. Render with drawer open
        app.cron_drawer_open = true;
        terminal
            .draw(|f| ui::render(f, &app))
            .expect("draw populated scheduler drawer open");

        let content_open: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content_open.contains("Execution Runs"));
        assert!(content_open.contains("run-exec-001"));
        assert!(content_open.contains("[DONE]"));
        assert!(content_open.contains("[FAILED]"));
        assert!(content_open.contains("Run Inspector"));
    }

    #[test]
    fn test_scheduler_sqlite_store_integration_and_zero_lag() {
        use chrono::Utc;
        use cortex_runtime::{JobStatus, OverlapPolicy, RunStore, ScheduledJob};

        let store = std::sync::Arc::new(RunStore::in_memory().expect("in memory store"));
        let job = ScheduledJob {
            id: cortex_core::JobId::from("job-sql-01"),
            name: "Automated Backup".to_string(),
            schedule: "*/30 * * * *".to_string(),
            prompt: "Back up repository snapshot".to_string(),
            overlap_policy: OverlapPolicy::Queue,
            status: JobStatus::Active,
            next_run_at: Some(Utc::now() + chrono::Duration::minutes(10)),
            last_run_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        store.save_cron_job(&job).expect("save job");
        store
            .record_cron_run_start("cron-run-sql-01", &job.id, &Utc::now().to_rfc3339())
            .expect("record run start");
        store
            .record_cron_run_finish(
                "cron-run-sql-01",
                "completed",
                &Utc::now().to_rfc3339(),
                Some(1500),
                Some("Snapshot backed up successfully"),
                None,
            )
            .expect("record run finish");

        let mut app = App::new(Some(store));
        app.set_tab(ActiveTab::Scheduler);
        assert_eq!(app.cron_jobs.len(), 1);
        assert_eq!(app.cron_jobs[0].name, "Automated Backup");
        assert_eq!(app.cron_job_runs.len(), 1);
        assert_eq!(app.cron_job_runs[0].id, "cron-run-sql-01");

        // Toggle pause
        app.toggle_pause_selected_cron_job();
        assert_eq!(app.cron_jobs[0].status, JobStatus::Paused);

        // Toggle resume
        app.toggle_pause_selected_cron_job();
        assert_eq!(app.cron_jobs[0].status, JobStatus::Active);
    }
}
