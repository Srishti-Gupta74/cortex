//! Application state management and navigation model for the Cortex TUI.

use cortex_core::{EventRecord, ExecutionEvent};
use cortex_harness::suite::get_suite_tasks;
use cortex_harness::task::BenchmarkTask;
use cortex_runtime::agent::{AgentContext, AgentLoop, AgentRunResult, CancellationToken};
use cortex_runtime::providers::create_model_provider;
use cortex_runtime::storage::{RunStore, RunSummary};
use cortex_runtime::tool::ToolRegistry;
use cortex_runtime::tools;
use cortex_runtime::workspace::Workspace;
use serde::{Deserialize, Serialize};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Available navigation tabs in the Cortex control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActiveTab {
    /// Overview dashboard with health indicators and metrics.
    #[default]
    Dashboard,
    /// Agent list and active policy inspector.
    Agents,
    /// Detailed inspector for current or selected execution run.
    ActiveRun,
    /// Chronological event and execution trace viewer.
    Events,
    /// Historical execution runs stored in SQLite.
    History,
    /// Curated benchmark tasks and evaluations.
    Tasks,
    /// Interactive chat interface for autonomous agent execution.
    Chat,
    /// Model provider and custom inference portal configuration.
    Portals,
    /// Background cron jobs and scheduled execution monitor.
    Scheduler,
}

impl ActiveTab {
    /// Return all tabs in standard display order.
    pub fn all() -> &'static [ActiveTab] {
        &[
            ActiveTab::Dashboard,
            ActiveTab::Agents,
            ActiveTab::ActiveRun,
            ActiveTab::Events,
            ActiveTab::History,
            ActiveTab::Tasks,
            ActiveTab::Chat,
            ActiveTab::Portals,
            ActiveTab::Scheduler,
        ]
    }

    /// Tab label displayed in the top navigation bar.
    pub fn label(&self) -> &'static str {
        match self {
            ActiveTab::Dashboard => "1: Dashboard",
            ActiveTab::Agents => "2: Agents",
            ActiveTab::ActiveRun => "3: Active Run",
            ActiveTab::Events => "4: Events",
            ActiveTab::History => "5: History",
            ActiveTab::Tasks => "6: Tasks & Bench",
            ActiveTab::Chat => "7: Agent Chat",
            ActiveTab::Portals => "8: Portals",
            ActiveTab::Scheduler => "9: Scheduler",
        }
    }
}

/// Lifecycle execution states for persistent agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AgentLifecycleState {
    /// Agent initialized and waiting for tasks.
    #[default]
    Ready,
    /// Agent currently executing an assigned task.
    Running,
    /// Agent paused by user or coordinator.
    Paused,
    /// Agent stopped and inactive.
    Stopped,
    /// Agent encountered an unrecoverable failure.
    Failed,
}

impl AgentLifecycleState {
    /// Formatted string representation of the state.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
            Self::Failed => "Failed",
        }
    }
}

/// Information representing an agent configured in the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentView {
    /// Identifier of the agent.
    pub id: String,
    /// Agent display name.
    pub name: String,
    /// Agent role description.
    pub role: String,
    /// Assigned operational policy or instruction summary.
    pub policy: String,
    /// Current execution state badge.
    pub status: String,
    /// Model identifier configured for this agent.
    pub model: String,
    /// Working directory boundary path.
    pub workspace: String,
    /// Authorized tool names.
    pub tools: Vec<String>,
    /// Number of runs executed by this agent.
    pub runs_count: usize,
}

/// Configuration for a model inference provider portal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPortal {
    /// Unique identifier for this portal.
    pub id: String,
    /// Display name shown in UI tables and selectors.
    pub name: String,
    /// Underlying provider family (e.g., "openai", "anthropic", "ollama", "custom").
    pub provider_kind: String,
    /// Model identifier sent to the provider.
    pub model_name: String,
    /// Custom API base URL if different from provider defaults.
    pub base_url: Option<String>,
    /// Optional explicit API key override.
    pub api_key: Option<String>,
    /// Whether this portal is currently selected for agent execution runs.
    pub is_active: bool,
}

impl ModelPortal {
    /// Create a new portal configuration.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        provider_kind: impl Into<String>,
        model_name: impl Into<String>,
        base_url: Option<String>,
        api_key: Option<String>,
        is_active: bool,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            provider_kind: provider_kind.into(),
            model_name: model_name.into(),
            base_url,
            api_key,
            is_active,
        }
    }

    /// Check if the portal has credentials or local endpoint availability configured.
    pub fn is_configured(&self) -> bool {
        let lower = self.model_name.to_lowercase();
        if lower.starts_with("ollama/") || self.provider_kind == "ollama" {
            return true;
        }
        if lower.starts_with("claude") || self.provider_kind == "anthropic" {
            return self.api_key.is_some()
                || std::env::var("ANTHROPIC_API_KEY").is_ok()
                || std::env::var("anthropic_api_key").is_ok();
        }
        self.api_key.is_some()
            || std::env::var("OPENAI_API_KEY").is_ok()
            || std::env::var("openai_api_key").is_ok()
    }

    /// Status badge text describing the configuration state.
    pub fn status_text(&self) -> &'static str {
        let lower = self.model_name.to_lowercase();
        if lower.starts_with("ollama/") || self.provider_kind == "ollama" {
            "Ready (Local)"
        } else if self.api_key.is_some() {
            "Configured (Custom Key)"
        } else if (lower.starts_with("claude")
            && (std::env::var("ANTHROPIC_API_KEY").is_ok()
                || std::env::var("anthropic_api_key").is_ok()))
            || std::env::var("OPENAI_API_KEY").is_ok()
            || std::env::var("openai_api_key").is_ok()
        {
            "Configured (Env Var)"
        } else {
            "Needs API Key"
        }
    }
}

/// Interactive mode for portal configuration inputs in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PortalInputMode {
    /// Normal navigation mode.
    #[default]
    Normal,
    /// Multi-step form for creating a new custom portal.
    Adding {
        /// Current field being edited.
        field: PortalField,
    },
    /// Editing the API key of the selected portal.
    EditingKey,
    /// Editing the Base URL of the selected portal.
    EditingBaseUrl,
}

/// Form fields for creating a new portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalField {
    /// Display name of the portal.
    Name,
    /// Model identifier.
    Model,
    /// Base endpoint URL.
    BaseUrl,
    /// API authentication key.
    ApiKey,
}

/// Draft values collected during interactive portal creation.
#[derive(Debug, Clone, Default)]
pub struct NewPortalDraft {
    /// Display name.
    pub name: String,
    /// Model name.
    pub model_name: String,
    /// Base URL.
    pub base_url: String,
    /// API key.
    pub api_key: String,
}

/// Role classification for messages displayed in the agent chat interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    /// Message sent by the human user.
    User,
    /// Response or final answer from the autonomous agent.
    Assistant,
    /// Informational system notification or execution lifecycle event.
    System,
    /// Tool invocation or execution observation.
    Tool,
    /// Internal reasoning trace or thinking tokens.
    Thinking,
    /// Error message.
    Error,
}

/// An individual message item displayed in the agent chat interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessageItem {
    /// Role of the message sender.
    pub role: ChatRole,
    /// Message content text.
    pub content: String,
    /// Timestamp when the message was recorded.
    pub timestamp: String,
    /// Whether this message (such as a thinking trace) is expanded in the UI.
    pub is_expanded: bool,
}

impl ChatMessageItem {
    /// Create a new message item with default expansion and current timestamp.
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        let now = chrono::Utc::now().format("%H:%M:%S").to_string();
        Self {
            role,
            content: content.into(),
            timestamp: now,
            is_expanded: false,
        }
    }

    /// Set an explicit timestamp on the message item.
    pub fn with_timestamp(mut self, timestamp: impl Into<String>) -> Self {
        self.timestamp = timestamp.into();
        self
    }

    /// Set whether the item is expanded.
    pub fn with_expanded(mut self, is_expanded: bool) -> Self {
        self.is_expanded = is_expanded;
        self
    }
}

/// Updates emitted by background agent execution worker threads.
#[derive(Debug)]
pub enum ChatAgentUpdate {
    /// Agent execution initiated.
    Started {
        /// Associated run identifier.
        run_id: String,
        /// Model descriptor name.
        model: String,
    },
    /// Incremental token streamed from model.
    Token(String),
    /// Tool execution started.
    ToolStarted {
        /// Tool name.
        name: String,
        /// Arguments json string.
        args: String,
    },
    /// Tool execution completed.
    ToolCompleted {
        /// Tool name.
        name: String,
        /// Output text.
        output: String,
        /// Whether tool resulted in error.
        is_error: bool,
    },
    /// Agent completed normally.
    Completed(Box<AgentRunResult>),
    /// Agent was cancelled.
    Cancelled(String),
    /// Agent encountered a fatal error.
    Error(String),
    /// Inter-agent message event during multi-agent collaboration.
    InterAgentMessage {
        /// Message identifier.
        message_id: String,
        /// Sending agent identifier.
        sender: String,
        /// Receiving agent identifier.
        recipient: String,
        /// Exact routing key.
        routing_key: String,
        /// Event timestamp.
        timestamp: String,
        /// Human-readable payload preview.
        payload_preview: String,
    },
}

/// Status badge and lifecycle state for a team agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamAgentStatus {
    /// Agent is idle and available to receive delegations.
    Ready,
    /// Agent is currently processing a delegated task.
    Running,
    /// Agent has been temporarily paused by operator.
    Paused,
    /// Agent encountered an error or failed.
    Failed,
}

impl TeamAgentStatus {
    /// Return live status badge string per Issue #53.
    pub fn badge(&self) -> &'static str {
        match self {
            TeamAgentStatus::Ready => "● Ready",
            TeamAgentStatus::Running => "⚙ Running",
            TeamAgentStatus::Paused => "⏸ Paused",
            TeamAgentStatus::Failed => "✗ Failed",
        }
    }
}

/// Agent member in a multi-agent team topology block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamAgent {
    /// Unique agent identifier.
    pub id: String,
    /// Agent display name.
    pub name: String,
    /// Agent team role (e.g. supervisor, specialist).
    pub role: String,
    /// Live execution status badge.
    pub status: TeamAgentStatus,
}

/// Status of a workflow stage in the DAG pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageStatus {
    /// Stage has not started yet.
    Pending,
    /// Stage is currently executing.
    Running,
    /// Stage has completed successfully.
    Completed,
    /// Stage failed during execution.
    Failed,
}

impl StageStatus {
    /// Return concise status glyph.
    pub fn glyph(&self) -> &'static str {
        match self {
            StageStatus::Pending => "·",
            StageStatus::Running => "⚙",
            StageStatus::Completed => "✓",
            StageStatus::Failed => "✗",
        }
    }
}

/// Stage in a multi-agent workflow DAG pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowStage {
    /// Unique stage identifier.
    pub id: String,
    /// Assigned agent identifier.
    pub agent: String,
    /// Live stage status.
    pub status: StageStatus,
}

/// Routed inter-agent message item in the live stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterAgentMessageItem {
    /// Message identifier.
    pub id: String,
    /// Formatted timestamp (e.g. "10:14:22").
    pub timestamp: String,
    /// Sending agent.
    pub sender: String,
    /// Receiving agent.
    pub recipient: String,
    /// Topic or routing key.
    pub routing_key: String,
    /// Content or instruction text.
    pub content: String,
}

/// State tracking multi-agent coordination, topology, pipeline, and message feed.
#[derive(Debug, Clone)]
pub struct MultiAgentCoordination {
    /// Registered team agents (manager, researcher, coder, reviewer).
    pub team_agents: Vec<TeamAgent>,
    /// Workflow stages in pipeline DAG order (research -> implementation -> review).
    pub pipeline_stages: Vec<WorkflowStage>,
    /// Live inter-agent routed message timeline.
    pub message_feed: Vec<InterAgentMessageItem>,
    /// Whether the multi-agent coordination viewer is displayed.
    pub show_viewer: bool,
    /// Selected agent filter for message feed (None = All).
    pub agent_filter: Option<String>,
    /// Selected stage filter for message feed (None = All).
    pub stage_filter: Option<String>,
    /// Scroll offset within the inter-agent message feed.
    pub feed_scroll: usize,
}

impl Default for MultiAgentCoordination {
    fn default() -> Self {
        Self {
            team_agents: vec![
                TeamAgent {
                    id: "manager".to_string(),
                    name: "Manager".to_string(),
                    role: "supervisor".to_string(),
                    status: TeamAgentStatus::Ready,
                },
                TeamAgent {
                    id: "researcher".to_string(),
                    name: "Researcher".to_string(),
                    role: "specialist".to_string(),
                    status: TeamAgentStatus::Ready,
                },
                TeamAgent {
                    id: "coder".to_string(),
                    name: "Coder".to_string(),
                    role: "specialist".to_string(),
                    status: TeamAgentStatus::Ready,
                },
                TeamAgent {
                    id: "reviewer".to_string(),
                    name: "Reviewer".to_string(),
                    role: "specialist".to_string(),
                    status: TeamAgentStatus::Ready,
                },
            ],
            pipeline_stages: vec![
                WorkflowStage {
                    id: "research".to_string(),
                    agent: "researcher".to_string(),
                    status: StageStatus::Pending,
                },
                WorkflowStage {
                    id: "implementation".to_string(),
                    agent: "coder".to_string(),
                    status: StageStatus::Pending,
                },
                WorkflowStage {
                    id: "review".to_string(),
                    agent: "reviewer".to_string(),
                    status: StageStatus::Pending,
                },
            ],
            message_feed: Vec::new(),
            show_viewer: false,
            agent_filter: None,
            stage_filter: None,
            feed_scroll: 0,
        }
    }
}

impl MultiAgentCoordination {
    /// Record a routed inter-agent message, capping queue depth to prevent memory leaks.
    pub fn record_message(&mut self, item: InterAgentMessageItem) {
        // Update agent status based on interaction
        for agent in &mut self.team_agents {
            if agent.id == item.recipient {
                agent.status = TeamAgentStatus::Running;
            } else if agent.id == item.sender && agent.status == TeamAgentStatus::Running {
                agent.status = TeamAgentStatus::Ready;
            }
        }

        // Update pipeline stages based on messages
        let content_lower = item.content.to_lowercase();
        if item.sender == "researcher"
            || content_lower.contains("failing")
            || content_lower.contains("research")
        {
            if let Some(stage) = self.pipeline_stages.iter_mut().find(|s| s.id == "research") {
                stage.status = StageStatus::Completed;
            }
            if let Some(stage) = self
                .pipeline_stages
                .iter_mut()
                .find(|s| s.id == "implementation")
            {
                if stage.status == StageStatus::Pending {
                    stage.status = StageStatus::Running;
                }
            }
        }
        if item.sender == "coder"
            || content_lower.contains("patch")
            || content_lower.contains("implement")
        {
            if let Some(stage) = self
                .pipeline_stages
                .iter_mut()
                .find(|s| s.id == "implementation")
            {
                stage.status = StageStatus::Completed;
            }
            if let Some(stage) = self.pipeline_stages.iter_mut().find(|s| s.id == "review") {
                if stage.status == StageStatus::Pending {
                    stage.status = StageStatus::Running;
                }
            }
        }
        if item.sender == "reviewer"
            || content_lower.contains("verify")
            || content_lower.contains("lgtm")
            || content_lower.contains("approved")
        {
            if let Some(stage) = self.pipeline_stages.iter_mut().find(|s| s.id == "review") {
                stage.status = StageStatus::Completed;
            }
            for agent in &mut self.team_agents {
                if agent.status == TeamAgentStatus::Running {
                    agent.status = TeamAgentStatus::Ready;
                }
            }
        }

        if self.message_feed.len() >= 1000 {
            self.message_feed.remove(0);
        }
        self.message_feed.push(item);
    }

    /// Cycle through active agent filters: None -> manager -> researcher -> coder -> reviewer -> None
    pub fn cycle_agent_filter(&mut self) {
        let agents = ["manager", "researcher", "coder", "reviewer"];
        self.agent_filter = match &self.agent_filter {
            None => Some(agents[0].to_string()),
            Some(curr) => {
                let idx = agents.iter().position(|a| a == curr);
                match idx {
                    Some(i) if i + 1 < agents.len() => Some(agents[i + 1].to_string()),
                    _ => None,
                }
            }
        };
    }

    /// Cycle through stage filters: None -> research -> implementation -> review -> None
    pub fn cycle_stage_filter(&mut self) {
        let stages = ["research", "implementation", "review"];
        self.stage_filter = match &self.stage_filter {
            None => Some(stages[0].to_string()),
            Some(curr) => {
                let idx = stages.iter().position(|s| s == curr);
                match idx {
                    Some(i) if i + 1 < stages.len() => Some(stages[i + 1].to_string()),
                    _ => None,
                }
            }
        };
    }

    /// Return filtered references to messages based on active filters.
    pub fn filtered_messages(&self) -> Vec<&InterAgentMessageItem> {
        self.message_feed
            .iter()
            .filter(|msg| {
                if let Some(agent) = &self.agent_filter {
                    if &msg.sender != agent && &msg.recipient != agent {
                        return false;
                    }
                }
                if let Some(stage) = &self.stage_filter {
                    let routing = &msg.routing_key;
                    let content = &msg.content;
                    if !routing.contains(stage) && !content.to_lowercase().contains(stage) {
                        return false;
                    }
                }
                true
            })
            .collect()
    }
}

/// Floating autocomplete popup state for slash commands.
#[derive(Debug, Clone, Default)]
pub struct AutocompleteState {
    /// Whether the autocomplete popup is currently displayed.
    pub is_open: bool,
    /// List of command definitions matching current query.
    pub matches: Vec<&'static crate::commands::CommandDefinition>,
    /// Index of currently highlighted match.
    pub selected_idx: usize,
}

/// Current execution phase of an autonomous agent lifecycle for visualizer HUD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentExecutionPhase {
    /// Agent is idle and standing by for user prompt.
    #[default]
    Idle,
    /// Model provider is active (thinking, streaming reasoning tokens).
    Planning,
    /// Evaluating security approval policies (HITL gate check).
    ApprovalGate,
    /// Executing a tool invocation within sandbox boundaries.
    ToolExecution,
    /// Ingesting tool output and updating context memory.
    Ingestion,
    /// Execution completed successfully.
    Completed,
    /// Execution failed or was interrupted/cancelled.
    Failed,
}

impl AgentExecutionPhase {
    /// Human-readable label for HUD display.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "IDLE · STANDBY",
            Self::Planning => "THINKING & PLANNING",
            Self::ApprovalGate => "SECURITY APPROVAL GATE",
            Self::ToolExecution => "TOOL EXECUTION",
            Self::Ingestion => "INGESTING RESULTS",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED / CANCELLED",
        }
    }
}

/// Status of an individual tool invocation recorded in the visualizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualizerToolStatus {
    /// Tool is currently executing.
    Running,
    /// Tool completed successfully.
    Success,
    /// Tool execution returned an error.
    Error,
}

/// Recorded tool execution entry for visualizer telemetry.
#[derive(Debug, Clone)]
pub struct VisualizerToolEvent {
    /// Name of the tool invoked.
    pub name: String,
    /// Single-line summary of tool arguments.
    pub args_summary: String,
    /// Outcome status of the tool execution.
    pub status: VisualizerToolStatus,
    /// Formatted wall-clock timestamp of invocation.
    pub timestamp: String,
    /// Truncated preview snippet of tool output.
    pub result_preview: Option<String>,
}

/// State tracking the live agent mission control visualizer HUD.
#[derive(Debug, Clone)]
pub struct LiveAgentVisualizer {
    /// Whether the visualizer HUD modal is currently open.
    pub is_open: bool,
    /// Current lifecycle phase.
    pub phase: AgentExecutionPhase,
    /// Current execution step / iteration counter.
    pub current_step: usize,
    /// Step budget limit.
    pub max_steps: usize,
    /// Currently executing tool name, if any.
    pub active_tool: Option<String>,
    /// Arguments summary for active tool.
    pub active_tool_args: Option<String>,
    /// Summary of the last tool result.
    pub last_tool_result: Option<String>,
    /// Chronological list of tool events during current session.
    pub tool_history: Vec<VisualizerToolEvent>,
    /// Total tokens streamed in current task.
    pub tokens_streamed: usize,
    /// Live snippet of model thought or output.
    pub live_stream_preview: String,
    /// Sub-tab within visualizer HUD (0 = Overview/Pipeline, 1 = Tool Calls, 2 = Security Boundaries).
    pub sub_tab: usize,
}

impl Default for LiveAgentVisualizer {
    fn default() -> Self {
        Self {
            is_open: false,
            phase: AgentExecutionPhase::Idle,
            current_step: 0,
            max_steps: 25,
            active_tool: None,
            active_tool_args: None,
            last_tool_result: None,
            tool_history: Vec::new(),
            tokens_streamed: 0,
            live_stream_preview: String::new(),
            sub_tab: 0,
        }
    }
}

impl LiveAgentVisualizer {
    /// Toggle visibility of the HUD visualizer modal.
    pub fn toggle(&mut self) {
        self.is_open = !self.is_open;
    }

    /// Explicitly close the HUD visualizer.
    pub fn close(&mut self) {
        self.is_open = false;
    }

    /// Cycle through HUD sub-tabs: 0 (Pipeline) -> 1 (Tools) -> 2 (Security) -> 0.
    pub fn cycle_tab(&mut self) {
        self.sub_tab = (self.sub_tab + 1) % 3;
    }

    /// Reset live counters upon starting a new execution run.
    pub fn on_run_started(&mut self, _run_id: &str, _model: &str) {
        self.phase = AgentExecutionPhase::Planning;
        self.current_step = 1;
        self.active_tool = None;
        self.active_tool_args = None;
        self.last_tool_result = None;
        self.tokens_streamed = 0;
        self.live_stream_preview.clear();
        self.tool_history.clear();
    }

    /// Ingest a streaming token into the live preview.
    pub fn on_token(&mut self, token: &str) {
        self.phase = AgentExecutionPhase::Planning;
        self.tokens_streamed = self.tokens_streamed.saturating_add(1);
        self.live_stream_preview.push_str(token);
        if self.live_stream_preview.len() > 600 {
            let keep_start = self.live_stream_preview.len() - 500;
            self.live_stream_preview = self.live_stream_preview[keep_start..].to_string();
        }
    }

    /// Ingest a tool execution started event.
    pub fn on_tool_started(&mut self, name: &str, args: &str) {
        self.phase = AgentExecutionPhase::ToolExecution;
        self.active_tool = Some(name.to_string());
        let summary = args.lines().next().unwrap_or(args).trim().to_string();
        self.active_tool_args = Some(summary.clone());
        let now = chrono::Utc::now().format("%H:%M:%S").to_string();
        self.tool_history.push(VisualizerToolEvent {
            name: name.to_string(),
            args_summary: summary,
            status: VisualizerToolStatus::Running,
            timestamp: now,
            result_preview: None,
        });
        if self.tool_history.len() > 50 {
            self.tool_history.remove(0);
        }
    }

    /// Ingest a tool execution completed event.
    pub fn on_tool_completed(&mut self, _name: &str, output: &str, is_error: bool) {
        self.phase = AgentExecutionPhase::Ingestion;
        let preview = output.lines().next().unwrap_or(output).trim().to_string();
        self.last_tool_result = Some(preview.clone());
        if let Some(last) = self.tool_history.last_mut() {
            last.status = if is_error {
                VisualizerToolStatus::Error
            } else {
                VisualizerToolStatus::Success
            };
            last.result_preview = Some(preview);
        }
        self.current_step = (self.current_step + 1).min(self.max_steps);
    }

    /// Ingest run completed event.
    pub fn on_completed(&mut self) {
        self.phase = AgentExecutionPhase::Completed;
        self.active_tool = None;
    }

    /// Ingest run failed or cancelled event.
    pub fn on_cancelled(&mut self) {
        self.phase = AgentExecutionPhase::Failed;
        self.active_tool = None;
    }
}

/// Execution handle for an asynchronous agent execution thread.
pub struct AgentExecutionHandle {
    /// Cancellation token to stop the running agent.
    pub cancel_token: CancellationToken,
    /// Channel receiver for agent updates and terminal outcomes.
    pub receiver: Receiver<ChatAgentUpdate>,
    /// Unique run ID assigned to this execution.
    pub run_id: Option<String>,
}

/// Query the current git repository branch and dirty status, if in a git directory.
pub fn detect_git_branch_status() -> Option<String> {
    let branch_output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;

    if !branch_output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();
    if branch.is_empty() || branch == "HEAD" {
        return None;
    }

    let status_output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok();

    let is_dirty = match status_output {
        Some(out) if out.status.success() => !out.stdout.is_empty(),
        _ => false,
    };

    if is_dirty {
        Some(format!("git: {}*", branch))
    } else {
        Some(format!("git: {}", branch))
    }
}

/// Primary application state container for the TUI client.
pub struct App {
    /// Currently focused navigation tab.
    pub active_tab: ActiveTab,
    /// Persistent SQLite storage client.
    pub store: Option<Arc<RunStore>>,
    /// Retrieved execution runs from storage.
    pub runs: Vec<RunSummary>,
    /// Index of currently selected run in tables.
    pub selected_run_idx: usize,
    /// Structured event stream for selected run.
    pub events: Vec<EventRecord>,
    /// Index of currently selected event in the log viewer.
    pub selected_event_idx: usize,
    /// Benchmark tasks available in the system.
    pub tasks: Vec<BenchmarkTask>,
    /// Index of currently selected benchmark task.
    pub selected_task_idx: usize,
    /// List of configured agents.
    pub agents: Vec<AgentView>,
    /// Index of currently selected agent.
    pub selected_agent_idx: usize,
    /// Flag indicating whether the TUI loop should exit.
    pub should_quit: bool,
    /// Transient status or notification message.
    pub status_message: Option<String>,

    // Chat interface state
    /// Recorded conversation and execution stream items.
    pub chat_messages: Vec<ChatMessageItem>,
    /// Current prompt text in the input box.
    pub chat_input: String,
    /// Insertion cursor position within the chat prompt input buffer.
    pub chat_cursor: usize,
    /// Prompt and slash command input history.
    pub chat_history: Vec<String>,
    /// Active index within the command history during Up/Down browsing.
    pub chat_history_idx: Option<usize>,
    /// Flag indicating whether the chat should automatically pin scroll to the newest message.
    pub chat_auto_scroll: bool,
    /// Global expansion state for thinking traces.
    pub thinking_expanded: bool,
    /// Global expansion state for tool call execution outputs.
    pub tool_calls_expanded: bool,
    /// Git repository branch and status indicator, if available.
    pub git_branch_info: Option<String>,
    /// Message stream scroll offset when manual scrolling is active.
    pub chat_scroll: usize,
    /// Whether an agent task is actively executing in background.
    pub chat_is_running: bool,
    /// Active agent background execution handle.
    pub chat_handle: Option<AgentExecutionHandle>,
    /// Highest event sequence number already rendered in chat stream.
    pub chat_last_event_seq: u64,
    /// Animation tick counter for shimmer and spinner frames.
    pub anim_tick: usize,
    /// Instant when current agent execution started.
    pub run_start_instant: Option<std::time::Instant>,
    /// Accumulated tokens consumed in the current session.
    pub session_tokens: u64,
    /// Accumulated estimated cost in USD for the current session.
    pub session_cost: f64,
    /// Current whimsical verb displayed during thinking/loading.
    pub current_verb: String,

    // Portals configuration state
    /// List of configured model portals.
    pub portals: Vec<ModelPortal>,
    /// Index of selected portal in table.
    pub selected_portal_idx: usize,
    /// Active interactive input mode in portals tab.
    pub portal_input_mode: PortalInputMode,
    /// Text buffer for editing portal fields.
    pub portal_input_buffer: String,
    /// Draft state for multi-step portal creation.
    pub new_portal_draft: NewPortalDraft,

    // Autocomplete & Indexing
    /// Floating autocomplete popup state for slash commands.
    pub autocomplete_state: AutocompleteState,
    /// Background workspace indexer.
    pub indexer: crate::indexer::BackgroundIndexer,

    // Multi-Agent Coordination
    /// Live multi-agent team coordination, topology, DAG pipeline, and message feed.
    pub coordination: MultiAgentCoordination,

    // Scheduler & Background Job State
    /// Registered scheduled jobs loaded from storage.
    pub cron_jobs: Vec<cortex_runtime::ScheduledJob>,
    /// Index of currently selected scheduled job.
    pub selected_cron_job_idx: usize,
    /// Execution runs recorded for the selected scheduled job.
    pub cron_job_runs: Vec<cortex_runtime::JobRunRecord>,
    /// Index of currently selected execution run in drawer.
    pub selected_cron_run_idx: usize,
    /// Whether the execution run drawer is expanded / focused.
    pub cron_drawer_open: bool,

    // Live Agent Visualizer HUD
    /// Live mission control visualizer HUD state.
    pub visualizer: LiveAgentVisualizer,
}

impl App {
    /// Create a new [`App`] initialized with an optional storage handle.
    pub fn new(store: Option<Arc<RunStore>>) -> Self {
        let ws = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());

        let mut app = Self {
            active_tab: ActiveTab::Chat,
            store,
            runs: Vec::new(),
            selected_run_idx: 0,
            events: Vec::new(),
            selected_event_idx: 0,
            tasks: Vec::new(),
            selected_task_idx: 0,
            agents: vec![
                AgentView {
                    id: "coder-01".to_string(),
                    name: "Coding Agent".to_string(),
                    role: "Autonomous Bug Fixing & Implementation".to_string(),
                    policy: "Inspect files before modifying, write minimal tests, verify with cargo test".to_string(),
                    status: "Ready".to_string(),
                    model: "gpt-4o-mini".to_string(),
                    workspace: ws.clone(),
                    tools: vec![
                        "read_file".into(),
                        "write_file".into(),
                        "list_dir".into(),
                        "shell".into(),
                        "git_*".into(),
                    ],
                    runs_count: 0,
                },
                AgentView {
                    id: "eval-01".to_string(),
                    name: "Benchmark Evaluator".to_string(),
                    role: "Deterministic Benchmark & Test Runner".to_string(),
                    policy: "Executes reproducible evaluation suites and collects ground-truth metrics".to_string(),
                    status: "Ready".to_string(),
                    model: "gpt-4o".to_string(),
                    workspace: "Isolated".to_string(),
                    tools: vec!["read_file".into(), "shell".into()],
                    runs_count: 0,
                },
                AgentView {
                    id: "reviewer-01".to_string(),
                    name: "Policy Reviewer".to_string(),
                    role: "PR Policy & Security Compliance".to_string(),
                    policy: "Audits git diffs, detects secret leaks, enforces code conventions".to_string(),
                    status: "Paused".to_string(),
                    model: "claude-3-5-sonnet".to_string(),
                    workspace: "Read-Only".to_string(),
                    tools: vec!["read_file".into(), "git_diff".into(), "git_log".into()],
                    runs_count: 0,
                },
            ],
            selected_agent_idx: 0,
            should_quit: false,
            status_message: Some("Cortex Harness active. Type /help for slash commands, 'q' to quit.".to_string()),

            chat_messages: Vec::new(),
            chat_input: String::new(),
            chat_cursor: 0,
            chat_history: Vec::new(),
            chat_history_idx: None,
            chat_auto_scroll: true,
            thinking_expanded: false,
            tool_calls_expanded: false,
            git_branch_info: detect_git_branch_status(),
            chat_scroll: 0,
            chat_is_running: false,
            chat_handle: None,
            chat_last_event_seq: 0,
            anim_tick: 0,
            run_start_instant: None,
            session_tokens: 0,
            session_cost: 0.0,
            current_verb: "Percolating...".to_string(),

            portals: Self::default_portals(),
            selected_portal_idx: 0,
            portal_input_mode: PortalInputMode::Normal,
            portal_input_buffer: String::new(),
            new_portal_draft: NewPortalDraft::default(),

            autocomplete_state: AutocompleteState::default(),
            indexer: {
                let idx = crate::indexer::BackgroundIndexer::new();
                let ws_buf = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                idx.start_indexing(ws_buf);
                idx
            },
            coordination: MultiAgentCoordination::default(),
            cron_jobs: Vec::new(),
            selected_cron_job_idx: 0,
            cron_job_runs: Vec::new(),
            selected_cron_run_idx: 0,
            cron_drawer_open: false,
            visualizer: LiveAgentVisualizer::default(),
        };

        // Ensure user settings file exists in ~/.cortex
        let _ = cortex_core::settings::UserSettings::load_or_create();

        app.load_tasks();
        app.refresh();
        app
    }

    /// Default set of preconfigured model portals seeded with user settings.
    pub fn default_portals() -> Vec<ModelPortal> {
        let settings = cortex_core::settings::UserSettings::load_or_default();
        let target_model = settings.model.clone();

        let mut portals = vec![
            ModelPortal::new(
                "openai-gpt4o-mini",
                "OpenAI GPT-4o-mini",
                "openai",
                "gpt-4o-mini",
                None,
                None,
                false,
            ),
            ModelPortal::new(
                "openai-gpt4o",
                "OpenAI GPT-4o",
                "openai",
                "gpt-4o",
                None,
                None,
                false,
            ),
            ModelPortal::new(
                "anthropic-sonnet",
                "Anthropic Claude 3.5 Sonnet",
                "anthropic",
                "claude-3-5-sonnet-20241022",
                None,
                None,
                false,
            ),
            ModelPortal::new(
                "anthropic-haiku",
                "Anthropic Claude 3.5 Haiku",
                "anthropic",
                "claude-3-5-haiku-20241022",
                None,
                None,
                false,
            ),
            ModelPortal::new(
                "ollama-llama3",
                "Ollama LLaMA 3.1 (Local)",
                "ollama",
                "ollama/llama3.1",
                Some("http://localhost:11434/v1".to_string()),
                None,
                false,
            ),
            ModelPortal::new(
                "ollama-qwen",
                "Ollama Qwen 2.5 Coder (Local)",
                "ollama",
                "ollama/qwen2.5-coder",
                Some("http://localhost:11434/v1".to_string()),
                None,
                false,
            ),
            ModelPortal::new(
                "deepseek-chat",
                "DeepSeek Chat",
                "openai",
                "deepseek-chat",
                Some("https://api.deepseek.com/v1".to_string()),
                None,
                false,
            ),
        ];

        // Seed API keys and base URLs from settings
        for portal in &mut portals {
            if portal.api_key.is_none() {
                portal.api_key = settings.resolve_api_key(&portal.model_name);
            }
            if portal.base_url.is_none() {
                portal.base_url = settings.resolve_base_url(&portal.model_name);
            }
        }

        // Activate the portal that matches target_model
        let mut found = false;
        for portal in &mut portals {
            if portal.model_name.eq_ignore_ascii_case(&target_model)
                || portal.id.eq_ignore_ascii_case(&target_model)
            {
                portal.is_active = true;
                found = true;
                break;
            }
        }

        if !found {
            // Add custom configured portal
            let provider = if target_model.to_lowercase().starts_with("claude") {
                "anthropic"
            } else if target_model.to_lowercase().starts_with("ollama/") {
                "ollama"
            } else {
                "openai"
            };
            let custom_portal = ModelPortal::new(
                "custom-settings-model",
                format!("Configured ({})", target_model),
                provider,
                &target_model,
                settings.resolve_base_url(&target_model),
                settings.resolve_api_key(&target_model),
                true,
            );
            portals.insert(0, custom_portal);
        }

        portals
    }

    /// Load benchmark tasks from the harness suite definitions.
    pub fn load_tasks(&mut self) {
        let mut tasks = get_suite_tasks("coding");
        tasks.extend(get_suite_tasks("refactor"));
        tasks.extend(get_suite_tasks("cli"));
        self.tasks = tasks;
    }

    /// Refresh state by querying the persistent [`RunStore`].
    pub fn refresh(&mut self) {
        self.git_branch_info = detect_git_branch_status();
        if let Some(store) = &self.store {
            if let Ok(runs) = store.list_runs(50) {
                self.runs = runs;
                if self.selected_run_idx >= self.runs.len() && !self.runs.is_empty() {
                    self.selected_run_idx = self.runs.len() - 1;
                }
            }

            self.refresh_events();
            self.refresh_cron_jobs();
        }
    }

    /// Refresh event stream for the currently selected run.
    pub fn refresh_events(&mut self) {
        if let Some(store) = &self.store {
            if let Some(run) = self.runs.get(self.selected_run_idx) {
                if let Ok(events) = store.get_events(&run.id) {
                    self.events = events;
                    if self.selected_event_idx >= self.events.len() && !self.events.is_empty() {
                        self.selected_event_idx = self.events.len() - 1;
                    }
                } else {
                    self.events.clear();
                }
            } else {
                self.events.clear();
            }
        }
    }

    /// Refresh registered cron jobs from SQLite and sync execution runs.
    pub fn refresh_cron_jobs(&mut self) {
        if let Some(store) = &self.store {
            if let Ok(jobs) = store.list_cron_jobs() {
                self.cron_jobs = jobs;
                if self.selected_cron_job_idx >= self.cron_jobs.len() && !self.cron_jobs.is_empty()
                {
                    self.selected_cron_job_idx = self.cron_jobs.len() - 1;
                }
            }
            self.refresh_cron_job_runs();
        }
    }

    /// Refresh execution runs for the currently selected scheduled job.
    pub fn refresh_cron_job_runs(&mut self) {
        if let Some(store) = &self.store {
            if let Some(job) = self.cron_jobs.get(self.selected_cron_job_idx) {
                if let Ok(runs) = store.list_cron_job_runs(&job.id, 50) {
                    self.cron_job_runs = runs;
                    if self.selected_cron_run_idx >= self.cron_job_runs.len()
                        && !self.cron_job_runs.is_empty()
                    {
                        self.selected_cron_run_idx = self.cron_job_runs.len() - 1;
                    }
                } else {
                    self.cron_job_runs.clear();
                }
            } else {
                self.cron_job_runs.clear();
            }
        }
    }

    /// Switch to next navigation tab.
    pub fn next_tab(&mut self) {
        let tabs = ActiveTab::all();
        let current_pos = tabs.iter().position(|t| *t == self.active_tab).unwrap_or(0);
        let next_pos = (current_pos + 1) % tabs.len();
        self.active_tab = tabs[next_pos];
    }

    /// Switch to previous navigation tab.
    pub fn prev_tab(&mut self) {
        let tabs = ActiveTab::all();
        let current_pos = tabs.iter().position(|t| *t == self.active_tab).unwrap_or(0);
        let prev_pos = if current_pos == 0 {
            tabs.len() - 1
        } else {
            current_pos - 1
        };
        self.active_tab = tabs[prev_pos];
    }

    /// Set navigation tab directly.
    pub fn set_tab(&mut self, tab: ActiveTab) {
        self.active_tab = tab;
    }

    /// Navigate to next item in current tab view.
    pub fn next_item(&mut self) {
        match self.active_tab {
            ActiveTab::History | ActiveTab::Dashboard => {
                if !self.runs.is_empty() && self.selected_run_idx + 1 < self.runs.len() {
                    self.selected_run_idx += 1;
                    self.refresh_events();
                }
            }
            ActiveTab::Events => {
                if !self.events.is_empty() && self.selected_event_idx + 1 < self.events.len() {
                    self.selected_event_idx += 1;
                }
            }
            ActiveTab::Tasks => {
                if !self.tasks.is_empty() && self.selected_task_idx + 1 < self.tasks.len() {
                    self.selected_task_idx += 1;
                }
            }
            ActiveTab::Agents => {
                if !self.agents.is_empty() && self.selected_agent_idx + 1 < self.agents.len() {
                    self.selected_agent_idx += 1;
                }
            }
            ActiveTab::Portals => {
                self.next_portal();
            }
            ActiveTab::Chat => {
                self.chat_scroll_down();
            }
            ActiveTab::Scheduler => {
                if self.cron_drawer_open {
                    if !self.cron_job_runs.is_empty()
                        && self.selected_cron_run_idx + 1 < self.cron_job_runs.len()
                    {
                        self.selected_cron_run_idx += 1;
                    }
                } else if !self.cron_jobs.is_empty()
                    && self.selected_cron_job_idx + 1 < self.cron_jobs.len()
                {
                    self.selected_cron_job_idx += 1;
                    self.refresh_cron_job_runs();
                }
            }
            ActiveTab::ActiveRun => {}
        }
    }

    /// Navigate to previous item in current tab view.
    pub fn prev_item(&mut self) {
        match self.active_tab {
            ActiveTab::History | ActiveTab::Dashboard => {
                if self.selected_run_idx > 0 {
                    self.selected_run_idx -= 1;
                    self.refresh_events();
                }
            }
            ActiveTab::Events => {
                if self.selected_event_idx > 0 {
                    self.selected_event_idx -= 1;
                }
            }
            ActiveTab::Tasks => {
                if self.selected_task_idx > 0 {
                    self.selected_task_idx -= 1;
                }
            }
            ActiveTab::Agents => {
                if self.selected_agent_idx > 0 {
                    self.selected_agent_idx -= 1;
                }
            }
            ActiveTab::Portals => {
                self.prev_portal();
            }
            ActiveTab::Chat => {
                self.chat_scroll_up();
            }
            ActiveTab::Scheduler => {
                if self.cron_drawer_open {
                    if self.selected_cron_run_idx > 0 {
                        self.selected_cron_run_idx -= 1;
                    }
                } else if self.selected_cron_job_idx > 0 {
                    self.selected_cron_job_idx -= 1;
                    self.refresh_cron_job_runs();
                }
            }
            ActiveTab::ActiveRun => {}
        }
    }

    /// Action triggered by pressing Enter in item tables.
    pub fn select_current(&mut self) {
        match self.active_tab {
            ActiveTab::Dashboard | ActiveTab::History if !self.runs.is_empty() => {
                self.active_tab = ActiveTab::ActiveRun;
            }
            ActiveTab::Portals => {
                self.activate_selected_portal();
            }
            ActiveTab::Scheduler => {
                self.toggle_cron_drawer();
            }
            _ => {}
        }
    }

    /// Toggle the execution run drawer for the selected scheduled job.
    pub fn toggle_cron_drawer(&mut self) {
        self.cron_drawer_open = !self.cron_drawer_open;
        if self.cron_drawer_open {
            self.refresh_cron_job_runs();
        }
    }

    /// Close the execution run drawer.
    pub fn close_cron_drawer(&mut self) {
        self.cron_drawer_open = false;
    }

    /// Currently selected scheduled job, if any.
    pub fn selected_cron_job(&self) -> Option<&cortex_runtime::ScheduledJob> {
        self.cron_jobs.get(self.selected_cron_job_idx)
    }

    /// Currently selected execution run record, if any.
    pub fn selected_cron_run(&self) -> Option<&cortex_runtime::JobRunRecord> {
        self.cron_job_runs.get(self.selected_cron_run_idx)
    }

    /// Toggle pause / resume status of the selected scheduled job in SQLite.
    pub fn toggle_pause_selected_cron_job(&mut self) {
        if let Some(store) = &self.store {
            if let Some(job) = self.cron_jobs.get(self.selected_cron_job_idx) {
                let engine = cortex_runtime::SchedulerEngine::new(Arc::clone(store));
                let res = match job.status {
                    cortex_runtime::JobStatus::Paused => engine.resume_job(&job.id),
                    _ => engine.pause_job(&job.id),
                };
                if let Ok(updated) = res {
                    if updated {
                        let new_state = if job.status == cortex_runtime::JobStatus::Paused {
                            "resumed"
                        } else {
                            "paused"
                        };
                        self.status_message = Some(format!("Job '{}' {}.", job.name, new_state));
                        self.refresh_cron_jobs();
                    }
                }
            }
        }
    }

    /// Trigger the selected scheduled job immediately.
    pub fn trigger_selected_cron_job(&mut self) {
        if let Some(store) = &self.store {
            if let Some(job) = self.cron_jobs.get(self.selected_cron_job_idx) {
                let engine = cortex_runtime::SchedulerEngine::new(Arc::clone(store));
                match engine.trigger_job(&job.id, chrono::Utc::now()) {
                    Ok(res) => {
                        self.status_message =
                            Some(format!("Triggered job '{}': {:?}", job.name, res));
                        self.refresh_cron_jobs();
                    }
                    Err(e) => {
                        self.status_message = format!("Failed to trigger job: {}", e).into();
                    }
                }
            }
        }
    }

    /// Resolve the target agent identifier for a scheduled job.
    pub fn resolve_target_agent(&self, job: &cortex_runtime::ScheduledJob) -> String {
        if let Some(agent_id) = &job.agent_id {
            return agent_id.as_str().to_string();
        }
        if let Some(rest) = job.prompt.strip_prefix('@') {
            if let Some(target) = rest.split_whitespace().next() {
                return target.to_string();
            }
        }
        if let Some(agent) = self.agents.first() {
            agent.id.clone()
        } else {
            "default".to_string()
        }
    }

    /// Format human-readable countdown string until the next fire timestamp.
    pub fn format_fire_countdown(
        next_run_at: Option<chrono::DateTime<chrono::Utc>>,
        status: cortex_runtime::JobStatus,
        now: chrono::DateTime<chrono::Utc>,
    ) -> String {
        match status {
            cortex_runtime::JobStatus::Paused => "Paused".to_string(),
            cortex_runtime::JobStatus::Completed => "Completed".to_string(),
            cortex_runtime::JobStatus::Active => match next_run_at {
                None => "-".to_string(),
                Some(target) => {
                    if target <= now {
                        "Due now".to_string()
                    } else {
                        let diff = target - now;
                        let days = diff.num_days();
                        let hours = diff.num_hours();
                        let minutes = diff.num_minutes();
                        let seconds = diff.num_seconds();

                        if days > 0 {
                            format!("in {}d {}h", days, hours % 24)
                        } else if hours > 0 {
                            format!("in {}h {}m", hours, minutes % 60)
                        } else if minutes > 0 {
                            format!("in {}m {}s", minutes, seconds % 60)
                        } else {
                            format!("in {}s", seconds.max(0))
                        }
                    }
                }
            },
        }
    }

    /// Currently selected run summary, if any.
    pub fn selected_run(&self) -> Option<&RunSummary> {
        self.runs.get(self.selected_run_idx)
    }

    /// Currently selected event record, if any.
    pub fn selected_event(&self) -> Option<&EventRecord> {
        self.events.get(self.selected_event_idx)
    }

    // ==========================================
    // Chat & Autonomous Agent Execution Methods
    // ==========================================

    /// Reference to the currently active [`ModelPortal`].
    pub fn active_portal(&self) -> &ModelPortal {
        self.portals
            .iter()
            .find(|p| p.is_active)
            .unwrap_or(&self.portals[0])
    }

    /// Insert a character into the chat input buffer at current cursor position.
    pub fn chat_input_insert(&mut self, c: char) {
        if self.chat_cursor >= self.chat_input.len() {
            self.chat_input.push(c);
            self.chat_cursor = self.chat_input.len();
        } else {
            self.chat_input.insert(self.chat_cursor, c);
            self.chat_cursor += 1;
        }
        self.update_autocomplete();
    }

    /// Delete character immediately preceding the cursor in chat input.
    pub fn chat_input_backspace(&mut self) {
        if self.chat_cursor > 0 && !self.chat_input.is_empty() {
            self.chat_cursor -= 1;
            if self.chat_cursor < self.chat_input.len() {
                self.chat_input.remove(self.chat_cursor);
            }
        }
        self.update_autocomplete();
    }

    /// Delete character at current cursor position in chat input.
    pub fn chat_input_delete(&mut self) {
        if self.chat_cursor < self.chat_input.len() {
            self.chat_input.remove(self.chat_cursor);
        }
        self.update_autocomplete();
    }

    /// Update floating autocomplete state based on current input buffer.
    pub fn update_autocomplete(&mut self) {
        if self.chat_input.starts_with('/') && !self.chat_input.contains(' ') {
            let matches = crate::commands::search_commands(&self.chat_input);
            if !matches.is_empty() {
                self.autocomplete_state.is_open = true;
                self.autocomplete_state.matches = matches;
                if self.autocomplete_state.selected_idx >= self.autocomplete_state.matches.len() {
                    self.autocomplete_state.selected_idx = 0;
                }
                return;
            }
        }
        self.autocomplete_state.is_open = false;
        self.autocomplete_state.matches.clear();
        self.autocomplete_state.selected_idx = 0;
    }

    /// Select next item in autocomplete popup.
    pub fn autocomplete_next(&mut self) {
        if self.autocomplete_state.is_open && !self.autocomplete_state.matches.is_empty() {
            self.autocomplete_state.selected_idx =
                (self.autocomplete_state.selected_idx + 1) % self.autocomplete_state.matches.len();
        }
    }

    /// Select previous item in autocomplete popup.
    pub fn autocomplete_prev(&mut self) {
        if self.autocomplete_state.is_open && !self.autocomplete_state.matches.is_empty() {
            if self.autocomplete_state.selected_idx == 0 {
                self.autocomplete_state.selected_idx = self.autocomplete_state.matches.len() - 1;
            } else {
                self.autocomplete_state.selected_idx -= 1;
            }
        }
    }

    /// Accept highlighted autocomplete command into input prompt.
    pub fn autocomplete_accept(&mut self) {
        if self.autocomplete_state.is_open && !self.autocomplete_state.matches.is_empty() {
            let selected = self.autocomplete_state.matches[self.autocomplete_state.selected_idx];
            self.chat_input = format!("/{} ", selected.name);
            self.chat_cursor = self.chat_input.len();
            self.autocomplete_state.is_open = false;
        }
    }

    /// Move input cursor one position to the left.
    pub fn chat_input_left(&mut self) {
        self.chat_cursor = self.chat_cursor.saturating_sub(1);
    }

    /// Move input cursor one position to the right.
    pub fn chat_input_right(&mut self) {
        if self.chat_cursor < self.chat_input.len() {
            self.chat_cursor += 1;
        }
    }

    /// Move input cursor to beginning of input line.
    pub fn chat_input_home(&mut self) {
        self.chat_cursor = 0;
    }

    /// Move input cursor to end of input line.
    pub fn chat_input_end(&mut self) {
        self.chat_cursor = self.chat_input.len();
    }

    /// Move cursor up one line in multi-line chat input buffer.
    pub fn chat_input_up(&mut self) {
        if !self.chat_input.contains('\n') {
            return;
        }
        let before = &self.chat_input[..self.chat_cursor];
        if let Some(prev_nl) = before.rfind('\n') {
            let line_before_prev = &before[..prev_nl];
            let prev_line_start = line_before_prev.rfind('\n').map(|p| p + 1).unwrap_or(0);
            let col = before.len() - prev_nl - 1;
            let prev_line_len = prev_nl - prev_line_start;
            self.chat_cursor = prev_line_start + col.min(prev_line_len);
        }
    }

    /// Move cursor down one line in multi-line chat input buffer.
    pub fn chat_input_down(&mut self) {
        if !self.chat_input.contains('\n') {
            return;
        }
        let after = &self.chat_input[self.chat_cursor..];
        if let Some(next_nl) = after.find('\n') {
            let next_line_start = self.chat_cursor + next_nl + 1;
            let line_after = &self.chat_input[next_line_start..];
            let next_line_end = line_after
                .find('\n')
                .map(|p| next_line_start + p)
                .unwrap_or(self.chat_input.len());
            let current_line_start = self.chat_input[..self.chat_cursor]
                .rfind('\n')
                .map(|p| p + 1)
                .unwrap_or(0);
            let col = self.chat_cursor.saturating_sub(current_line_start);
            let next_line_len = next_line_end.saturating_sub(next_line_start);
            self.chat_cursor = next_line_start + col.min(next_line_len);
        }
    }

    /// Close autocomplete popup.
    pub fn autocomplete_close(&mut self) {
        self.autocomplete_state.is_open = false;
    }

    /// Clear chat prompt input line and reset cursor.
    pub fn chat_input_clear(&mut self) {
        self.chat_input.clear();
        self.chat_cursor = 0;
        self.chat_history_idx = None;
        self.update_autocomplete();
    }

    /// Recall previous command from history into input.
    pub fn chat_history_prev(&mut self) {
        if self.chat_history.is_empty() {
            return;
        }
        let next_idx = match self.chat_history_idx {
            None => self.chat_history.len() - 1,
            Some(idx) => idx.saturating_sub(1),
        };
        self.chat_history_idx = Some(next_idx);
        self.chat_input = self.chat_history[next_idx].clone();
        self.chat_cursor = self.chat_input.len();
    }

    /// Recall next command from history into input.
    pub fn chat_history_next(&mut self) {
        if self.chat_history.is_empty() {
            return;
        }
        if let Some(idx) = self.chat_history_idx {
            if idx + 1 < self.chat_history.len() {
                let next_idx = idx + 1;
                self.chat_history_idx = Some(next_idx);
                self.chat_input = self.chat_history[next_idx].clone();
                self.chat_cursor = self.chat_input.len();
            } else {
                self.chat_history_idx = None;
                self.chat_input.clear();
                self.chat_cursor = 0;
            }
        }
    }

    /// Set thinking trace expansion state globally and update all existing thinking items.
    pub fn set_thinking_expanded(&mut self, expanded: bool) {
        self.thinking_expanded = expanded;
        for msg in &mut self.chat_messages {
            if msg.role == ChatRole::Thinking {
                msg.is_expanded = expanded;
            }
        }
    }

    /// Toggle thinking trace expansion.
    pub fn toggle_thinking_expanded(&mut self) {
        let new_state = !self.thinking_expanded;
        self.set_thinking_expanded(new_state);
    }

    /// Set tool execution output expansion state globally and update all existing tool items.
    pub fn set_tool_calls_expanded(&mut self, expanded: bool) {
        self.tool_calls_expanded = expanded;
        for msg in &mut self.chat_messages {
            if msg.role == ChatRole::Tool {
                msg.is_expanded = expanded;
            }
        }
    }

    /// Toggle tool execution output expansion.
    pub fn toggle_tool_calls_expanded(&mut self) {
        let new_state = !self.tool_calls_expanded;
        self.set_tool_calls_expanded(new_state);
    }

    /// Scroll chat messages view upwards.
    pub fn chat_scroll_up(&mut self) {
        self.chat_auto_scroll = false;
        self.chat_scroll = self.chat_scroll.saturating_add(3);
    }

    /// Scroll chat messages view downwards.
    pub fn chat_scroll_down(&mut self) {
        self.chat_scroll = self.chat_scroll.saturating_sub(3);
        if self.chat_scroll == 0 {
            self.chat_auto_scroll = true;
        }
    }

    /// Scroll chat messages view upwards by a full page.
    pub fn chat_scroll_page_up(&mut self) {
        self.chat_auto_scroll = false;
        self.chat_scroll = self.chat_scroll.saturating_add(15);
    }

    /// Scroll chat messages view downwards by a full page.
    pub fn chat_scroll_page_down(&mut self) {
        self.chat_scroll = self.chat_scroll.saturating_sub(15);
        if self.chat_scroll == 0 {
            self.chat_auto_scroll = true;
        }
    }

    /// Submit current chat input prompt or slash command.
    pub fn dispatch_chat(&mut self) {
        let prompt = self.chat_input.trim().to_string();
        if prompt.is_empty() {
            return;
        }

        // Record prompt into input history
        if self.chat_history.last().map(|s| s.as_str()) != Some(&prompt) {
            self.chat_history.push(prompt.clone());
        }
        self.chat_history_idx = None;
        self.chat_input.clear();
        self.chat_cursor = 0;
        self.chat_auto_scroll = true;
        self.chat_scroll = 0;

        // Route slash commands
        if prompt.starts_with('/') {
            let now = chrono::Utc::now().format("%H:%M:%S").to_string();
            self.chat_messages.push(ChatMessageItem {
                role: ChatRole::User,
                content: prompt.clone(),
                timestamp: now.clone(),
                is_expanded: false,
            });

            if let Some(command) = crate::commands::parse_command(&prompt) {
                let response = crate::commands::execute_command(self, command);
                self.chat_messages.push(ChatMessageItem {
                    role: ChatRole::System,
                    content: response,
                    timestamp: now,
                    is_expanded: false,
                });
            }
            return;
        }

        if self.chat_is_running {
            self.status_message = Some(
                "An agent is already running. Press Esc to cancel it before launching a new task."
                    .to_string(),
            );
            return;
        }

        let mut history: Vec<cortex_runtime::agent::ChatMessage> = Vec::new();
        let start_idx = self.chat_messages.len().saturating_sub(20);
        for item in &self.chat_messages[start_idx..] {
            match item.role {
                ChatRole::User => {
                    history.push(cortex_runtime::agent::ChatMessage::User(
                        item.content.clone(),
                    ));
                }
                ChatRole::Assistant => {
                    if !item.content.trim().is_empty() {
                        history.push(cortex_runtime::agent::ChatMessage::Assistant(
                            item.content.clone(),
                        ));
                    }
                }
                ChatRole::Tool => {
                    history.push(cortex_runtime::agent::ChatMessage::Assistant(format!(
                        "Observation: {}",
                        item.content
                    )));
                }
                ChatRole::Error => {
                    history.push(cortex_runtime::agent::ChatMessage::Assistant(format!(
                        "Error: {}",
                        item.content
                    )));
                }
                ChatRole::System | ChatRole::Thinking => {}
            }
        }

        let now = chrono::Utc::now().format("%H:%M:%S").to_string();
        self.chat_messages.push(ChatMessageItem {
            role: ChatRole::User,
            content: prompt.clone(),
            timestamp: now,
            is_expanded: false,
        });

        let active_portal = self.active_portal().clone();
        let cancel_token = CancellationToken::new();
        let (sender, receiver) = channel();

        self.chat_is_running = true;
        self.chat_last_event_seq = 0;
        self.run_start_instant = Some(std::time::Instant::now());
        self.current_verb = crate::theme::whimsical_verb(self.anim_tick).to_string();
        self.chat_handle = Some(AgentExecutionHandle {
            cancel_token: cancel_token.clone(),
            receiver,
            run_id: None,
        });

        let params = ChatAgentExecutionParams {
            prompt,
            history,
            model: active_portal.model_name.clone(),
            api_key: active_portal.api_key.clone(),
            base_url: active_portal.base_url.clone(),
            store: self.store.clone(),
            cancel_token,
            sender,
        };

        std::thread::spawn(move || {
            execute_chat_agent(params);
        });
    }

    /// Check whether the active chat agent is currently in the process of cancelling.
    pub fn is_chat_cancelling(&self) -> bool {
        self.chat_handle
            .as_ref()
            .is_some_and(|h| h.cancel_token.is_cancelled())
    }

    /// Cancel the currently executing agent run, if any.
    pub fn cancel_chat_agent(&mut self) {
        if let Some(handle) = &self.chat_handle {
            if !handle.cancel_token.is_cancelled() {
                handle.cancel_token.cancel();
                let now = chrono::Utc::now().format("%H:%M:%S").to_string();
                self.chat_messages.push(ChatMessageItem {
                    role: ChatRole::System,
                    content: "Cancellation requested... Waiting for agent to abort gracefully (press Ctrl+C again to force quit)."
                        .to_string(),
                    timestamp: now,
                    is_expanded: false,
                });
                self.status_message =
                    Some("Sent cancellation signal to running agent.".to_string());
            }
        }
    }

    /// Gracefully shutdown background workers and cancel active executions.
    pub fn shutdown(&mut self) {
        self.indexer.stop();
        if let Some(handle) = &self.chat_handle {
            handle.cancel_token.cancel();
        }
    }

    /// Poll for real-time updates from background agent executions and event store.
    /// Returns `true` if any real-time agent updates were received.
    pub fn poll_chat_updates(&mut self) -> bool {
        self.anim_tick = self.anim_tick.wrapping_add(1);
        let mut finished = false;
        let mut new_run_id: Option<String> = None;
        let mut received_any = false;

        if let Some(handle) = &mut self.chat_handle {
            while let Ok(update) = handle.receiver.try_recv() {
                received_any = true;
                let now = chrono::Utc::now().format("%H:%M:%S").to_string();
                match update {
                    ChatAgentUpdate::Started { run_id, model } => {
                        handle.run_id = Some(run_id.clone());
                        new_run_id = Some(run_id.clone());
                        self.visualizer.on_run_started(&run_id, &model);
                        self.chat_messages.push(ChatMessageItem {
                            role: ChatRole::System,
                            content: format!(
                                "Agent execution started (Run ID: {}, Model: {})",
                                run_id, model
                            ),
                            timestamp: now,
                            is_expanded: false,
                        });
                    }
                    ChatAgentUpdate::Token(token) => {
                        self.visualizer.on_token(&token);
                        if let Some(last) = self.chat_messages.last_mut() {
                            if last.role == ChatRole::Assistant {
                                last.content.push_str(&token);
                            } else {
                                self.chat_messages
                                    .push(ChatMessageItem::new(ChatRole::Assistant, token));
                            }
                        } else {
                            self.chat_messages
                                .push(ChatMessageItem::new(ChatRole::Assistant, token));
                        }
                    }
                    ChatAgentUpdate::ToolStarted { name, args } => {
                        let formatted = format_tool_start(&name, &args);
                        self.visualizer.on_tool_started(&name, &args);
                        self.chat_messages
                            .push(ChatMessageItem::new(ChatRole::Tool, formatted));
                    }
                    ChatAgentUpdate::ToolCompleted {
                        name,
                        output,
                        is_error,
                    } => {
                        let formatted = format_tool_end(&name, &output, is_error);
                        self.visualizer.on_tool_completed(&name, &output, is_error);
                        self.chat_messages.push(ChatMessageItem::new(
                            if is_error {
                                ChatRole::Error
                            } else {
                                ChatRole::Tool
                            },
                            formatted,
                        ));
                    }
                    ChatAgentUpdate::Completed(res) => {
                        self.session_tokens += res.tokens_total as u64;
                        self.session_cost += res.estimated_cost_usd;
                        self.run_start_instant = None;
                        self.visualizer.on_completed();

                        // Ensure final answer is displayed if streaming was quiet
                        let has_assistant = self.chat_messages.last().is_some_and(|m| {
                            m.role == ChatRole::Assistant && !m.content.trim().is_empty()
                        });

                        if !has_assistant && !res.final_answer.trim().is_empty() {
                            let answer = &res.final_answer;
                            if let (Some(start), Some(end)) =
                                (answer.find("<think>"), answer.find("</think>"))
                            {
                                let thought = answer[start + 7..end].trim().to_string();
                                let remaining = answer[end + 8..].trim().to_string();
                                if !thought.is_empty() {
                                    self.chat_messages.push(ChatMessageItem {
                                        role: ChatRole::Thinking,
                                        content: thought,
                                        timestamp: now.clone(),
                                        is_expanded: self.thinking_expanded,
                                    });
                                }
                                self.chat_messages.push(ChatMessageItem {
                                    role: ChatRole::Assistant,
                                    content: remaining,
                                    timestamp: now.clone(),
                                    is_expanded: false,
                                });
                            } else {
                                self.chat_messages.push(ChatMessageItem {
                                    role: ChatRole::Assistant,
                                    content: res.final_answer.clone(),
                                    timestamp: now.clone(),
                                    is_expanded: false,
                                });
                            }
                        }

                        self.chat_messages.push(ChatMessageItem {
                            role: ChatRole::System,
                            content: format!(
                                "✓ Run completed in {}ms across {} iterations | Tokens: {} | Cost: ${:.4}",
                                res.duration_ms,
                                res.iterations,
                                res.tokens_total,
                                res.estimated_cost_usd
                            ),
                            timestamp: now,
                            is_expanded: false,
                        });
                        finished = true;
                    }
                    ChatAgentUpdate::Cancelled(reason) => {
                        self.run_start_instant = None;
                        self.visualizer.on_cancelled();
                        self.chat_messages.push(ChatMessageItem {
                            role: ChatRole::System,
                            content: format!("Agent run cancelled: {}", reason),
                            timestamp: now,
                            is_expanded: false,
                        });
                        finished = true;
                    }
                    ChatAgentUpdate::Error(err) => {
                        self.run_start_instant = None;
                        self.visualizer.on_cancelled();
                        self.chat_messages.push(ChatMessageItem {
                            role: ChatRole::Error,
                            content: format!("Agent execution error: {}", err),
                            timestamp: now,
                            is_expanded: false,
                        });
                        finished = true;
                    }
                    ChatAgentUpdate::InterAgentMessage {
                        message_id,
                        sender,
                        recipient,
                        routing_key,
                        timestamp,
                        payload_preview,
                    } => {
                        let ts = if timestamp.len() >= 8 {
                            timestamp.clone()
                        } else {
                            now.clone()
                        };
                        let msg = InterAgentMessageItem {
                            id: message_id,
                            timestamp: ts.clone(),
                            sender: sender.clone(),
                            recipient: recipient.clone(),
                            routing_key,
                            content: payload_preview.clone(),
                        };
                        self.coordination.record_message(msg);
                        self.coordination.show_viewer = true;
                        self.chat_messages.push(ChatMessageItem {
                            role: ChatRole::System,
                            content: format!(
                                "[{}] {} → {}: \"{}\"",
                                ts, sender, recipient, payload_preview
                            ),
                            timestamp: now.clone(),
                            is_expanded: false,
                        });
                    }
                }
            }
        }

        // Ingest any execution events from the active run event store
        let start_seq = self.chat_last_event_seq as usize;
        let new_events: Vec<ExecutionEvent> = if start_seq < self.events.len() {
            self.events[start_seq..]
                .iter()
                .map(|r| r.event.clone())
                .collect()
        } else {
            Vec::new()
        };
        for event in new_events {
            self.ingest_execution_event(&event);
        }
        self.chat_last_event_seq = self.events.len() as u64;

        if finished {
            self.chat_handle = None;
            self.chat_is_running = false;
            self.refresh();
        } else if new_run_id.is_some() {
            self.refresh();
        }

        received_any || finished
    }

    /// Record an inter-agent routed message into live coordination state.
    pub fn record_inter_agent_message(
        &mut self,
        message_id: &str,
        sender: &str,
        recipient: &str,
        routing_key: &str,
        timestamp: &str,
        payload_preview: &str,
    ) {
        let msg = InterAgentMessageItem {
            id: message_id.to_string(),
            timestamp: if timestamp.len() >= 8 && timestamp.contains(':') {
                timestamp.to_string()
            } else {
                chrono::Utc::now().format("%H:%M:%S").to_string()
            },
            sender: sender.to_string(),
            recipient: recipient.to_string(),
            routing_key: routing_key.to_string(),
            content: payload_preview.to_string(),
        };
        self.coordination.record_message(msg);
        self.coordination.show_viewer = true;
    }

    /// Toggle visibility of the multi-agent coordination viewer.
    pub fn toggle_coordination_panel(&mut self) {
        self.coordination.show_viewer = !self.coordination.show_viewer;
    }

    /// Cycle agent filter in coordination viewer.
    pub fn cycle_coordination_filter(&mut self) {
        self.coordination.cycle_agent_filter();
    }

    /// Cycle stage filter in coordination viewer.
    pub fn cycle_stage_filter(&mut self) {
        self.coordination.cycle_stage_filter();
    }

    /// Toggle visibility of the Live Agent Visualizer HUD.
    pub fn toggle_visualizer(&mut self) {
        self.visualizer.toggle();
    }

    /// Close the Live Agent Visualizer HUD.
    pub fn close_visualizer(&mut self) {
        self.visualizer.close();
    }

    /// Cycle through sub-tabs in the Live Agent Visualizer HUD.
    pub fn cycle_visualizer_tab(&mut self) {
        self.visualizer.cycle_tab();
    }

    /// Ingest an ExecutionEvent to update coordination state if applicable.
    pub fn ingest_execution_event(&mut self, event: &ExecutionEvent) {
        if let ExecutionEvent::InterAgentMessage {
            message_id,
            sender,
            recipient,
            routing_key,
            timestamp,
            payload,
            ..
        } = event
        {
            let preview = match payload {
                serde_json::Value::Object(map) => {
                    if let Some(instructions) = map.get("instructions").and_then(|v| v.as_str()) {
                        instructions.to_string()
                    } else if let Some(output) = map.get("output").and_then(|v| v.as_str()) {
                        output.to_string()
                    } else if let Some(content) = map.get("content").and_then(|v| v.as_str()) {
                        content.to_string()
                    } else {
                        payload.to_string()
                    }
                }
                serde_json::Value::String(s) => s.clone(),
                _ => payload.to_string(),
            };
            let ts = if timestamp.len() >= 19 {
                timestamp[11..19].to_string()
            } else {
                timestamp.clone()
            };
            self.record_inter_agent_message(
                message_id,
                sender.as_str(),
                recipient.as_str(),
                routing_key,
                &ts,
                &preview,
            );
        }
    }

    // ==========================================
    // Persistent Agent State Machine & Management
    // ==========================================

    /// Register a new persistent agent in the workspace.
    pub fn register_agent(
        &mut self,
        id: String,
        name: String,
        role: String,
        model: String,
        policy: String,
    ) {
        let ws = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());
        let agent = AgentView {
            id,
            name,
            role,
            policy,
            status: "Ready".to_string(),
            model,
            workspace: ws,
            tools: vec![
                "read_file".into(),
                "write_file".into(),
                "list_dir".into(),
                "shell".into(),
                "git_*".into(),
            ],
            runs_count: 0,
        };
        self.agents.push(agent);
    }

    /// Transition a persistent agent to a new lifecycle state.
    pub fn transition_agent_state(
        &mut self,
        id: &str,
        new_state: AgentLifecycleState,
    ) -> Result<(), String> {
        if let Some(agent) = self
            .agents
            .iter_mut()
            .find(|a| a.id.eq_ignore_ascii_case(id))
        {
            agent.status = new_state.as_str().to_string();
            self.status_message = Some(format!(
                "Agent '{}' transitioned to {}.",
                id,
                new_state.as_str()
            ));
            Ok(())
        } else {
            Err(format!("Agent '{}' not found", id))
        }
    }

    /// Transition currently selected agent to RUNNING.
    pub fn start_selected_agent(&mut self) {
        if let Some(agent) = self.agents.get(self.selected_agent_idx) {
            let id = agent.id.clone();
            let _ = self.transition_agent_state(&id, AgentLifecycleState::Running);
        }
    }

    /// Transition currently selected agent to STOPPED.
    pub fn stop_selected_agent(&mut self) {
        if let Some(agent) = self.agents.get(self.selected_agent_idx) {
            let id = agent.id.clone();
            let _ = self.transition_agent_state(&id, AgentLifecycleState::Stopped);
        }
    }

    /// Transition currently selected agent to PAUSED.
    pub fn pause_selected_agent(&mut self) {
        if let Some(agent) = self.agents.get(self.selected_agent_idx) {
            let id = agent.id.clone();
            let _ = self.transition_agent_state(&id, AgentLifecycleState::Paused);
        }
    }

    // ==========================================
    // Portals & Provider Configuration Methods
    // ==========================================

    /// Navigate to next portal in table.
    pub fn next_portal(&mut self) {
        if !self.portals.is_empty() && self.selected_portal_idx + 1 < self.portals.len() {
            self.selected_portal_idx += 1;
        }
    }

    /// Navigate to previous portal in table.
    pub fn prev_portal(&mut self) {
        if self.selected_portal_idx > 0 {
            self.selected_portal_idx -= 1;
        }
    }

    /// Mark the currently selected portal as active.
    pub fn activate_selected_portal(&mut self) {
        if self.portals.is_empty() {
            return;
        }
        for (i, p) in self.portals.iter_mut().enumerate() {
            p.is_active = i == self.selected_portal_idx;
        }
        let name = self.portals[self.selected_portal_idx].name.clone();
        self.status_message = Some(format!("Activated portal: {}", name));
    }

    /// Delete the selected portal if more than one exists.
    pub fn delete_selected_portal(&mut self) {
        if self.portals.len() <= 1 {
            self.status_message = Some("Cannot delete the only remaining portal.".to_string());
            return;
        }
        let was_active = self.portals[self.selected_portal_idx].is_active;
        let name = self.portals.remove(self.selected_portal_idx).name;
        if self.selected_portal_idx >= self.portals.len() {
            self.selected_portal_idx = self.portals.len() - 1;
        }
        if was_active {
            self.portals[0].is_active = true;
        }
        self.status_message = Some(format!("Deleted portal '{}'.", name));
    }

    /// Begin interactive flow for adding a new portal.
    pub fn start_adding_portal(&mut self) {
        self.portal_input_mode = PortalInputMode::Adding {
            field: PortalField::Name,
        };
        self.portal_input_buffer.clear();
        self.new_portal_draft = NewPortalDraft::default();
        self.status_message = Some("Creating new portal: Enter Portal Name.".to_string());
    }

    /// Begin editing API key for currently selected portal.
    pub fn start_editing_key(&mut self) {
        if let Some(portal) = self.portals.get(self.selected_portal_idx) {
            self.portal_input_mode = PortalInputMode::EditingKey;
            self.portal_input_buffer = portal.api_key.clone().unwrap_or_default();
            self.status_message = Some(format!(
                "Editing API key for '{}'. Press Enter to save, Esc to cancel.",
                portal.name
            ));
        }
    }

    /// Begin editing Base URL for currently selected portal.
    pub fn start_editing_base_url(&mut self) {
        if let Some(portal) = self.portals.get(self.selected_portal_idx) {
            self.portal_input_mode = PortalInputMode::EditingBaseUrl;
            self.portal_input_buffer = portal.base_url.clone().unwrap_or_default();
            self.status_message = Some(format!(
                "Editing Base URL for '{}'. Press Enter to save, Esc to cancel.",
                portal.name
            ));
        }
    }

    /// Confirm the current interactive input field in Portals tab.
    pub fn confirm_portal_input(&mut self) {
        match self.portal_input_mode {
            PortalInputMode::Normal => {}
            PortalInputMode::EditingKey => {
                let trimmed = self.portal_input_buffer.trim().to_string();
                let key_opt = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                };
                if let Some(portal) = self.portals.get_mut(self.selected_portal_idx) {
                    portal.api_key = key_opt;
                    self.status_message = Some(format!("Updated API key for '{}'.", portal.name));
                }
                self.portal_input_mode = PortalInputMode::Normal;
                self.portal_input_buffer.clear();
            }
            PortalInputMode::EditingBaseUrl => {
                let trimmed = self.portal_input_buffer.trim().to_string();
                let url_opt = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                };
                if let Some(portal) = self.portals.get_mut(self.selected_portal_idx) {
                    portal.base_url = url_opt;
                    self.status_message = Some(format!("Updated Base URL for '{}'.", portal.name));
                }
                self.portal_input_mode = PortalInputMode::Normal;
                self.portal_input_buffer.clear();
            }
            PortalInputMode::Adding { field } => match field {
                PortalField::Name => {
                    let val = self.portal_input_buffer.trim().to_string();
                    self.new_portal_draft.name = if val.is_empty() {
                        "Custom Portal".to_string()
                    } else {
                        val
                    };
                    self.portal_input_buffer.clear();
                    self.portal_input_mode = PortalInputMode::Adding {
                        field: PortalField::Model,
                    };
                    self.status_message = Some(
                        "Enter Model identifier (e.g. gpt-4o, claude-3-5-sonnet, ollama/llama3.1):"
                            .to_string(),
                    );
                }
                PortalField::Model => {
                    let val = self.portal_input_buffer.trim().to_string();
                    self.new_portal_draft.model_name = if val.is_empty() {
                        "gpt-4o-mini".to_string()
                    } else {
                        val
                    };
                    self.portal_input_buffer.clear();
                    self.portal_input_mode = PortalInputMode::Adding {
                        field: PortalField::BaseUrl,
                    };
                    self.status_message =
                        Some("Enter Base URL (or leave blank for provider default):".to_string());
                }
                PortalField::BaseUrl => {
                    let val = self.portal_input_buffer.trim().to_string();
                    self.new_portal_draft.base_url = val;
                    self.portal_input_buffer.clear();
                    self.portal_input_mode = PortalInputMode::Adding {
                        field: PortalField::ApiKey,
                    };
                    self.status_message = Some(
                        "Enter API Key (or leave blank if using environment variable):".to_string(),
                    );
                }
                PortalField::ApiKey => {
                    let key_val = self.portal_input_buffer.trim().to_string();
                    self.new_portal_draft.api_key = key_val;
                    let id = format!("portal-{}", chrono::Utc::now().timestamp_millis());
                    let lower = self.new_portal_draft.model_name.to_lowercase();
                    let provider_kind = if lower.starts_with("ollama/") {
                        "ollama".to_string()
                    } else if lower.starts_with("claude") {
                        "anthropic".to_string()
                    } else {
                        "openai".to_string()
                    };
                    let base_url = if self.new_portal_draft.base_url.is_empty() {
                        None
                    } else {
                        Some(self.new_portal_draft.base_url.clone())
                    };
                    let api_key = if self.new_portal_draft.api_key.is_empty() {
                        None
                    } else {
                        Some(self.new_portal_draft.api_key.clone())
                    };

                    let new_portal = ModelPortal::new(
                        id,
                        self.new_portal_draft.name.clone(),
                        provider_kind,
                        self.new_portal_draft.model_name.clone(),
                        base_url,
                        api_key,
                        true,
                    );

                    for p in &mut self.portals {
                        p.is_active = false;
                    }
                    self.portals.push(new_portal);
                    self.selected_portal_idx = self.portals.len() - 1;
                    self.portal_input_mode = PortalInputMode::Normal;
                    self.portal_input_buffer.clear();
                    self.new_portal_draft = NewPortalDraft::default();
                    self.status_message = Some("Created and activated new portal.".to_string());
                }
            },
        }
    }

    /// Cancel any active portal input mode and discard buffer.
    pub fn cancel_portal_input(&mut self) {
        self.portal_input_mode = PortalInputMode::Normal;
        self.portal_input_buffer.clear();
        self.new_portal_draft = NewPortalDraft::default();
        self.status_message = Some("Cancelled portal edit.".to_string());
    }

    /// Test configuration of currently selected portal.
    pub fn test_selected_portal(&mut self) {
        if let Some(portal) = self.portals.get(self.selected_portal_idx) {
            if portal.is_configured() {
                self.status_message = Some(format!(
                    "Portal '{}' ({}) is configured and ready.",
                    portal.name,
                    portal.status_text()
                ));
            } else {
                self.status_message = Some(format!(
                    "Portal '{}' requires credentials! Press 'e' to set API key or export env var.",
                    portal.name
                ));
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Execution parameters for background chat agent worker.
struct ChatAgentExecutionParams {
    prompt: String,
    history: Vec<cortex_runtime::agent::ChatMessage>,
    model: String,
    api_key: Option<String>,
    base_url: Option<String>,
    store: Option<Arc<RunStore>>,
    cancel_token: CancellationToken,
    sender: Sender<ChatAgentUpdate>,
}

/// Asynchronous worker executing the agent iteration loop in background.
fn execute_chat_agent(params: ChatAgentExecutionParams) {
    let ChatAgentExecutionParams {
        prompt,
        history,
        model,
        api_key,
        base_url,
        store,
        cancel_token,
        sender,
    } = params;
    let ws_path = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let ws = match Workspace::new(&ws_path) {
        Ok(w) => Arc::new(w),
        Err(e) => {
            let _ = sender.send(ChatAgentUpdate::Error(format!(
                "Failed to initialize workspace at {}: {}",
                ws_path.display(),
                e
            )));
            return;
        }
    };

    let registry = ToolRegistry::new();
    let reg_res = tools::register_standard_tools(&registry, ws.clone());

    if let Err(e) = reg_res {
        let _ = sender.send(ChatAgentUpdate::Error(format!(
            "Failed to configure tools: {}",
            e
        )));
        return;
    }

    let provider = match create_model_provider(&model, api_key, base_url) {
        Ok(p) => p,
        Err(e) => {
            let _ = sender.send(ChatAgentUpdate::Error(format!(
                "Failed to instantiate model provider: {}",
                e
            )));
            return;
        }
    };

    match provider.is_configured() {
        Ok(true) => {}
        Ok(false) => {
            let _ = sender.send(ChatAgentUpdate::Error(format!(
                "Model provider '{}' for model '{}' is not configured.\n\
                 Please configure your API key in Portals (Tab 8) or set OPENAI_API_KEY / ANTHROPIC_API_KEY.",
                provider.descriptor().provider,
                model
            )));
            return;
        }
        Err(e) => {
            let _ = sender.send(ChatAgentUpdate::Error(format!(
                "Provider configuration check failed: {}",
                e
            )));
            return;
        }
    }

    let mut context = AgentContext::new(&prompt).with_workspace(ws);
    // Ground context with multi-turn history to eliminate hallucinations across conversational turns
    if !history.is_empty() {
        let system_msg = context.messages.remove(0);
        let user_msg = context.messages.pop();

        let mut combined = vec![system_msg];
        combined.extend(history);
        if let Some(u) = user_msg {
            combined.push(u);
        }
        context.messages = combined;
    }
    let run_id = context.run_id.to_string();

    let _ = sender.send(ChatAgentUpdate::Started {
        run_id: run_id.clone(),
        model: model.clone(),
    });

    let sender_token = sender.clone();
    let token_cb = Arc::new(move |token: &str| {
        let _ = sender_token.send(ChatAgentUpdate::Token(token.to_string()));
    });

    let sender_start = sender.clone();
    let tool_start_cb = Arc::new(move |name: &str, args: &str| {
        let _ = sender_start.send(ChatAgentUpdate::ToolStarted {
            name: name.to_string(),
            args: args.to_string(),
        });
    });

    let sender_end = sender.clone();
    let tool_end_cb = Arc::new(move |name: &str, output: &str, is_error: bool| {
        let _ = sender_end.send(ChatAgentUpdate::ToolCompleted {
            name: name.to_string(),
            output: output.to_string(),
            is_error,
        });
    });

    let agent_loop = AgentLoop::new(15)
        .with_cancellation_token(cancel_token)
        .with_store_optional(store)
        .with_token_callback(token_cb)
        .with_tool_callbacks(tool_start_cb, tool_end_cb);

    match agent_loop.run(&mut context, &*provider, &registry) {
        Ok(res) => {
            let _ = sender.send(ChatAgentUpdate::Completed(Box::new(res)));
        }
        Err(cortex_core::CortexError::Cancelled(reason)) => {
            let _ = sender.send(ChatAgentUpdate::Cancelled(reason));
        }
        Err(e) => {
            let _ = sender.send(ChatAgentUpdate::Error(e.to_string()));
        }
    }
}

fn format_tool_start(name: &str, args: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    match name {
        "grep" => {
            let pat = parsed.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let path = parsed.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!("grep \"{}\" {}", pat, path)
        }
        "read_file" => {
            let path = parsed.get("path").and_then(|v| v.as_str()).unwrap_or("");
            format!("Reading {}", path)
        }
        "write_file" | "edit_file" => {
            let path = parsed.get("path").and_then(|v| v.as_str()).unwrap_or("");
            format!("✎ {}", path)
        }
        "bash" | "execute_command" | "shell" => {
            let cmd = parsed.get("command").and_then(|v| v.as_str()).unwrap_or("");
            format!("$ {}", cmd)
        }
        "glob" => {
            let pat = parsed.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            format!("glob {}", pat)
        }
        "git_diff" => "git diff".to_string(),
        "git_status" => "git status".to_string(),
        _ => format!("{}({})", name, args),
    }
}

fn format_tool_end(name: &str, output: &str, is_error: bool) -> String {
    if is_error {
        let first_line = output.lines().next().unwrap_or("error");
        return format!("✗ {}", first_line);
    }
    match name {
        "grep" => {
            let count = output.lines().filter(|l| !l.trim().is_empty()).count();
            format!("✓ {} matches", count)
        }
        "read_file" => {
            let lines = output.lines().count();
            format!("✓ {} lines", lines)
        }
        "write_file" | "edit_file" => "✓ updated".to_string(),
        "bash" | "execute_command" | "shell" => {
            let trimmed = output.trim();
            if trimmed.is_empty() {
                "✓ done".to_string()
            } else {
                let first = trimmed.lines().next().unwrap_or("done");
                if first.len() > 60 {
                    format!("✓ {}...", &first[..57])
                } else {
                    format!("✓ {}", first)
                }
            }
        }
        "glob" => {
            let count = output.lines().filter(|l| !l.trim().is_empty()).count();
            format!("✓ {} files found", count)
        }
        "git_diff" => {
            let count = output.lines().count();
            format!("✓ diff ({} lines)", count)
        }
        _ => "✓ done".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_initialization_and_tab_cycling() {
        let mut app = App::new(None);
        assert_eq!(app.active_tab, ActiveTab::Chat);

        app.set_tab(ActiveTab::Dashboard);
        assert_eq!(app.active_tab, ActiveTab::Dashboard);

        app.next_tab();
        assert_eq!(app.active_tab, ActiveTab::Agents);

        app.next_tab();
        assert_eq!(app.active_tab, ActiveTab::ActiveRun);

        app.prev_tab();
        assert_eq!(app.active_tab, ActiveTab::Agents);

        app.set_tab(ActiveTab::Tasks);
        assert_eq!(app.active_tab, ActiveTab::Tasks);
        assert!(!app.tasks.is_empty());

        app.set_tab(ActiveTab::Chat);
        assert_eq!(app.active_tab, ActiveTab::Chat);

        app.set_tab(ActiveTab::Portals);
        assert_eq!(app.active_tab, ActiveTab::Portals);

        app.set_tab(ActiveTab::Scheduler);
        assert_eq!(app.active_tab, ActiveTab::Scheduler);
        assert_eq!(app.active_tab.label(), "9: Scheduler");
    }

    #[test]
    fn test_scheduler_state_and_countdown_formatting() {
        let mut app = App::new(None);
        assert_eq!(app.cron_jobs.len(), 0);
        assert_eq!(app.cron_job_runs.len(), 0);
        assert!(!app.cron_drawer_open);

        app.set_tab(ActiveTab::Scheduler);
        app.select_current();
        assert!(app.cron_drawer_open);
        app.close_cron_drawer();
        assert!(!app.cron_drawer_open);

        let now = chrono::Utc::now();
        let next_run = now + chrono::Duration::minutes(15);
        let countdown =
            App::format_fire_countdown(Some(next_run), cortex_runtime::JobStatus::Active, now);
        assert!(countdown.contains("15m") || countdown.contains("14m"));

        let paused_countdown =
            App::format_fire_countdown(Some(next_run), cortex_runtime::JobStatus::Paused, now);
        assert_eq!(paused_countdown, "Paused");

        let completed_countdown =
            App::format_fire_countdown(Some(next_run), cortex_runtime::JobStatus::Completed, now);
        assert_eq!(completed_countdown, "Completed");
    }

    #[test]
    fn test_app_navigation_bounds() {
        let mut app = App::new(None);
        assert_eq!(app.selected_run_idx, 0);

        // Underflow safe
        app.prev_item();
        assert_eq!(app.selected_run_idx, 0);

        // Overflow safe on empty list
        app.next_item();
        assert_eq!(app.selected_run_idx, 0);
    }

    #[test]
    fn test_portal_activation_and_deletion() {
        let mut app = App::new(None);
        assert!(!app.portals.is_empty());
        assert!(app.portals[0].is_active);

        // Switch active portal
        app.selected_portal_idx = 1;
        app.activate_selected_portal();
        assert!(!app.portals[0].is_active);
        assert!(app.portals[1].is_active);

        // Delete portal
        let count_before = app.portals.len();
        app.delete_selected_portal();
        assert_eq!(app.portals.len(), count_before - 1);
    }

    #[test]
    fn test_portal_editing_flow() {
        let mut app = App::new(None);
        app.selected_portal_idx = 0;

        app.start_editing_key();
        assert_eq!(app.portal_input_mode, PortalInputMode::EditingKey);
        app.portal_input_buffer = "sk-test-custom-key".to_string();
        app.confirm_portal_input();

        assert_eq!(app.portal_input_mode, PortalInputMode::Normal);
        assert_eq!(
            app.portals[0].api_key,
            Some("sk-test-custom-key".to_string())
        );
    }

    #[test]
    fn test_portal_adding_flow() {
        let mut app = App::new(None);
        let count_before = app.portals.len();

        app.start_adding_portal();
        assert_eq!(
            app.portal_input_mode,
            PortalInputMode::Adding {
                field: PortalField::Name
            }
        );

        app.portal_input_buffer = "My Custom Ollama".to_string();
        app.confirm_portal_input();
        assert_eq!(
            app.portal_input_mode,
            PortalInputMode::Adding {
                field: PortalField::Model
            }
        );

        app.portal_input_buffer = "ollama/deepseek-r1".to_string();
        app.confirm_portal_input();
        assert_eq!(
            app.portal_input_mode,
            PortalInputMode::Adding {
                field: PortalField::BaseUrl
            }
        );

        app.portal_input_buffer = "http://127.0.0.1:11434/v1".to_string();
        app.confirm_portal_input();
        assert_eq!(
            app.portal_input_mode,
            PortalInputMode::Adding {
                field: PortalField::ApiKey
            }
        );

        app.portal_input_buffer = "".to_string();
        app.confirm_portal_input();

        assert_eq!(app.portal_input_mode, PortalInputMode::Normal);
        assert_eq!(app.portals.len(), count_before + 1);
        let added = app.portals.last().unwrap();
        assert_eq!(added.name, "My Custom Ollama");
        assert_eq!(added.model_name, "ollama/deepseek-r1");
        assert_eq!(
            added.base_url,
            Some("http://127.0.0.1:11434/v1".to_string())
        );
        assert!(added.is_active);
    }
}
