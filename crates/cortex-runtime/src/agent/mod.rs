//! Agent execution context, iteration coordinator, cancellation, approval interception, and persistent lifecycle management.

pub mod approval;
pub mod bus;
pub mod coordination;
pub mod execution;
pub mod lifecycle;
pub mod manager;
pub mod manifest;
pub mod message;
pub mod registry;
pub mod task_queue;

pub use approval::{
    ApprovalHandler, ApprovalInterceptor, ApprovalSessionState, CallbackApprovalHandler,
    InteractiveApprovalHandler, StaticApprovalHandler,
};
pub use bus::{BusMessage, BusReceiver, DeadLetterEnvelope, DeadLetterReason, MessageBus};
pub use coordination::{AgentEndpoint, AgentMessage};
pub use execution::{
    AgentContext, AgentLoop, AgentRunResult, CancellationToken, ChatMessage, PermissionState,
    SessionMetadata, TodoItem, CODING_AGENT_POLICY,
};
pub use lifecycle::{AgentLifecycleEvent, AgentState};
pub use manager::{Agent, AgentManager, AgentManifest, AgentModelConfig, AgentPermissions};
pub use manifest::{AgentYamlManifest, AgentYamlModel, AgentYamlPermissions, YamlPermissionValue};
pub use message::{AgentMessagePayload, MessageType, RoutingKey};
pub use registry::{AgentCapability, AgentDescriptor, AgentRegistry};
pub use task_queue::{TaskPriority, TaskQueue, TaskStatus, WorkflowCoordinator, WorkflowTask};
