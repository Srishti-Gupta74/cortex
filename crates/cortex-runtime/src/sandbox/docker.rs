//! Docker/OCI container execution sandbox backend with capability boundaries.

use super::{NetworkIsolationPolicy, Sandbox, SandboxExecutionResult, SandboxMode};
use cortex_core::{CortexError, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// Configuration parameters for containerized Docker execution sandboxes.
#[derive(Debug, Clone)]
pub struct DockerSandboxConfig {
    /// Base container image name (e.g., `alpine:latest`, `debian:bookworm-slim`).
    pub image: String,
    /// Host workspace root directory strictly mounted into `/workspace`.
    pub workspace_root: PathBuf,
    /// Network egress policy.
    pub network_policy: NetworkIsolationPolicy,
    /// Maximum memory limit in bytes (e.g., `512 * 1024 * 1024` for 512 MB).
    pub memory_limit_bytes: Option<u64>,
    /// CPU core allocation limit (e.g., `1.0` for 1 dedicated core).
    pub cpu_quota: Option<f32>,
    /// Maximum process count to prevent fork bombs.
    pub max_pids: Option<u32>,
    /// Whether to mount the container root filesystem as read-only.
    pub read_only_root_fs: bool,
    /// Execution timeout in milliseconds.
    pub timeout_ms: u64,
}

impl DockerSandboxConfig {
    /// Create a new configuration for the given workspace root.
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            image: "alpine:latest".to_string(),
            workspace_root: workspace_root.into(),
            network_policy: NetworkIsolationPolicy::Offline,
            memory_limit_bytes: Some(512 * 1024 * 1024), // 512 MB
            cpu_quota: Some(1.0),
            max_pids: Some(100),
            read_only_root_fs: true,
            timeout_ms: 30_000,
        }
    }

    /// Set container image.
    pub fn with_image(mut self, image: impl Into<String>) -> Self {
        self.image = image.into();
        self
    }

    /// Set network egress policy.
    pub fn with_network_policy(mut self, policy: NetworkIsolationPolicy) -> Self {
        self.network_policy = policy;
        self
    }

    /// Set memory limit in megabytes.
    pub fn with_memory_mb(mut self, mb: u64) -> Self {
        self.memory_limit_bytes = Some(mb * 1024 * 1024);
        self
    }

    /// Set CPU quota limit.
    pub fn with_cpu_quota(mut self, cpus: f32) -> Self {
        self.cpu_quota = Some(cpus);
        self
    }

    /// Set maximum process limit.
    pub fn with_max_pids(mut self, pids: u32) -> Self {
        self.max_pids = Some(pids);
        self
    }
}

/// Docker-based container execution sandbox enforcing isolation and resource boundaries.
pub struct DockerSandbox {
    config: DockerSandboxConfig,
}

impl DockerSandbox {
    /// Construct a new [`DockerSandbox`] with the specified configuration.
    pub fn new(config: DockerSandboxConfig) -> Self {
        Self { config }
    }

    /// Access the underlying configuration.
    pub fn config(&self) -> &DockerSandboxConfig {
        &self.config
    }

    /// Build the `docker run` argument vector according to configuration and security policies.
    ///
    /// Validates working directory containment to strictly prevent workspace escapes.
    pub fn build_docker_args(
        &self,
        command: &str,
        working_dir: Option<&Path>,
    ) -> Result<Vec<String>> {
        let canonical_workspace =
            self.config.workspace_root.canonicalize().map_err(|e| {
                CortexError::Validation(format!("invalid workspace root path: {e}"))
            })?;

        // Determine target working directory inside container
        let container_workdir = if let Some(sub_dir) = working_dir {
            if sub_dir.as_os_str().to_string_lossy().contains('\0') {
                return Err(CortexError::Validation(
                    "null-byte injection detected in working directory".to_string(),
                ));
            }

            let target = if sub_dir.is_absolute() {
                sub_dir.to_path_buf()
            } else {
                canonical_workspace.join(sub_dir)
            };

            let canonical_sub = target
                .canonicalize()
                .unwrap_or_else(|_| normalize_path(&target));

            // Prevent path traversal breakout
            if !canonical_sub.starts_with(&canonical_workspace) {
                return Err(CortexError::PermissionDenied(format!(
                    "working directory '{}' escapes workspace boundary '{}'",
                    canonical_sub.display(),
                    canonical_workspace.display()
                )));
            }

            let rel = canonical_sub
                .strip_prefix(&canonical_workspace)
                .map_err(|e| CortexError::PermissionDenied(format!("traversal escape: {e}")))?;

            if rel.as_os_str().is_empty() {
                "/workspace".to_string()
            } else {
                // The container uses POSIX separators even on a Windows host.
                let container_relative = rel
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                format!("/workspace/{container_relative}")
            }
        } else {
            "/workspace".to_string()
        };

        let mut args = vec![
            "run".to_string(),
            "--rm".to_string(),
            "-i".to_string(),
            // Security hardening: drop all capabilities and disallow privilege escalation
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges:true".to_string(),
        ];

        // Network isolation policy
        match self.config.network_policy {
            NetworkIsolationPolicy::Offline => {
                args.push("--network".to_string());
                args.push("none".to_string());
            }
            NetworkIsolationPolicy::IntranetOnly => {
                args.push("--network".to_string());
                args.push("internal".to_string());
            }
            NetworkIsolationPolicy::FullInternet => {
                args.push("--network".to_string());
                args.push("bridge".to_string());
            }
        }

        // Resource limits
        if let Some(mem) = self.config.memory_limit_bytes {
            args.push("--memory".to_string());
            args.push(format!("{mem}b"));
        }
        if let Some(cpus) = self.config.cpu_quota {
            args.push("--cpus".to_string());
            args.push(format!("{cpus:.2}"));
        }
        if let Some(pids) = self.config.max_pids {
            args.push("--pids-limit".to_string());
            args.push(pids.to_string());
        }

        // Read-only filesystem with ephemeral temporary mount
        if self.config.read_only_root_fs {
            args.push("--read-only".to_string());
            args.push("--tmpfs".to_string());
            args.push("/tmp:rw,noexec,nosuid,size=64m".to_string());
        }

        // Mount workspace volume confined to /workspace
        args.push("-v".to_string());
        args.push(format!("{}:/workspace:rw", canonical_workspace.display()));

        args.push("-w".to_string());
        args.push(container_workdir);

        args.push(self.config.image.clone());

        // Command execution inside container shell
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(command.to_string());

        Ok(args)
    }
}

impl Sandbox for DockerSandbox {
    fn mode(&self) -> SandboxMode {
        SandboxMode::Container
    }

    fn is_healthy(&self) -> Result<bool> {
        let output = Command::new("docker")
            .arg("version")
            .output()
            .map_err(|e| {
                CortexError::Internal(format!("docker binary not found or executable: {e}"))
            })?;

        Ok(output.status.success())
    }

    fn execute(&self, command: &str, working_dir: Option<&Path>) -> Result<SandboxExecutionResult> {
        let args = self.build_docker_args(command, working_dir)?;
        let start = Instant::now();

        let output = Command::new("docker")
            .args(&args)
            .output()
            .map_err(|e| CortexError::Internal(format!("failed to execute docker command: {e}")))?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(SandboxExecutionResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration_ms,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_docker_args_security_boundaries_and_containment() {
        let temp_dir =
            std::env::temp_dir().join(format!("cortex_docker_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let sub_dir = temp_dir.join("subdir");
        std::fs::create_dir_all(&sub_dir).unwrap();

        let config = DockerSandboxConfig::new(&temp_dir)
            .with_image("debian:bookworm-slim")
            .with_network_policy(NetworkIsolationPolicy::Offline)
            .with_memory_mb(256)
            .with_cpu_quota(0.5)
            .with_max_pids(50);

        let sandbox = DockerSandbox::new(config);

        // Valid execution within workspace
        let args = sandbox
            .build_docker_args("echo hello", Some(&sub_dir))
            .unwrap();
        assert!(args.contains(&"--cap-drop".to_string()));
        assert!(args.contains(&"ALL".to_string()));
        assert!(args.contains(&"none".to_string())); // network offline
        assert!(args.contains(&"--read-only".to_string()));
        assert!(args.contains(&"--memory".to_string()));
        assert!(args.contains(&"debian:bookworm-slim".to_string()));
        assert!(args.contains(&"/workspace/subdir".to_string()));

        // Out-of-bounds traversal attempt is strictly rejected
        let escape_dir = temp_dir.join("..").join("..");
        let err = sandbox.build_docker_args("cat /etc/passwd", Some(&escape_dir));
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), CortexError::PermissionDenied(_)));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_network_policy_flags() {
        let temp_dir = std::env::temp_dir().join(format!("cortex_net_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let config_online = DockerSandboxConfig::new(&temp_dir)
            .with_network_policy(NetworkIsolationPolicy::FullInternet);
        let online_sandbox = DockerSandbox::new(config_online);
        let online_args = online_sandbox.build_docker_args("ls", None).unwrap();
        assert!(online_args.contains(&"bridge".to_string()));

        let config_intra = DockerSandboxConfig::new(&temp_dir)
            .with_network_policy(NetworkIsolationPolicy::IntranetOnly);
        let intra_sandbox = DockerSandbox::new(config_intra);
        let intra_args = intra_sandbox.build_docker_args("ls", None).unwrap();
        assert!(intra_args.contains(&"internal".to_string()));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
