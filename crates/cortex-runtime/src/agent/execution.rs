//! Agent execution context, iteration coordinator, cancellation, and system policies.

use crate::model::{ModelOutput, ModelProvider, ToolCall};
use crate::storage::RunStore;
use crate::tool::{ToolDefinition, ToolRegistry};
use crate::workspace::Workspace;
use chrono::Utc;
use cortex_core::{CortexError, EventRecord, ExecutionEvent, Redactor, Result, RunId};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Standard coding-agent policy guidelines from the architecture specification.
pub const CODING_AGENT_POLICY: &str = "\
You are Cortex, an autonomous coding agent operating inside a configured repository workspace.
Follow these operational principles strictly:
1. Inspect before modifying: Read files and directory contents before editing.
2. Understand existing code: Respect established patterns and architecture.
3. Make minimal changes: Change only what is required to achieve the goal.
4. Preserve project conventions: Follow existing style, naming, and structure.
5. Run relevant tests: Always verify behavior using the shell test runner.
6. Inspect failures: Analyze errors carefully rather than guessing fixes.
7. Iterate: Refine changes until all relevant checks pass.
8. Review final diff: Verify changes with git diff before committing.
9. Never claim success without verification: Do not declare completion until verified.\
";

/// Types of chat messages recorded during agent execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatMessage {
    /// System instruction or operational policy.
    System(String),
    /// User prompt or task assignment.
    User(String),
    /// Assistant response message.
    Assistant(String),
    /// Tool execution request proposed by the model.
    ToolCall(ToolCall),
    /// Result returned by executing a tool.
    ToolResult {
        /// Identifier linking back to the [`ToolCall`].
        call_id: String,
        /// Name of the tool executed.
        tool_name: String,
        /// Output content returned by the tool.
        output: String,
        /// Flag indicating whether the tool reported an error.
        is_error: bool,
    },
}

/// Task or todo item maintained by the agent during problem solving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Unique identifier for the item.
    pub id: String,
    /// Task description or objective.
    pub text: String,
    /// Completion status.
    pub completed: bool,
}

/// Authorization and confirmation mode for tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PermissionState {
    /// Interactive confirmation for modifying or executing tools.
    #[default]
    AskConfirm,
    /// Auto-accept (YOLO) mode where non-dangerous tools execute automatically.
    AutoAccept,
}

/// Metadata describing the active agent session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SessionMetadata {
    /// ISO 8601 start timestamp.
    pub started_at: String,
    /// Model name identifier.
    pub model_name: String,
    /// Provider family (e.g. "openai", "anthropic", "ollama").
    pub provider_kind: String,
    /// Total execution cost accumulator in USD.
    pub cost_usd: f64,
}

/// Execution context maintaining conversation history and workspace state.
#[derive(Debug, Clone)]
pub struct AgentContext {
    /// Unique identifier for this execution run.
    pub run_id: RunId,
    /// Initial task assigned to the agent.
    pub task: String,
    /// History of messages exchanged during this run.
    pub messages: Vec<ChatMessage>,
    /// Optional workspace boundary for filesystem and subprocess operations.
    pub workspace: Option<Arc<Workspace>>,
    /// Current iteration count.
    pub iterations: usize,
    /// Available tool definitions provided to the model.
    pub tools: Vec<ToolDefinition>,
    /// Current working directory path for relative resolutions.
    pub current_dir: std::path::PathBuf,
    /// List of file paths modified during this session.
    pub modified_files: Vec<String>,
    /// List of active tasks/todos tracked by the agent.
    pub todos: Vec<TodoItem>,
    /// Active permission mode for tool authorization.
    pub permission_state: PermissionState,
    /// Configured capability permissions boundary.
    pub permissions: Option<super::manager::AgentPermissions>,
    /// Session metadata and telemetry.
    pub session_metadata: SessionMetadata,
    /// Optional cancellation token for cooperative cancellation.
    pub cancellation_token: Option<CancellationToken>,
}

impl AgentContext {
    /// Create a new [`AgentContext`] with an initial user task and generated [`RunId`].
    pub fn new(task: impl Into<String>) -> Self {
        let task_str = task.into();
        Self {
            run_id: RunId::generate(),
            messages: vec![
                ChatMessage::System(CODING_AGENT_POLICY.to_string()),
                ChatMessage::User(task_str.clone()),
            ],
            task: task_str,
            workspace: None,
            iterations: 0,
            tools: Vec::new(),
            current_dir: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            modified_files: Vec::new(),
            todos: Vec::new(),
            permission_state: PermissionState::default(),
            permissions: None,
            session_metadata: SessionMetadata::default(),
            cancellation_token: None,
        }
    }

    /// Set an explicit [`RunId`] for this execution context.
    pub fn with_run_id(mut self, run_id: RunId) -> Self {
        self.run_id = run_id;
        self
    }

    /// Attach a [`Workspace`] boundary to this context.
    pub fn with_workspace(mut self, workspace: Arc<Workspace>) -> Self {
        self.current_dir = workspace.root().to_path_buf();
        self.workspace = Some(workspace);
        self
    }

    /// Attach tool definitions to this context.
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    /// Set the permission state for this context.
    pub fn with_permission_state(mut self, state: PermissionState) -> Self {
        self.permission_state = state;
        self
    }

    /// Attach capability permissions to this execution context.
    pub fn with_permissions(mut self, permissions: super::manager::AgentPermissions) -> Self {
        self.permissions = Some(permissions);
        self
    }

    /// Attach a cooperative cancellation token to this context.
    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// Check if cooperative cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token
            .as_ref()
            .is_some_and(|t| t.is_cancelled())
    }

    /// Append a message to the context history.
    pub fn push_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }

    /// Record a modified file into session tracking.
    pub fn record_modified_file(&mut self, path: impl Into<String>) {
        let p = path.into();
        if !self.modified_files.contains(&p) {
            self.modified_files.push(p);
        }
    }

    /// Add or update a todo item.
    pub fn upsert_todo(&mut self, id: impl Into<String>, text: impl Into<String>, completed: bool) {
        let id_str = id.into();
        let text_str = text.into();
        if let Some(existing) = self.todos.iter_mut().find(|t| t.id == id_str) {
            existing.text = text_str;
            existing.completed = completed;
        } else {
            self.todos.push(TodoItem {
                id: id_str,
                text: text_str,
                completed,
            });
        }
    }
}

/// Thread-safe cancellation token for terminating running agent loops.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Create a new, active [`CancellationToken`].
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Trigger cancellation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Check if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Final summary outcome produced by an agent execution run.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRunResult {
    /// Associated execution run identifier.
    pub run_id: RunId,
    /// Final text message or report from the agent.
    pub final_answer: String,
    /// Number of iterations executed during the run.
    pub iterations: usize,
    /// Whether the agent reached normal completion.
    pub completed: bool,
    /// Elapsed execution time in milliseconds.
    pub duration_ms: u64,
    /// Prompt tokens consumed across all model inference steps.
    pub tokens_prompt: usize,
    /// Completion tokens generated across all model inference steps.
    pub tokens_completion: usize,
    /// Total tokens consumed.
    pub tokens_total: usize,
    /// Estimated cost in USD.
    pub estimated_cost_usd: f64,
}

/// Callback invoked when an incremental streaming token is received.
pub type TokenCallback = Arc<dyn Fn(&str) + Send + Sync>;
/// Callback invoked when a tool invocation begins.
pub type ToolStartCallback = Arc<dyn Fn(&str, &str) + Send + Sync>;
/// Callback invoked when a tool invocation finishes.
pub type ToolEndCallback = Arc<dyn Fn(&str, &str, bool) + Send + Sync>;

/// Coordinates the iterative model $\to$ tool $\to$ result $\to$ model execution loop.
pub struct AgentLoop {
    max_iterations: usize,
    cancellation_token: CancellationToken,
    store: Option<Arc<RunStore>>,
    redactor: Arc<Redactor>,
    token_callback: Option<TokenCallback>,
    tool_start_callback: Option<ToolStartCallback>,
    tool_end_callback: Option<ToolEndCallback>,
    permissions: Option<super::manager::AgentPermissions>,
}

impl AgentLoop {
    /// Create a new [`AgentLoop`] with the specified iteration cap.
    pub fn new(max_iterations: usize) -> Self {
        Self {
            max_iterations,
            cancellation_token: CancellationToken::new(),
            store: None,
            redactor: Arc::new(Redactor::default()),
            token_callback: None,
            tool_start_callback: None,
            tool_end_callback: None,
            permissions: None,
        }
    }

    /// Attach a custom [`CancellationToken`].
    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = token;
        self
    }

    /// Attach a persistent [`RunStore`] for recording structured events and execution runs.
    pub fn with_store(mut self, store: Arc<RunStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Attach an optional persistent [`RunStore`].
    pub fn with_store_optional(mut self, store: Option<Arc<RunStore>>) -> Self {
        self.store = store;
        self
    }

    /// Attach a custom [`Redactor`] for scrubbing sensitive secrets from traces.
    pub fn with_redactor(mut self, redactor: Arc<Redactor>) -> Self {
        self.redactor = redactor;
        self
    }

    /// Attach a streaming token callback invoked on each incremental response token.
    pub fn with_token_callback(mut self, cb: TokenCallback) -> Self {
        self.token_callback = Some(cb);
        self
    }

    /// Attach tool lifecycle callbacks invoked on tool start and tool completion.
    pub fn with_tool_callbacks(
        mut self,
        start_cb: ToolStartCallback,
        end_cb: ToolEndCallback,
    ) -> Self {
        self.tool_start_callback = Some(start_cb);
        self.tool_end_callback = Some(end_cb);
        self
    }

    /// Attach explicit capability permissions to this agent execution loop.
    pub fn with_permissions(mut self, permissions: super::manager::AgentPermissions) -> Self {
        self.permissions = Some(permissions);
        self
    }

    /// Run the agent execution loop until completion, cancellation, or max iterations.
    pub fn run(
        &self,
        context: &mut AgentContext,
        model: &dyn ModelProvider,
        registry: &ToolRegistry,
    ) -> Result<AgentRunResult> {
        context.cancellation_token = Some(self.cancellation_token.clone());
        context.tools = registry.list();
        let start_time = Instant::now();
        let started_at = Utc::now().to_rfc3339();
        let mut sequence: u64 = 1;
        let mut total_prompt_tokens = 0usize;
        let mut total_completion_tokens = 0usize;

        // Record run start in store
        if let Some(store) = &self.store {
            store.record_run_start(&context.run_id, &context.task, &started_at)?;

            let ws_root = context
                .workspace
                .as_ref()
                .map(|w| w.root().to_string_lossy().to_string());

            let run_started_event = ExecutionEvent::RunStarted {
                run_id: context.run_id.clone(),
                task: self.redactor.redact_text(&context.task),
                workspace_root: ws_root,
            };
            store.record_event(&EventRecord::new(sequence, run_started_event))?;
            sequence += 1;
        }

        while context.iterations < self.max_iterations {
            if self.cancellation_token.is_cancelled() {
                let duration_ms = start_time.elapsed().as_millis() as u64;
                let finished_at = Utc::now().to_rfc3339();
                let estimated_cost = crate::providers::estimate_cost(
                    &model.descriptor().name,
                    total_prompt_tokens,
                    total_completion_tokens,
                );
                if let Some(store) = &self.store {
                    let cancel_event = ExecutionEvent::RunCancelled {
                        run_id: context.run_id.clone(),
                        reason: "agent execution cancelled by request".to_string(),
                    };
                    store.record_event(&EventRecord::new(sequence, cancel_event))?;
                    store.record_run_completion(
                        &context.run_id,
                        "cancelled",
                        &finished_at,
                        duration_ms,
                        total_prompt_tokens as u32,
                        total_completion_tokens as u32,
                        estimated_cost,
                        Some("agent execution cancelled by request"),
                    )?;
                }
                return Err(CortexError::Cancelled(
                    "agent execution cancelled by request".to_string(),
                ));
            }

            context.iterations += 1;

            // Emit ModelRequest event
            if let Some(store) = &self.store {
                let preview = context
                    .messages
                    .last()
                    .map(|m| format!("{:?}", m))
                    .unwrap_or_default();
                let event = ExecutionEvent::ModelRequest {
                    run_id: context.run_id.clone(),
                    prompt_preview: self.redactor.redact_text(&preview),
                };
                store.record_event(&EventRecord::new(sequence, event))?;
                sequence += 1;
            }

            // Generate next action from model (with progressive token streaming)
            let mut on_token = |chunk: &str| {
                if self.cancellation_token.is_cancelled() {
                    return Err(CortexError::Cancelled(
                        "agent execution cancelled by request".to_string(),
                    ));
                }
                if let Some(cb) = &self.token_callback {
                    cb(chunk);
                }
                Ok(())
            };
            let output = match model.stream(context, &mut on_token).and_then(|output| {
                if self.cancellation_token.is_cancelled() {
                    Err(CortexError::Cancelled(
                        "agent execution cancelled by request".to_string(),
                    ))
                } else {
                    Ok(output)
                }
            }) {
                Ok(out) => out,
                Err(CortexError::Cancelled(reason)) => {
                    let duration_ms = start_time.elapsed().as_millis() as u64;
                    let finished_at = Utc::now().to_rfc3339();
                    let estimated_cost = crate::providers::estimate_cost(
                        &model.descriptor().name,
                        total_prompt_tokens,
                        total_completion_tokens,
                    );
                    if let Some(store) = &self.store {
                        let cancel_event = ExecutionEvent::RunCancelled {
                            run_id: context.run_id.clone(),
                            reason: reason.clone(),
                        };
                        store.record_event(&EventRecord::new(sequence, cancel_event))?;
                        store.record_run_completion(
                            &context.run_id,
                            "cancelled",
                            &finished_at,
                            duration_ms,
                            total_prompt_tokens as u32,
                            total_completion_tokens as u32,
                            estimated_cost,
                            Some(&reason),
                        )?;
                    }
                    return Err(CortexError::Cancelled(reason));
                }
                Err(e) => return Err(e),
            };
            if let Some(usage) = model.last_usage() {
                total_prompt_tokens += usage.prompt_tokens;
                total_completion_tokens += usage.completion_tokens;
            }

            // Emit ModelResponse event
            if let Some(store) = &self.store {
                let summary = match &output {
                    ModelOutput::FinalAnswer(ans) => format!("FinalAnswer: {}", ans),
                    ModelOutput::ToolCalls(calls) => {
                        let names: Vec<_> = calls.iter().map(|c| c.name.as_str()).collect();
                        format!("ToolCalls: {}", names.join(", "))
                    }
                };
                let structured_output = serde_json::to_value(&output).ok();
                let event = ExecutionEvent::ModelResponse {
                    run_id: context.run_id.clone(),
                    output_summary: self.redactor.redact_text(&summary),
                    structured_output,
                };
                store.record_event(&EventRecord::new(sequence, event))?;
                sequence += 1;
            }

            match output {
                ModelOutput::FinalAnswer(answer) => {
                    let duration_ms = start_time.elapsed().as_millis() as u64;
                    let finished_at = Utc::now().to_rfc3339();

                    context.push_message(ChatMessage::Assistant(answer.clone()));

                    if let Some(store) = &self.store {
                        let comp_event = ExecutionEvent::RunCompleted {
                            run_id: context.run_id.clone(),
                            final_answer: self.redactor.redact_text(&answer),
                            iterations: context.iterations,
                            duration_ms,
                        };
                        let estimated_cost = crate::providers::estimate_cost(
                            &model.descriptor().name,
                            total_prompt_tokens,
                            total_completion_tokens,
                        );
                        store.record_event(&EventRecord::new(sequence, comp_event))?;
                        store.record_run_completion(
                            &context.run_id,
                            "completed",
                            &finished_at,
                            duration_ms,
                            total_prompt_tokens as u32,
                            total_completion_tokens as u32,
                            estimated_cost,
                            None,
                        )?;
                    }

                    let estimated_cost = crate::providers::estimate_cost(
                        &model.descriptor().name,
                        total_prompt_tokens,
                        total_completion_tokens,
                    );

                    return Ok(AgentRunResult {
                        run_id: context.run_id.clone(),
                        final_answer: answer,
                        iterations: context.iterations,
                        completed: true,
                        duration_ms,
                        tokens_prompt: total_prompt_tokens,
                        tokens_completion: total_completion_tokens,
                        tokens_total: total_prompt_tokens + total_completion_tokens,
                        estimated_cost_usd: estimated_cost,
                    });
                }
                ModelOutput::ToolCalls(calls) => {
                    for call in calls {
                        if self.cancellation_token.is_cancelled() {
                            let duration_ms = start_time.elapsed().as_millis() as u64;
                            let finished_at = Utc::now().to_rfc3339();
                            if let Some(store) = &self.store {
                                let cancel_event = ExecutionEvent::RunCancelled {
                                    run_id: context.run_id.clone(),
                                    reason: "agent execution cancelled during tool processing"
                                        .to_string(),
                                };
                                store.record_event(&EventRecord::new(sequence, cancel_event))?;
                                store.record_run_completion(
                                    &context.run_id,
                                    "cancelled",
                                    &finished_at,
                                    duration_ms,
                                    0,
                                    0,
                                    0.0,
                                    Some("agent execution cancelled during tool processing"),
                                )?;
                            }
                            return Err(CortexError::Cancelled(
                                "agent execution cancelled during tool processing".to_string(),
                            ));
                        }

                        // Emit ToolStarted event
                        if let Some(store) = &self.store {
                            let start_event = ExecutionEvent::ToolStarted {
                                run_id: context.run_id.clone(),
                                tool_name: call.name.clone(),
                                arguments: self.redactor.redact_json(&call.arguments),
                            };
                            store.record_event(&EventRecord::new(sequence, start_event))?;
                            sequence += 1;
                        }

                        context.push_message(ChatMessage::ToolCall(call.clone()));

                        if let Some(cb) = &self.tool_start_callback {
                            let args_str =
                                serde_json::to_string(&call.arguments).unwrap_or_default();
                            cb(&call.name, &args_str);
                        }

                        // Execute tool through registry with capability permissions validation
                        let active_perms =
                            self.permissions.as_ref().or(context.permissions.as_ref());
                        let tool_result = registry.execute_with_permissions(
                            &call.name,
                            &call.arguments,
                            active_perms,
                            Some(&self.cancellation_token),
                        );

                        if let Err(CortexError::Cancelled(reason)) = &tool_result {
                            let duration_ms = start_time.elapsed().as_millis() as u64;
                            let finished_at = Utc::now().to_rfc3339();
                            if let Some(store) = &self.store {
                                let cancel_event = ExecutionEvent::RunCancelled {
                                    run_id: context.run_id.clone(),
                                    reason: reason.clone(),
                                };
                                store.record_event(&EventRecord::new(sequence, cancel_event))?;
                                store.record_run_completion(
                                    &context.run_id,
                                    "cancelled",
                                    &finished_at,
                                    duration_ms,
                                    total_prompt_tokens as u32,
                                    total_completion_tokens as u32,
                                    0.0,
                                    Some(reason),
                                )?;
                            }
                            return Err(CortexError::Cancelled(reason.clone()));
                        }

                        match tool_result {
                            Ok(res) => {
                                if let Some(cb) = &self.tool_end_callback {
                                    cb(&call.name, &res.output, res.is_error);
                                }

                                let redacted_out = self.redactor.redact_text(&res.output);
                                if let Some(store) = &self.store {
                                    let ev = if res.is_error {
                                        ExecutionEvent::ToolFailed {
                                            run_id: context.run_id.clone(),
                                            tool_name: call.name.clone(),
                                            error: redacted_out.clone(),
                                        }
                                    } else {
                                        ExecutionEvent::ToolCompleted {
                                            run_id: context.run_id.clone(),
                                            tool_name: call.name.clone(),
                                            output: redacted_out.clone(),
                                        }
                                    };
                                    store.record_event(&EventRecord::new(sequence, ev))?;
                                    sequence += 1;
                                }

                                if !res.is_error
                                    && (call.name == "write_file"
                                        || call.name == "edit_file"
                                        || call.name == "delete_file")
                                {
                                    if let Some(path_str) =
                                        call.arguments.get("path").and_then(|p| p.as_str())
                                    {
                                        context.record_modified_file(path_str);
                                    }
                                }

                                context.push_message(ChatMessage::ToolResult {
                                    call_id: call.id,
                                    tool_name: call.name,
                                    output: res.output,
                                    is_error: res.is_error,
                                });
                            }
                            Err(e) => {
                                let err_msg = format!("execution error: {}", e);
                                if let Some(cb) = &self.tool_end_callback {
                                    cb(&call.name, &err_msg, true);
                                }

                                let redacted_err = self.redactor.redact_text(&err_msg);
                                if let Some(store) = &self.store {
                                    let ev = ExecutionEvent::ToolFailed {
                                        run_id: context.run_id.clone(),
                                        tool_name: call.name.clone(),
                                        error: redacted_err,
                                    };
                                    store.record_event(&EventRecord::new(sequence, ev))?;
                                    sequence += 1;
                                }

                                context.push_message(ChatMessage::ToolResult {
                                    call_id: call.id,
                                    tool_name: call.name,
                                    output: err_msg,
                                    is_error: true,
                                });
                            }
                        }
                    }
                }
            }
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;
        let finished_at = Utc::now().to_rfc3339();
        if let Some(store) = &self.store {
            let error_event = ExecutionEvent::AgentError {
                run_id: context.run_id.clone(),
                error: format!("exceeded max iterations limit ({})", self.max_iterations),
            };
            let estimated_cost = crate::providers::estimate_cost(
                &model.descriptor().name,
                total_prompt_tokens,
                total_completion_tokens,
            );
            store.record_event(&EventRecord::new(sequence, error_event))?;
            store.record_run_completion(
                &context.run_id,
                "failed",
                &finished_at,
                duration_ms,
                total_prompt_tokens as u32,
                total_completion_tokens as u32,
                estimated_cost,
                Some("exceeded max iterations limit"),
            )?;
        }

        Err(CortexError::Timeout(self.max_iterations as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MockModelProvider;
    use crate::tool::{Tool, ToolDefinition, ToolResult};
    use serde_json::json;

    struct DummyTool;
    impl Tool for DummyTool {
        fn definition(&self) -> &ToolDefinition {
            static DEF: std::sync::OnceLock<ToolDefinition> = std::sync::OnceLock::new();
            DEF.get_or_init(|| {
                ToolDefinition::new("dummy", "test dummy tool", json!({ "type": "object" }))
            })
        }

        fn execute(&self, _input: &serde_json::Value) -> Result<ToolResult> {
            Ok(ToolResult::success("dummy called"))
        }
    }

    struct SecretLeakingTool;
    impl Tool for SecretLeakingTool {
        fn definition(&self) -> &ToolDefinition {
            static DEF: std::sync::OnceLock<ToolDefinition> = std::sync::OnceLock::new();
            DEF.get_or_init(|| {
                ToolDefinition::new("secret_tool", "leaks secret", json!({ "type": "object" }))
            })
        }

        fn execute(&self, _input: &serde_json::Value) -> Result<ToolResult> {
            Ok(ToolResult::success(
                "API Token: sk-live1234567890abcdef12345678",
            ))
        }
    }

    #[test]
    fn test_agent_loop_successful_completion() {
        let mut context = AgentContext::new("solve task");
        let model = MockModelProvider::new();
        let registry = ToolRegistry::new();

        registry.register_tool(DummyTool).unwrap();

        // Step 1: Model calls tool
        model.queue_response(ModelOutput::ToolCalls(vec![ToolCall::new(
            "call_1",
            "dummy",
            json!({}),
        )]));

        // Step 2: Model finishes with answer
        model.queue_response(ModelOutput::FinalAnswer("task resolved".to_string()));

        let agent_loop = AgentLoop::new(5);
        let result = agent_loop.run(&mut context, &model, &registry).unwrap();

        assert!(result.completed);
        assert_eq!(result.final_answer, "task resolved");
        assert_eq!(result.iterations, 2);
    }

    #[test]
    fn test_agent_loop_cancellation() {
        let mut context = AgentContext::new("infinite task");
        let model = MockModelProvider::new();
        let registry = ToolRegistry::new();

        let token = CancellationToken::new();
        token.cancel();

        let agent_loop = AgentLoop::new(5).with_cancellation_token(token);
        let err = agent_loop.run(&mut context, &model, &registry).unwrap_err();

        match err {
            CortexError::Cancelled(_) => {}
            _ => panic!("expected Cancelled error, got {:?}", err),
        }
    }

    #[test]
    fn test_agent_loop_with_store_and_secret_redaction() {
        let store = Arc::new(RunStore::in_memory().unwrap());
        let mut context = AgentContext::new("Process with sk-secretkey1234567890abcdef");
        let model = MockModelProvider::new();
        let registry = ToolRegistry::new();

        registry.register_tool(SecretLeakingTool).unwrap();

        model.queue_response(ModelOutput::ToolCalls(vec![ToolCall::new(
            "c1",
            "secret_tool",
            json!({ "token": "ghp_1234567890abcdef1234567890abcdef1234" }),
        )]));
        model.queue_response(ModelOutput::FinalAnswer("all done".to_string()));

        let agent_loop = AgentLoop::new(5).with_store(Arc::clone(&store));
        let res = agent_loop.run(&mut context, &model, &registry).unwrap();
        assert!(res.completed);

        // Verify stored run
        let run = store.get_run(&res.run_id).unwrap().unwrap();
        assert_eq!(run.status, "completed");

        // Verify stored events and redactions
        let events = store.get_events(&res.run_id).unwrap();
        assert!(!events.is_empty());

        for record in &events {
            match &record.event {
                ExecutionEvent::RunStarted { task, .. } => {
                    assert!(!task.contains("sk-secretkey"));
                    assert!(task.contains("[REDACTED]"));
                }
                ExecutionEvent::ToolStarted { arguments, .. } => {
                    let arg_str = arguments.to_string();
                    assert!(!arg_str.contains("ghp_"));
                    assert!(arg_str.contains("[REDACTED]"));
                }
                ExecutionEvent::ToolCompleted { output, .. } => {
                    assert!(!output.contains("sk-live"));
                    assert!(output.contains("[REDACTED]"));
                }
                _ => {}
            }
        }
    }

    #[test]
    fn test_agent_loop_cancellation_during_streaming() {
        let mut context = AgentContext::new("stream and cancel");
        let model = MockModelProvider::new();
        let registry = ToolRegistry::new();

        model.queue_response(ModelOutput::FinalAnswer(
            "one two three four five six seven eight nine ten".to_string(),
        ));

        let token = CancellationToken::new();
        let token_for_cb = token.clone();
        let received_tokens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count_cb = Arc::clone(&received_tokens);

        let token_cb = Arc::new(move |_chunk: &str| {
            let prev = count_cb.fetch_add(1, Ordering::SeqCst);
            if prev >= 2 {
                token_for_cb.cancel();
            }
        });

        let agent_loop = AgentLoop::new(5)
            .with_cancellation_token(token)
            .with_token_callback(token_cb);

        let err = agent_loop.run(&mut context, &model, &registry).unwrap_err();
        match err {
            CortexError::Cancelled(reason) => {
                assert!(reason.contains("cancelled"));
            }
            other => panic!("expected Cancelled error, got: {:?}", other),
        }

        assert!(received_tokens.load(Ordering::SeqCst) < 10);
    }

    #[test]
    fn test_agent_loop_enforces_permissions_and_records_tool_failed() {
        let store = Arc::new(RunStore::in_memory().unwrap());
        let permissions = crate::agent::manager::AgentPermissions::read_only();
        let mut context = AgentContext::new("attempt write").with_permissions(permissions);
        let model = MockModelProvider::new();
        let registry = ToolRegistry::new();

        struct MutatingTool {
            def: ToolDefinition,
        }
        impl Tool for MutatingTool {
            fn definition(&self) -> &ToolDefinition {
                &self.def
            }
            fn execute(&self, _input: &serde_json::Value) -> Result<ToolResult> {
                Ok(ToolResult::success("mutated workspace"))
            }
        }

        registry
            .register_tool(MutatingTool {
                def: ToolDefinition::new("write_file", "writes file", json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" }, "content": { "type": "string" } }
                }))
                .with_permission(crate::tool::PermissionLevel::WorkspaceWrite),
            })
            .unwrap();

        // Model attempts to call write_file (disallowed under read_only permissions)
        model.queue_response(ModelOutput::ToolCalls(vec![ToolCall::new(
            "call_disallowed_1",
            "write_file",
            json!({ "path": "test.txt", "content": "malicious edit" }),
        )]));
        model.queue_response(ModelOutput::FinalAnswer(
            "stopped due to permission denial".to_string(),
        ));

        let agent_loop = AgentLoop::new(5).with_store(Arc::clone(&store));
        let res = agent_loop.run(&mut context, &model, &registry).unwrap();
        assert!(res.completed);

        // Verify stored events: should have ToolFailed recorded for write_file
        let events = store.get_events(&res.run_id).unwrap();
        let failed_event = events
            .iter()
            .find(|record| matches!(&record.event, ExecutionEvent::ToolFailed { tool_name, .. } if tool_name == "write_file"));
        assert!(
            failed_event.is_some(),
            "ExecutionEvent::ToolFailed must be recorded in RunStore"
        );

        if let ExecutionEvent::ToolFailed { error, .. } = &failed_event.unwrap().event {
            assert!(error.to_lowercase().contains("permission denied"));
        } else {
            panic!("expected ToolFailed event");
        }
    }
}
