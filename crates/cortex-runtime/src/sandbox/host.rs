//! Host-based execution sandbox with workspace path containment.

use super::{Sandbox, SandboxExecutionResult, SandboxMode};
use cortex_core::{CortexError, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// Host sandbox enforcing local directory containment boundaries.
#[derive(Debug, Clone)]
pub struct HostSandbox {
    workspace_root: PathBuf,
}

impl HostSandbox {
    /// Create a new [`HostSandbox`] rooted at the specified workspace directory.
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    /// Access the workspace root path.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Validate that a requested target directory is strictly contained within the workspace root.
    pub fn validate_path(&self, target: &Path) -> Result<PathBuf> {
        if target.as_os_str().to_string_lossy().contains('\0') {
            return Err(CortexError::Validation(
                "null-byte injection detected in path".to_string(),
            ));
        }

        let canonical_root = self
            .workspace_root
            .canonicalize()
            .map_err(|e| CortexError::Validation(format!("invalid workspace root: {e}")))?;

        let raw_target = if target.is_absolute() {
            target.to_path_buf()
        } else {
            canonical_root.join(target)
        };

        let canonical_target = raw_target
            .canonicalize()
            .unwrap_or_else(|_| normalize_path(&raw_target));

        if !canonical_target.starts_with(&canonical_root) {
            return Err(CortexError::PermissionDenied(format!(
                "path '{}' escapes workspace root '{}'",
                canonical_target.display(),
                canonical_root.display()
            )));
        }

        Ok(canonical_target)
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

impl Sandbox for HostSandbox {
    fn mode(&self) -> SandboxMode {
        SandboxMode::Host
    }

    fn is_healthy(&self) -> Result<bool> {
        Ok(self.workspace_root.exists())
    }

    fn execute(&self, command: &str, working_dir: Option<&Path>) -> Result<SandboxExecutionResult> {
        let target_dir = match working_dir {
            Some(dir) => self.validate_path(dir)?,
            None => self
                .workspace_root
                .canonicalize()
                .map_err(|e| CortexError::Validation(format!("invalid workspace root: {e}")))?,
        };

        let start = Instant::now();

        #[cfg(windows)]
        let mut cmd = Command::new("cmd.exe");
        #[cfg(windows)]
        cmd.args(["/C", command]);

        #[cfg(not(windows))]
        let mut cmd = Command::new("sh");
        #[cfg(not(windows))]
        cmd.args(["-c", command]);

        cmd.current_dir(target_dir);

        // Scrub sensitive credentials and host tokens from sandboxed execution
        cmd.env_remove("AWS_SECRET_ACCESS_KEY");
        cmd.env_remove("AWS_SESSION_TOKEN");
        cmd.env_remove("OPENAI_API_KEY");
        cmd.env_remove("ANTHROPIC_API_KEY");
        cmd.env_remove("GITHUB_TOKEN");
        cmd.env_remove("GH_TOKEN");
        cmd.env_remove("SSH_AUTH_SOCK");
        cmd.env_remove("SSH_AGENT_PID");
        cmd.env_remove("CORTEX_API_KEY");

        let output = cmd.output().map_err(|e| {
            CortexError::Internal(format!("failed to execute command on host: {e}"))
        })?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(SandboxExecutionResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_sandbox_containment() {
        let temp_dir = std::env::temp_dir().join(format!("cortex_host_sb_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let sandbox = HostSandbox::new(&temp_dir);
        assert_eq!(sandbox.mode(), SandboxMode::Host);
        assert!(sandbox.is_healthy().unwrap());

        // Subdir ok
        let sub = temp_dir.join("inner");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(sandbox.validate_path(&sub).is_ok());

        // Escape denied
        let parent = temp_dir.join("..").join("..");
        assert!(sandbox.validate_path(&parent).is_err());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
