//! Interactive human-in-the-loop approval interception layer for dangerous and high-risk operations.

use cortex_core::permissions::{
    classify_operation, is_interactive_terminal, ApprovalDecision, ApprovalMode, HighRiskOperation,
};
use cortex_core::{CortexError, Result};
use std::fmt;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Handler trait for soliciting human or programmatic approval for high-risk operations.
pub trait ApprovalHandler: Send + Sync {
    /// Request approval for an intercepted high-risk operation.
    fn request_approval(&self, op: &HighRiskOperation) -> Result<ApprovalDecision>;
}

/// Interactive console approval handler that prompts the operator on standard error / standard input.
///
/// If standard input is not connected to a terminal (TTY), automatically falls back to `Deny`
/// to protect unattended headless environments (CI, cron runs, benchmarks).
#[derive(Debug, Default, Clone)]
pub struct InteractiveApprovalHandler;

impl InteractiveApprovalHandler {
    /// Create a new [`InteractiveApprovalHandler`].
    pub fn new() -> Self {
        Self
    }
}

impl ApprovalHandler for InteractiveApprovalHandler {
    fn request_approval(&self, op: &HighRiskOperation) -> Result<ApprovalDecision> {
        if !is_interactive_terminal() {
            tracing::warn!(
                tool = %op.tool_name,
                category = %op.category,
                reason = %op.reason,
                "headless execution detected: standard input is not a TTY, auto-rejecting high-risk operation"
            );
            return Ok(ApprovalDecision::Deny);
        }

        eprintln!("\n=== [Cortex Security Gate] HIGH-RISK OPERATION INTERCEPTED ===");
        eprintln!("Tool:        {}", op.tool_name);
        eprintln!("Category:    {}", op.category);
        eprintln!("Reason:      {}", op.reason);
        eprintln!("Description: {}", op.description);
        eprint!("Authorize this operation? [y]es / [n]o / [a]lways for session: ");
        let _ = std::io::stderr().flush();

        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_ok() {
            let trimmed = input.trim();
            if let Ok(decision) = trimmed.parse::<ApprovalDecision>() {
                return Ok(decision);
            }
        }

        Ok(ApprovalDecision::Deny)
    }
}

/// Programmable callback approval handler for closures and custom runners.
pub struct CallbackApprovalHandler<F>
where
    F: Fn(&HighRiskOperation) -> Result<ApprovalDecision> + Send + Sync,
{
    callback: F,
}

impl<F> CallbackApprovalHandler<F>
where
    F: Fn(&HighRiskOperation) -> Result<ApprovalDecision> + Send + Sync,
{
    /// Create a new approval handler backed by a closure.
    pub fn new(callback: F) -> Self {
        Self { callback }
    }
}

impl<F> ApprovalHandler for CallbackApprovalHandler<F>
where
    F: Fn(&HighRiskOperation) -> Result<ApprovalDecision> + Send + Sync,
{
    fn request_approval(&self, op: &HighRiskOperation) -> Result<ApprovalDecision> {
        (self.callback)(op)
    }
}

/// Static approval handler returning a predefined decision.
#[derive(Debug, Clone)]
pub struct StaticApprovalHandler {
    decision: ApprovalDecision,
}

impl StaticApprovalHandler {
    /// Create a new static handler returning the given decision.
    pub fn new(decision: ApprovalDecision) -> Self {
        Self { decision }
    }
}

impl ApprovalHandler for StaticApprovalHandler {
    fn request_approval(&self, _op: &HighRiskOperation) -> Result<ApprovalDecision> {
        Ok(self.decision)
    }
}

/// Thread-safe session authorization state maintaining "always allow" session approvals.
#[derive(Debug, Default)]
pub struct ApprovalSessionState {
    always_allowed: AtomicBool,
}

impl ApprovalSessionState {
    /// Create a new, fresh [`ApprovalSessionState`].
    pub fn new() -> Self {
        Self {
            always_allowed: AtomicBool::new(false),
        }
    }

    /// Check whether the operator has granted session-wide approval for high-risk operations.
    pub fn is_always_allowed(&self) -> bool {
        self.always_allowed.load(Ordering::SeqCst)
    }

    /// Update session-wide approval state.
    pub fn set_always_allowed(&self, val: bool) {
        self.always_allowed.store(val, Ordering::SeqCst);
    }

    /// Reset session authorization state.
    pub fn reset(&self) {
        self.set_always_allowed(false);
    }
}

/// Central interception engine enforcing approval policies on proposed tool calls.
#[derive(Clone)]
pub struct ApprovalInterceptor {
    mode: ApprovalMode,
    handler: Arc<dyn ApprovalHandler>,
    session_state: Arc<ApprovalSessionState>,
}

impl fmt::Debug for ApprovalInterceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApprovalInterceptor")
            .field("mode", &self.mode)
            .field("always_allowed", &self.session_state.is_always_allowed())
            .finish()
    }
}

impl ApprovalInterceptor {
    /// Create a new [`ApprovalInterceptor`] with the specified mode and handler.
    pub fn new(mode: ApprovalMode, handler: Arc<dyn ApprovalHandler>) -> Self {
        Self {
            mode,
            handler,
            session_state: Arc::new(ApprovalSessionState::new()),
        }
    }

    /// Create an [`ApprovalInterceptor`] sharing an existing [`ApprovalSessionState`].
    pub fn with_session_state(
        mode: ApprovalMode,
        handler: Arc<dyn ApprovalHandler>,
        session_state: Arc<ApprovalSessionState>,
    ) -> Self {
        Self {
            mode,
            handler,
            session_state,
        }
    }

    /// Create a default interactive interceptor in `Prompt` mode with TTY safety.
    pub fn interactive() -> Self {
        Self::new(
            ApprovalMode::Prompt,
            Arc::new(InteractiveApprovalHandler::new()),
        )
    }

    /// Create an interceptor in `Deny` mode that rejects all high-risk operations.
    pub fn deny() -> Self {
        Self::new(
            ApprovalMode::Deny,
            Arc::new(StaticApprovalHandler::new(ApprovalDecision::Deny)),
        )
    }

    /// Create an interceptor in `Allow` mode that permits all operations without prompting.
    pub fn allow() -> Self {
        Self::new(
            ApprovalMode::Allow,
            Arc::new(StaticApprovalHandler::new(ApprovalDecision::AllowOnce)),
        )
    }

    /// Create an interceptor for the specified mode with default handlers.
    pub fn from_mode(mode: ApprovalMode) -> Self {
        match mode {
            ApprovalMode::Prompt => Self::interactive(),
            ApprovalMode::Deny => Self::deny(),
            ApprovalMode::Allow => Self::allow(),
        }
    }

    /// Create an interceptor backed by a custom callback.
    pub fn with_callback<F>(mode: ApprovalMode, callback: F) -> Self
    where
        F: Fn(&HighRiskOperation) -> Result<ApprovalDecision> + Send + Sync + 'static,
    {
        Self::new(mode, Arc::new(CallbackApprovalHandler::new(callback)))
    }

    /// Return the active approval mode.
    pub fn mode(&self) -> ApprovalMode {
        self.mode
    }

    /// Mutate the active approval mode.
    pub fn set_mode(&mut self, mode: ApprovalMode) {
        self.mode = mode;
    }

    /// Access the shared session state handle.
    pub fn session_state(&self) -> Arc<ApprovalSessionState> {
        self.session_state.clone()
    }

    /// Check and authorize a proposed tool execution.
    ///
    /// Low-risk operations pass unconditionally.
    /// High-risk operations are checked against the configured policy mode, session approvals,
    /// or forwarded to the [`ApprovalHandler`].
    ///
    /// Returns `Ok(())` if authorized.
    /// Returns [`CortexError::PermissionDenied`] if rejected.
    pub fn check_approval(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        workspace_root: Option<&Path>,
    ) -> Result<()> {
        let (risk, op_opt) = classify_operation(tool_name, arguments, workspace_root);
        if risk.is_low_risk() {
            return Ok(());
        }

        let op = op_opt.unwrap_or_else(|| {
            HighRiskOperation::new(
                tool_name,
                "unspecified",
                "high-risk operation detected",
                format!("tool: {}, args: {}", tool_name, arguments),
            )
        });

        // Check if session-wide authorization has already been granted
        if self.session_state.is_always_allowed() {
            tracing::info!(
                tool = %tool_name,
                category = %op.category,
                "high-risk operation authorized by session-wide approval"
            );
            return Ok(());
        }

        match self.mode {
            ApprovalMode::Allow => {
                tracing::info!(
                    tool = %tool_name,
                    category = %op.category,
                    "high-risk operation automatically permitted by policy (mode: allow)"
                );
                Ok(())
            }
            ApprovalMode::Deny => {
                tracing::warn!(
                    tool = %tool_name,
                    category = %op.category,
                    "high-risk operation rejected by approval policy (mode: deny)"
                );
                Err(CortexError::PermissionDenied(format!(
                    "high-risk operation '{}' rejected by approval policy (mode: deny): {}",
                    tool_name, op.reason
                )))
            }
            ApprovalMode::Prompt => {
                let decision = self.handler.request_approval(&op)?;
                match decision {
                    ApprovalDecision::AllowOnce => {
                        tracing::info!(
                            tool = %tool_name,
                            category = %op.category,
                            "high-risk operation authorized once by operator"
                        );
                        Ok(())
                    }
                    ApprovalDecision::AllowAlways => {
                        tracing::info!(
                            tool = %tool_name,
                            category = %op.category,
                            "high-risk operation authorized for session by operator"
                        );
                        self.session_state.set_always_allowed(true);
                        Ok(())
                    }
                    ApprovalDecision::Deny => {
                        tracing::warn!(
                            tool = %tool_name,
                            category = %op.category,
                            "high-risk operation rejected by operator"
                        );
                        Err(CortexError::PermissionDenied(format!(
                            "high-risk operation '{}' was rejected by approval gate: {}",
                            tool_name, op.reason
                        )))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn test_approval_mode_allow_permits_high_risk() {
        let interceptor = ApprovalInterceptor::allow();
        let res = interceptor.check_approval("git_push", &json!({}), None);
        assert!(res.is_ok());

        let res = interceptor.check_approval("delete_file", &json!({"path": "foo.txt"}), None);
        assert!(res.is_ok());
    }

    #[test]
    fn test_approval_mode_deny_rejects_high_risk() {
        let interceptor = ApprovalInterceptor::deny();
        let res = interceptor.check_approval("git_push", &json!({}), None);
        assert!(res.is_err());
        assert!(matches!(res.unwrap_err(), CortexError::PermissionDenied(_)));

        // Low risk still passes
        let res = interceptor.check_approval("read_file", &json!({"path": "src/main.rs"}), None);
        assert!(res.is_ok());
    }

    #[test]
    fn test_approval_prompt_allow_once() {
        let prompt_count = Arc::new(AtomicUsize::new(0));
        let count_clone = prompt_count.clone();

        let interceptor = ApprovalInterceptor::with_callback(ApprovalMode::Prompt, move |_op| {
            count_clone.fetch_add(1, Ordering::SeqCst);
            Ok(ApprovalDecision::AllowOnce)
        });

        let res1 = interceptor.check_approval("git_push", &json!({}), None);
        assert!(res1.is_ok());
        assert_eq!(prompt_count.load(Ordering::SeqCst), 1);

        let res2 = interceptor.check_approval("git_push", &json!({}), None);
        assert!(res2.is_ok());
        assert_eq!(prompt_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_approval_prompt_allow_always_session() {
        let prompt_count = Arc::new(AtomicUsize::new(0));
        let count_clone = prompt_count.clone();

        let interceptor = ApprovalInterceptor::with_callback(ApprovalMode::Prompt, move |_op| {
            count_clone.fetch_add(1, Ordering::SeqCst);
            Ok(ApprovalDecision::AllowAlways)
        });

        let res1 = interceptor.check_approval("git_push", &json!({}), None);
        assert!(res1.is_ok());
        assert_eq!(prompt_count.load(Ordering::SeqCst), 1);

        // Subsequent call should bypass prompt because always_allowed is now true
        let res2 = interceptor.check_approval("delete_file", &json!({"path": "bar.txt"}), None);
        assert!(res2.is_ok());
        assert_eq!(prompt_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_approval_prompt_deny_decision() {
        let interceptor = ApprovalInterceptor::with_callback(ApprovalMode::Prompt, |_op| {
            Ok(ApprovalDecision::Deny)
        });

        let res = interceptor.check_approval("git_push", &json!({}), None);
        assert!(res.is_err());
        assert!(matches!(res.unwrap_err(), CortexError::PermissionDenied(_)));
    }
}
