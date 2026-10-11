//! Shell tool for executing commands within the workspace boundary.

use crate::agent::CancellationToken;
use crate::tool::{Tool, ToolDefinition, ToolResult};
use crate::workspace::Workspace;
use cortex_core::{CortexError, Result};
use serde_json::json;
use std::process::Command;
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Tool for executing shell commands inside the workspace root.
pub struct ShellTool {
    workspace: Arc<Workspace>,
    def: ToolDefinition,
}

impl ShellTool {
    /// Create a new [`ShellTool`].
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Self {
            workspace,
            def: ToolDefinition::new(
                "shell",
                "Executes a command inside the workspace directory and returns output",
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                }),
            ),
        }
    }
}

impl Tool for ShellTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn execute(&self, input: &serde_json::Value) -> Result<ToolResult> {
        execute_shell_command(&self.workspace, input, None)
    }

    fn execute_with_cancellation(
        &self,
        input: &serde_json::Value,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<ToolResult> {
        execute_shell_command(&self.workspace, input, cancellation_token)
    }
}

/// Helper function to execute shell command within workspace bounds.
fn execute_shell_command(
    workspace: &Workspace,
    input: &serde_json::Value,
    cancellation_token: Option<&CancellationToken>,
) -> Result<ToolResult> {
    if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
        return Err(CortexError::Cancelled(
            "shell execution cancelled by request".to_string(),
        ));
    }
    let command_str = input["command"].as_str().ok_or_else(|| {
        CortexError::Validation("missing required 'command' parameter".to_string())
    })?;

    let workdir = if let Some(sub) = input.get("working_dir").and_then(|w| w.as_str()) {
        workspace.resolve_path(sub)?
    } else {
        workspace.root().to_path_buf()
    };

    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", command_str]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command_str]);
        #[cfg(unix)]
        c.process_group(0);
        c
    };

    // Pin working directory to workspace bounds
    cmd.current_dir(workdir);

    // Scrub sensitive host environment variables
    cmd.env_remove("AWS_SECRET_ACCESS_KEY");
    cmd.env_remove("AWS_SESSION_TOKEN");
    cmd.env_remove("OPENAI_API_KEY");
    cmd.env_remove("ANTHROPIC_API_KEY");
    cmd.env_remove("GITHUB_TOKEN");
    cmd.env_remove("GH_TOKEN");
    cmd.env_remove("SSH_AUTH_SOCK");
    cmd.env_remove("SSH_AGENT_PID");
    cmd.env_remove("CORTEX_API_KEY");

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    #[cfg(windows)]
    let process_tree = super::process_tree::ProcessTree::new()?;

    let mut child = cmd.spawn().map_err(|e| {
        CortexError::Internal(format!(
            "failed to execute command '{}': {}",
            command_str, e
        ))
    })?;

    #[cfg(windows)]
    if let Err(error) = process_tree.attach(&child) {
        child
            .kill()
            .map_err(|e| CortexError::Internal(format!("failed to kill unconfined shell: {e}")))?;
        child
            .wait()
            .map_err(|e| CortexError::Internal(format!("failed to reap unconfined shell: {e}")))?;
        return Err(error);
    }

    let stdout_pipe = child.stdout.take().expect("piped stdout");
    let stderr_pipe = child.stderr.take().expect("piped stderr");
    let (output_tx, output_rx) = std::sync::mpsc::channel();
    for (is_stderr, mut pipe) in [
        (
            false,
            Box::new(stdout_pipe) as Box<dyn std::io::Read + Send>,
        ),
        (true, Box::new(stderr_pipe) as Box<dyn std::io::Read + Send>),
    ] {
        let tx = output_tx.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
            // Receiver closure means the owning shell invocation has ended.
            let _ = tx.send((is_stderr, result));
        });
    }
    drop(output_tx);

    let mut stdout_bytes = None;
    let mut stderr_bytes = None;
    let mut status = None;
    let mut cancelled = false;
    loop {
        if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
            cancelled = true;
            break;
        }
        if status.is_none() {
            status = child.try_wait().map_err(|e| {
                CortexError::Internal(format!("failed waiting for child process: {e}"))
            })?;
        }
        if status.is_some() && stdout_bytes.is_some() && stderr_bytes.is_some() {
            break;
        }
        match output_rx.recv_timeout(std::time::Duration::from_millis(25)) {
            Ok((is_stderr, output)) => {
                let bytes = output.map_err(|e| {
                    CortexError::Internal(format!("failed reading shell output: {e}"))
                })?;
                if is_stderr {
                    stderr_bytes = Some(bytes);
                } else {
                    stdout_bytes = Some(bytes);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if stdout_bytes.is_none() || stderr_bytes.is_none() {
                    return Err(CortexError::Internal(
                        "shell output worker disconnected".to_string(),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    }

    if cancelled {
        #[cfg(windows)]
        process_tree.terminate()?;
        #[cfg(unix)]
        {
            // The process group survives a direct shell's exit while descendants
            // retain inherited output pipes.
            let pgid = child.id() as i32;
            if unsafe { kill(-pgid, 9) } != 0 {
                let error = std::io::Error::last_os_error();
                // ESRCH means every group member has already exited.
                if error.raw_os_error() != Some(3) {
                    return Err(CortexError::Internal(format!(
                        "failed to kill shell process group: {error}"
                    )));
                }
            }
        }
        child
            .wait()
            .map_err(|e| CortexError::Internal(format!("failed to reap cancelled shell: {e}")))?;
        return Err(CortexError::Cancelled(format!(
            "command '{command_str}' cancelled by request"
        )));
    }

    let status = status.expect("child exit observed");
    let stdout_bytes = stdout_bytes.expect("stdout completed");
    let stderr_bytes = stderr_bytes.expect("stderr completed");

    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let stderr = String::from_utf8_lossy(&stderr_bytes);

    let combined = if stderr.is_empty() {
        stdout.to_string()
    } else if stdout.is_empty() {
        stderr.to_string()
    } else {
        format!("{}\n{}", stdout, stderr)
    };

    if status.success() {
        Ok(ToolResult::success(combined))
    } else {
        let exit_code = status.code().unwrap_or(-1);
        Ok(ToolResult::error(format!(
            "Command exited with code {}:\n{}",
            exit_code, combined
        )))
    }
}

/// Tool for executing bash commands (Claude Code standard tool 'bash').
pub struct BashTool {
    workspace: Arc<Workspace>,
    def: ToolDefinition,
}

impl BashTool {
    /// Create a new [`BashTool`].
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Self {
            workspace,
            def: ToolDefinition::new(
                "bash",
                "Executes a bash command within the workspace boundary and returns stdout/stderr",
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "working_dir": { "type": "string" }
                    },
                    "required": ["command"]
                }),
            )
            .with_permission(crate::tool::PermissionLevel::Execute),
        }
    }
}

impl Tool for BashTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn execute(&self, input: &serde_json::Value) -> Result<ToolResult> {
        execute_shell_command(&self.workspace, input, None)
    }

    fn execute_with_cancellation(
        &self,
        input: &serde_json::Value,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<ToolResult> {
        execute_shell_command(&self.workspace, input, cancellation_token)
    }
}

/// Tool for executing CLI commands ('execute_command').
pub struct ExecuteCommandTool {
    workspace: Arc<Workspace>,
    def: ToolDefinition,
}

impl ExecuteCommandTool {
    /// Create a new [`ExecuteCommandTool`].
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Self {
            workspace,
            def: ToolDefinition::new(
                "execute_command",
                "Executes a CLI command within workspace bounds",
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "working_dir": { "type": "string" }
                    },
                    "required": ["command"]
                }),
            )
            .with_permission(crate::tool::PermissionLevel::Execute),
        }
    }
}

impl Tool for ExecuteCommandTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn execute(&self, input: &serde_json::Value) -> Result<ToolResult> {
        execute_shell_command(&self.workspace, input, None)
    }

    fn execute_with_cancellation(
        &self,
        input: &serde_json::Value,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<ToolResult> {
        execute_shell_command(&self.workspace, input, cancellation_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_tool_execution() {
        let temp_dir =
            std::env::temp_dir().join(format!("cortex_test_shell_{}", std::process::id()));
        let ws = Arc::new(Workspace::new(&temp_dir).unwrap());
        let shell = ShellTool::new(ws);

        let res = shell
            .execute(&json!({
                "command": "echo 'cortex test'"
            }))
            .unwrap();

        assert!(!res.is_error);
        assert!(res.output.contains("cortex test"));

        // Clean up
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_shell_tool_failure() {
        let temp_dir =
            std::env::temp_dir().join(format!("cortex_test_shell_fail_{}", std::process::id()));
        let ws = Arc::new(Workspace::new(&temp_dir).unwrap());
        let shell = ShellTool::new(ws);

        let res = shell
            .execute(&json!({
                "command": "exit 1"
            }))
            .unwrap();

        assert!(res.is_error);
        assert!(res.output.contains("Command exited with code 1"));

        // Clean up
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_shell_tool_cancellation() {
        let temp_dir =
            std::env::temp_dir().join(format!("cortex_test_shell_cancel_{}", std::process::id()));
        let ws = Arc::new(Workspace::new(&temp_dir).unwrap());
        let shell = ShellTool::new(ws);

        let cancel_token = CancellationToken::new();
        let cancel_clone = cancel_token.clone();

        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            cancel_clone.cancel();
        });

        let cmd = if cfg!(target_os = "windows") {
            "ping -n 10 127.0.0.1 > nul"
        } else {
            "sleep 10"
        };

        let start = std::time::Instant::now();
        let res = shell.execute_with_cancellation(&json!({ "command": cmd }), Some(&cancel_token));
        let elapsed = start.elapsed();

        assert!(res.is_err());
        match res.unwrap_err() {
            CortexError::Cancelled(msg) => {
                assert!(msg.contains("cancelled by request"));
            }
            other => panic!("expected Cancelled error, got: {:?}", other),
        }
        assert!(elapsed < std::time::Duration::from_secs(4));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
