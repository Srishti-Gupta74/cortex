//! # cortex-core
//!
//! Foundational domain types, identifiers, error abstractions, event models,
//! and secret redaction for the Cortex agent runtime and harness.
//!
//! Cortex is an open-source runtime for autonomous AI workers.
//! This crate provides shared kernel primitives that higher-level crates
//! (`cortex-runtime`, `cortex-cli`) depend upon.

#![deny(missing_docs)]

pub mod error;
pub mod event;
pub mod id;
pub mod permissions;
pub mod redact;
pub mod settings;
pub mod workflow;

pub use error::{CortexError, Result};
pub use event::{EventRecord, ExecutionEvent};
pub use id::{AgentId, JobId, RunId, SessionId, WorkflowId};
pub use permissions::{
    classify_operation, is_interactive_terminal, ApprovalDecision, ApprovalMode, HighRiskOperation,
    RiskLevel,
};
pub use redact::{Redactor, SecretRedactor};
pub use settings::UserSettings;
pub use workflow::{WorkflowAgentRef, WorkflowManifest, WorkflowStage};

/// Current semantic version of the Cortex core library.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_available() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn test_error_formatting() {
        let err = CortexError::NotFound("test-resource".to_string());
        assert_eq!(err.to_string(), "resource not found: test-resource");
    }
}
