//! Integration tests for persistent cron scheduling engine, one-shot timers,
//! execution overlap policies, and restart recovery.

use chrono::{Duration, TimeZone, Utc};
use cortex_core::JobId;
use cortex_runtime::agent::{AgentManager, AgentManifest, AgentModelConfig};
use cortex_runtime::model::{MockModelProvider, ModelOutput, ToolCall};
use cortex_runtime::scheduler::{
    CronExpression, JobRunStatus, JobStatus, OverlapPolicy, Schedule, SchedulerEngine,
    TriggerResult,
};
use cortex_runtime::storage::RunStore;
use cortex_runtime::tool::{Tool, ToolDefinition, ToolRegistry, ToolResult};
use serde_json::json;
use std::sync::Arc;

#[test]
fn test_cron_parsing_comprehensive() {
    // 5-field standard expressions
    let every_5_min = CronExpression::parse("*/5 * * * *").unwrap();
    assert!(every_5_min.minute.matches(0));
    assert!(every_5_min.minute.matches(5));
    assert!(!every_5_min.minute.matches(6));
    assert!(every_5_min.hour.is_wildcard());

    // Macros
    let hourly = CronExpression::parse("@hourly").unwrap();
    assert!(hourly.minute.matches(0));
    assert!(hourly.hour.is_wildcard());

    let midnight = CronExpression::parse("@midnight").unwrap();
    assert!(midnight.minute.matches(0));
    assert!(midnight.hour.matches(0));

    // Named months and days
    let complex = CronExpression::parse("30 8 15 JAN,JUN MON-FRI").unwrap();
    assert!(complex.minute.matches(30));
    assert!(complex.hour.matches(8));
    assert!(complex.day_of_month.matches(15));
    assert!(complex.month.matches(1));
    assert!(complex.month.matches(6));
    assert!(!complex.month.matches(2));
    assert!(complex.day_of_week.matches(1)); // Mon
    assert!(complex.day_of_week.matches(5)); // Fri
    assert!(!complex.day_of_week.matches(0)); // Sun
}

#[test]
fn test_next_execution_calculation_without_busy_polling() {
    let cron = CronExpression::parse("0 9 * * 1-5").unwrap(); // 9:00 AM on weekdays
    let friday = Utc.with_ymd_and_hms(2026, 10, 9, 17, 30, 0).unwrap(); // Friday 5:30 PM

    let next = cron.next_run_after(&friday).unwrap();
    // Should jump directly to next Monday Oct 12 at 09:00:00
    assert_eq!(next, Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap());
}

#[test]
fn test_one_shot_timer_schedule() {
    let target = Utc::now() + Duration::hours(2);
    let sched_str = format!("@once {}", target.to_rfc3339());
    let sched = Schedule::parse(&sched_str).unwrap();

    assert!(sched.is_one_shot());
    let before = Utc::now();
    assert_eq!(sched.next_run_after(&before), Some(target));

    let after = target + Duration::minutes(1);
    assert_eq!(sched.next_run_after(&after), None);
}

#[test]
fn test_sqlite_persistence_across_restarts() {
    let tmp_dir =
        std::env::temp_dir().join(format!("cortex_cron_persist_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    let db_path = tmp_dir.join("cortex_cron.db");

    let job_id: JobId;

    // Phase 1: Create store, register job, start a run, and drop engine/store
    {
        let store = Arc::new(RunStore::open(&db_path).unwrap());
        let engine = SchedulerEngine::new(store);

        let job = engine
            .register_job(
                "Nightly Cleanup",
                "0 2 * * *",
                "Clean up temporary build artifacts",
                OverlapPolicy::Queue,
            )
            .unwrap();

        job_id = job.id.clone();

        let trigger = engine.trigger_job(&job_id, Utc::now()).unwrap();
        match trigger {
            TriggerResult::Started { run_id, .. } => {
                engine
                    .finish_job_run(
                        &run_id,
                        &job_id,
                        JobRunStatus::Completed,
                        Some("Cleaned 45 files".into()),
                        None,
                    )
                    .unwrap();
            }
            _ => panic!("expected started"),
        }
    }

    // Phase 2: Reopen from disk, verify job and run history persist completely
    {
        let store = Arc::new(RunStore::open(&db_path).unwrap());
        let engine = SchedulerEngine::new(store);

        let job = engine.get_job(&job_id).unwrap().expect("job must persist");
        assert_eq!(job.name, "Nightly Cleanup");
        assert_eq!(job.schedule, "0 2 * * *");
        assert_eq!(job.prompt, "Clean up temporary build artifacts");
        assert_eq!(job.overlap_policy, OverlapPolicy::Queue);
        assert_eq!(job.status, JobStatus::Active);

        let runs = engine.list_job_runs(&job_id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, JobRunStatus::Completed);
        assert_eq!(runs[0].output.as_deref(), Some("Cleaned 45 files"));
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_overlap_policy_skip_behavior() {
    let store = Arc::new(RunStore::in_memory().unwrap());
    let engine = SchedulerEngine::new(store);

    let job = engine
        .register_job("Skip Job", "*/5 * * * *", "Prompt", OverlapPolicy::Skip)
        .unwrap();

    let now = Utc::now();
    let res1 = engine.trigger_job(&job.id, now).unwrap();
    let run_id = match res1 {
        TriggerResult::Started { run_id, .. } => run_id,
        _ => panic!("expected started"),
    };

    // Second trigger while first is running -> Skipped
    let res2 = engine.trigger_job(&job.id, now).unwrap();
    assert!(matches!(res2, TriggerResult::Skipped { .. }));

    // Complete run
    engine
        .finish_job_run(&run_id, &job.id, JobRunStatus::Completed, None, None)
        .unwrap();

    let runs = engine.list_job_runs(&job.id, 10).unwrap();
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().any(|r| r.status == JobRunStatus::Skipped));
    assert!(runs.iter().any(|r| r.status == JobRunStatus::Completed));
}

#[test]
fn test_overlap_policy_queue_behavior() {
    let store = Arc::new(RunStore::in_memory().unwrap());
    let engine = SchedulerEngine::new(store);

    let job = engine
        .register_job("Queue Job", "*/5 * * * *", "Prompt", OverlapPolicy::Queue)
        .unwrap();

    let now = Utc::now();
    let res1 = engine.trigger_job(&job.id, now).unwrap();
    let first_run_id = match res1 {
        TriggerResult::Started { run_id, .. } => run_id,
        _ => panic!("expected started"),
    };

    // Second trigger while first is running -> Queued
    let res2 = engine.trigger_job(&job.id, now).unwrap();
    assert!(matches!(res2, TriggerResult::Queued { .. }));

    // Finish first run -> should automatically trigger queued execution!
    let next_run = engine
        .finish_job_run(&first_run_id, &job.id, JobRunStatus::Completed, None, None)
        .unwrap();

    assert!(matches!(next_run, Some(TriggerResult::Started { .. })));
}

#[test]
fn test_overlap_policy_replace_behavior() {
    let store = Arc::new(RunStore::in_memory().unwrap());
    let engine = SchedulerEngine::new(store);

    let job = engine
        .register_job(
            "Replace Job",
            "*/5 * * * *",
            "Prompt",
            OverlapPolicy::Replace,
        )
        .unwrap();

    let now = Utc::now();
    let res1 = engine.trigger_job(&job.id, now).unwrap();
    let first_run_id = match res1 {
        TriggerResult::Started { run_id, .. } => run_id,
        _ => panic!("expected started"),
    };

    // Second trigger while running -> Replaces active run
    let res2 = engine.trigger_job(&job.id, now).unwrap();
    match res2 {
        TriggerResult::Replaced {
            cancelled_run_id,
            new_run_id,
            ..
        } => {
            assert_eq!(cancelled_run_id, first_run_id);
            assert_ne!(new_run_id, first_run_id);
        }
        _ => panic!("expected replaced"),
    }

    let runs = engine.list_job_runs(&job.id, 10).unwrap();
    let cancelled = runs.iter().find(|r| r.id == first_run_id).unwrap();
    assert_eq!(cancelled.status, JobRunStatus::Cancelled);
}

#[test]
fn test_time_until_next_job_and_tick() {
    let store = Arc::new(RunStore::in_memory().unwrap());
    let engine = SchedulerEngine::new(store);

    // No jobs yet
    assert_eq!(engine.time_until_next_job(Utc::now()).unwrap(), None);

    let target = Utc::now() + Duration::seconds(30);
    let sched_str = format!("@once {}", target.to_rfc3339());
    let job = engine
        .register_job("One Shot", &sched_str, "Execute once", OverlapPolicy::Skip)
        .unwrap();

    let duration = engine.time_until_next_job(Utc::now()).unwrap().unwrap();
    assert!(duration.as_secs() <= 30);

    // Tick before target time: nothing triggered
    let triggered_early = engine.tick(Utc::now()).unwrap();
    assert!(triggered_early.is_empty());

    // Tick at/after target time: triggers job
    let triggered = engine.tick(target + Duration::seconds(1)).unwrap();
    assert_eq!(triggered.len(), 1);
    assert!(matches!(triggered[0], TriggerResult::Started { .. }));

    // After triggering one-shot, status is Completed and next_run_at is None
    let updated_job = engine.get_job(&job.id).unwrap().unwrap();
    assert_eq!(updated_job.status, JobStatus::Completed);
    assert_eq!(updated_job.next_run_at, None);
}

struct TestDiagnosticTool;
impl Tool for TestDiagnosticTool {
    fn definition(&self) -> &ToolDefinition {
        static DEF: std::sync::OnceLock<ToolDefinition> = std::sync::OnceLock::new();
        DEF.get_or_init(|| {
            ToolDefinition::new(
                "test_diagnostic",
                "Runs system diagnostic and checks status",
                json!({
                    "type": "object",
                    "properties": {
                        "component": { "type": "string" }
                    },
                    "required": ["component"]
                }),
            )
        })
    }

    fn execute(&self, input: &serde_json::Value) -> cortex_core::Result<ToolResult> {
        let comp = input
            .get("component")
            .and_then(|v| v.as_str())
            .unwrap_or("core");
        Ok(ToolResult::success(format!("Diagnostic {} ok", comp)))
    }
}

#[test]
fn test_scheduler_dispatch_agent_loop_execution_and_event_linkage() {
    let store = Arc::new(RunStore::in_memory().unwrap());
    let model = MockModelProvider::new();

    // Step 1: Model requests tool execution
    model.queue_response(ModelOutput::ToolCalls(vec![ToolCall::new(
        "call_diag_1",
        "test_diagnostic",
        json!({ "component": "storage" }),
    )]));
    // Step 2: Model returns final answer
    model.queue_response(ModelOutput::FinalAnswer(
        "System diagnostics completed successfully: storage nominal".to_string(),
    ));

    let registry = Arc::new(ToolRegistry::new());
    registry.register_tool(TestDiagnosticTool).unwrap();

    let engine = SchedulerEngine::new(Arc::clone(&store))
        .with_model_provider(Arc::new(model))
        .with_tool_registry(registry);

    let job = engine
        .register_job(
            "Periodic Diagnostic",
            "*/10 * * * *",
            "Execute system diagnostics on storage",
            OverlapPolicy::Skip,
        )
        .unwrap();

    let trigger = engine.dispatch_job(&job.id, Utc::now()).unwrap();
    let cron_run_id = match trigger {
        TriggerResult::Started { run_id, .. } => run_id,
        _ => panic!("expected trigger started"),
    };

    // Verify cron run record and trace linkage
    let runs = engine.list_job_runs(&job.id, 10).unwrap();
    assert_eq!(runs.len(), 1);
    let run_record = &runs[0];
    assert_eq!(run_record.id, cron_run_id);
    assert_eq!(run_record.status, JobRunStatus::Completed);
    assert_eq!(
        run_record.output.as_deref(),
        Some("System diagnostics completed successfully: storage nominal")
    );

    let execution_run_id = run_record
        .run_id
        .as_ref()
        .expect("cron_job_runs.run_id must link to runs.id");

    // Verify correlated execution run in SQLite runs table
    let run_summary = store
        .get_run(execution_run_id)
        .unwrap()
        .expect("run must exist in runs table");
    assert_eq!(run_summary.status, "completed");
    assert_eq!(run_summary.task, "Execute system diagnostics on storage");

    // Verify full SQLite event tracing
    let events = store.get_events(execution_run_id).unwrap();
    assert_eq!(events.len(), 8);

    assert_eq!(events[0].event.event_type(), "RunStarted");
    assert_eq!(events[1].event.event_type(), "ModelRequest");
    assert_eq!(events[2].event.event_type(), "ModelResponse");
    assert_eq!(events[3].event.event_type(), "ToolStarted");
    assert_eq!(events[4].event.event_type(), "ToolCompleted");
    assert_eq!(events[5].event.event_type(), "ModelRequest");
    assert_eq!(events[6].event.event_type(), "ModelResponse");
    assert_eq!(events[7].event.event_type(), "RunCompleted");

    // Verify sequences are strictly 1..=8
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev.sequence, (i + 1) as u64);
    }
}

#[test]
fn test_scheduler_target_agent_resolution_and_fallback() {
    let store = Arc::new(RunStore::in_memory().unwrap());
    let agent_manager = Arc::new(AgentManager::new_with_store(Arc::clone(&store)));

    let tmp_dir = std::env::temp_dir().join(format!("cortex_agent_res_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);

    // Register primary agent
    let manifest1 = AgentManifest::new(
        "Security Scanner",
        "Security Auditor",
        &tmp_dir,
        AgentModelConfig::new("mock", "mock-model"),
    )
    .with_system_prompt("You are a specialized security auditing agent.");
    let _agent1 = agent_manager.create(manifest1).unwrap();

    // Register secondary agent
    let manifest2 = AgentManifest::new(
        "Ops Specialist",
        "DevOps Operator",
        &tmp_dir,
        AgentModelConfig::new("mock", "mock-model"),
    )
    .with_system_prompt("You are a dedicated infrastructure operations agent.");
    let agent2 = agent_manager.create(manifest2).unwrap();

    let model = MockModelProvider::new();
    model.queue_response(ModelOutput::FinalAnswer(
        "Security scan finished".to_string(),
    ));
    model.queue_response(ModelOutput::FinalAnswer("Ops run finished".to_string()));

    let engine = SchedulerEngine::new(Arc::clone(&store))
        .with_agent_manager(Arc::clone(&agent_manager))
        .with_model_provider(Arc::new(model));

    // Job 1: configured with agent2 explicitly
    let job1 = engine
        .register_job_with_agent(
            "Ops Audit",
            "0 3 * * *",
            "Audit clusters",
            OverlapPolicy::Skip,
            Some(agent2.id.clone()),
        )
        .unwrap();
    assert_eq!(job1.agent_id, Some(agent2.id));

    let trigger1 = engine.dispatch_job(&job1.id, Utc::now()).unwrap();
    assert!(matches!(trigger1, TriggerResult::Started { .. }));

    let runs1 = engine.list_job_runs(&job1.id, 1).unwrap();
    assert_eq!(runs1[0].output.as_deref(), Some("Security scan finished"));

    // Job 2: fallback to default registered agent when agent_id is None
    let job2 = engine
        .register_job(
            "General Maintenance",
            "0 4 * * *",
            "General health check",
            OverlapPolicy::Skip,
        )
        .unwrap();
    assert_eq!(job2.agent_id, None);

    let trigger2 = engine.dispatch_job(&job2.id, Utc::now()).unwrap();
    assert!(matches!(trigger2, TriggerResult::Started { .. }));

    let runs2 = engine.list_job_runs(&job2.id, 1).unwrap();
    assert_eq!(runs2[0].output.as_deref(), Some("Ops run finished"));

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_scheduler_dispatch_overlap_replace_and_cancel_active() {
    let store = Arc::new(RunStore::in_memory().unwrap());
    let engine = SchedulerEngine::new(Arc::clone(&store));

    let job = engine
        .register_job(
            "Long Task",
            "*/5 * * * *",
            "Long running task",
            OverlapPolicy::Replace,
        )
        .unwrap();

    // Trigger run 1
    let trigger1 = engine.trigger_job(&job.id, Utc::now()).unwrap();
    let run1_id = match trigger1 {
        TriggerResult::Started { run_id, .. } => run_id,
        _ => panic!("expected started"),
    };

    // User or system cancels active job
    let cancelled = engine.cancel_active_job(&job.id).unwrap();
    assert!(cancelled);

    let runs = engine.list_job_runs(&job.id, 10).unwrap();
    assert_eq!(runs[0].id, run1_id);
    assert_eq!(runs[0].status, JobRunStatus::Cancelled);
}

#[test]
fn test_scheduler_tick_dispatches_through_agent_loop_when_model_configured() {
    let store = Arc::new(RunStore::in_memory().unwrap());
    let model = MockModelProvider::new();
    model.queue_response(ModelOutput::FinalAnswer(
        "Tick execution completed".to_string(),
    ));

    let engine = SchedulerEngine::new(Arc::clone(&store)).with_model_provider(Arc::new(model));

    let target = Utc::now() + Duration::seconds(10);
    let sched_str = format!("@once {}", target.to_rfc3339());
    let job = engine
        .register_job(
            "Scheduled Tick",
            &sched_str,
            "Tick prompt",
            OverlapPolicy::Skip,
        )
        .unwrap();

    // Tick at/after scheduled target time: dispatches through AgentLoop!
    let results = engine.tick(target + Duration::seconds(1)).unwrap();
    assert_eq!(results.len(), 1);
    assert!(matches!(results[0], TriggerResult::Started { .. }));

    let runs = engine.list_job_runs(&job.id, 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, JobRunStatus::Completed);
    assert_eq!(runs[0].output.as_deref(), Some("Tick execution completed"));
    assert!(runs[0].run_id.is_some());

    let exec_run_id = runs[0].run_id.as_ref().unwrap();
    let stored_run = store.get_run(exec_run_id).unwrap().unwrap();
    assert_eq!(stored_run.status, "completed");
}
