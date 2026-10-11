# Sandbox Architecture

The sandbox forms an isolation layer between actions proposed by model agents and the host operating system. Cortex uses this layer so that untrusted commands cannot escape designated workspace paths or consume unbounded system resources.

## Architecture Overview

The sandbox sits between the runtime tool dispatcher and the underlying operating system. When an agent requests a shell command or file modification, the runtime passes the request through capability checks before handing it to a sandbox backend.

```text
[Agent Loop]
     |
     v
[Tool Registry / Capability Filter]
     |
     v
[Sandbox Trait]
     |
     +--> [Docker Sandbox]  --> (OCI Container: drop capabilities, offline network, cgroups)
     |
     +--> [Host Sandbox]    --> (Process Tree: workspace bounds, env scrub, job cleanup)
```

## Manifest Capability Permissions

Agents declare operational permissions in their manifest (`agent.yaml` or programmatic `AgentManifest`):

```yaml
permissions:
  filesystem: workspace
  shell: true
  network: false
  git:
    read: true
    commit: true
    push: false
```

### Capability Domains

1. **Filesystem (`filesystem`)**:
   - `workspace` / `workspace_only`: Restricts mutations strictly to the assigned workspace root and forbids path traversal (`..`).
   - `read_only` / `["read"]`: Allows inspection (`read_file`, `list_directory`, `grep`), but strictly blocks mutations (`write_file`, `edit_file`, `delete_file`).
   - Boolean (`true`/`false`): Shorthand enabling full workspace read/write or disallowing all filesystem operations.

2. **Shell Execution (`shell`)**:
   - Boolean (`false`): Completely disables subprocess execution tools (`shell`, `bash`, `execute_command`).
   - Binary Whitelist (e.g., `["cargo", "git"]`): Allows execution only for specified executable commands; unauthorized binaries are rejected before invocation.
   - Wildcard (`["*"]` / `true`): Allows unrestricted shell execution within the workspace directory.

3. **Network Access (`network`)**:
   - Boolean (`false`): Completely blocks network-capable tools and outgoing HTTP requests.
   - Host Whitelist (e.g., `["api.github.com", "crates.io"]`): Restricts outbound network connections to approved destinations.
   - Boolean (`true` / `["allow"]`): Permits outbound network connections.

4. **Git Operations (`git`)**:
   - Granular permissions: `read` (`git_status`, `git_diff`, `git_log`), `commit` (`git_commit`, branch creation), and `push` (`git_push`).
   - **Strict Push Policy**: Git push is disabled by default and requires explicit authorization (`git.push: true` or `git: ["push"]`). Even standard presets reject remote pushes.

## Interactive Human-in-the-Loop Approval Policy

To protect systems from destructive or irreversible actions proposed by autonomous model agents, Cortex implements an interactive approval interception layer (`ApprovalInterceptor` in `cortex-runtime` and `permissions` in `cortex-core`).

### High-Risk Operation Classification

Operations are classified into risk tiers prior to execution:

- **High-Risk Operations**:
  - `git push`: Remote pushes via `git_push`, `git` with push op, or shell `git push`.
  - **File Deletions**: File or directory deletions via `delete_file`, `remove_file`, `rm`, `del`, `Remove-Item`.
  - **Outbound Network**: Outgoing network requests (`curl`, `wget`, `http_request`, `fetch`, `web_search`, `git clone`).
  - **Arbitrary Package Installations**: Package manager execution (`pip install`, `npm install`, `cargo install/add`, `gem install`, `apt install`, `brew install`).
  - **Outside Workspace Paths**: Directory traversals (`..`) or commands targeting absolute paths outside the assigned workspace.
- **Low-Risk Operations**: Read-only inspections (`read_file`, `list_dir`, `git status`, `git diff`, `git log`, `grep`).

### Approval Modes

Operators configure authorization behavior globally in `~/.cortex/settings.json` or per-agent in `agent.yaml`:

| Mode | Behavior | Use Case |
| :--- | :--- | :--- |
| `prompt` (default) | Prompts operator interactively (`[y]es`, `[n]o`, `[a]lways for session`). | Interactive CLI/TUI developer workflows. |
| `deny` | Automatically rejects high-risk actions with `PermissionDenied`. | Strict unattended automation, safety evaluations. |
| `allow` | Permits high-risk actions without interactive confirmation. | Sandboxed headless pipelines, benchmark runs. |

### Headless & Automation Safety

When running in automated environments where standard input is not connected to a terminal (`!is_terminal()`, such as CI pipelines, cron jobs, or headless daemon runs), the interactive `prompt` mode automatically falls back to `deny` to prevent hung processes or unauthorized execution.

## Runtime Enforcement Pipeline

Runtime authorization is decoupled from model inference:

```text
+-------------------+      ToolCall      +-------------------+
|   Model Provider  | -----------------> |     AgentLoop     |
+-------------------+                    +-------------------+
                                                   |
                                       Approval Interceptor Checked
                                       (prompt / deny / allow / TTY)
                                                   v
                                         +-------------------+
                                         |   ToolRegistry    |
                                         +-------------------+
                                            /             \
                             Authorized    /               \  PermissionDenied
                                          v                 v
                                 +----------------+   +-------------------+
                                 |  Tool Execute  |   | ToolFailed Event  |
                                 +----------------+   | Recorded in Store |
                                                      +-------------------+
```

1. **Pre-execution Interception**: Prior to argument schema validation or subprocess invocation, `ApprovalInterceptor::check_approval` and `ToolRegistry::execute_with_permissions` inspect the operation. High-risk actions in `prompt` mode require explicit user authorization.
2. **Inviolable Security Boundary**: Model system prompts or user instructions cannot bypass capability enforcement or approval policies. Prohibited or rejected requests immediately fail with `CortexError::PermissionDenied`.
3. **Structured Auditability**: All permission denials and approval rejections are recorded as `ExecutionEvent::ToolFailed` events within persistent SQLite storage (`RunStore`), providing a verifiable audit log.

## Execution Backends

Cortex defines the `Sandbox` trait in `crates/cortex-runtime/src/sandbox/mod.rs` with two backends:

1. `HostSandbox`: Direct process execution on the host machine. Used for local CLI runs where Docker is not installed or when running trusted local tasks.
2. `DockerSandbox`: Isolated container execution using the Docker or Podman CLI. Used for running untrusted workflows, multi-agent tasks, and automated benchmarks.

Both backends return a `SandboxExecutionResult` containing process exit code, captured standard output, standard error, and execution duration in milliseconds.

## Docker Container Isolation

`DockerSandbox` configures OCI containers with restricted privileges using `DockerSandboxConfig`:

- **Dropped capabilities**: Drops all Linux capabilities (`--cap-drop ALL`) so child processes cannot perform privileged network or system administration operations.
- **No privilege escalation**: Disallows setuid binaries and privilege escalation (`--security-opt no-new-privileges:true`).
- **Read-only root filesystem**: Mounts the container root image as read-only (`--read-only`), preventing permanent filesystem changes outside mounted directories.
- **Workspace volume bind mount**: The host workspace is canonicalized and mounted into `/workspace`. Before execution, relative working directory paths are verified to ensure they resolve strictly within the workspace root. Traversal escapes attempting to leave the workspace are rejected with `PermissionDenied`.
- **Network isolation policies**: Supports three network modes via `NetworkIsolationPolicy`:
  - `Offline` (`--network none`): Complete network egress block. This is the default setting.
  - `IntranetOnly` (`--network internal`): Traffic restricted to user-defined container networks without internet routing.
  - `FullInternet` (`--network bridge`): Standard container network bridge.
- **Resource controls**:
  - Memory caps: Configured through `--memory <bytes>` (default 512 MB).
  - CPU quotas: Configured through `--cpus <quota>` (default 1.0 CPU).
  - Process limits: Configured through `--pids-limit <count>` (default 100) to block fork bombs.
- **Execution timeouts**: Commands run under a deadline (default 30 seconds). Processes exceeding the timeout are terminated.
- **Ephemeral lifecycle**: Containers run with `--rm` so state is discarded upon command completion.

## Host Execution Boundary

When Docker is unavailable, `HostSandbox` executes commands directly on the host operating system with minimal protections:

- **Working directory validation**: Ensures the command execution directory canonicalizes inside the configured workspace root.
- **Environment scrubbing**: Strips sensitive host environment variables (such as cloud provider credentials, SSH agent sockets, and API keys) before spawning the child process.
- **Process tree cleanup**: Child processes are attached to a dedicated process group on Unix or a Job Object on Windows, enabling cancellation routines to terminate child processes when cancelled or timed out.

## Sandbox Limitations and Non-Guarantees

Sandboxing in Cortex provides defense in depth. It does not provide absolute hardware isolation. Operators must account for the following technical limitations:

1. **Shared host kernel**: Docker containers share the host Linux kernel. Any unpatched kernel vulnerability or privilege escalation in the host kernel could allow a process to break container boundaries.
2. **Volume mount risks**: Because the workspace directory is mounted read-write into the container, a sandboxed process can modify, corrupt, or delete files inside that specific workspace directory. Host files outside the workspace remain protected, but workspace contents are mutable by design.
3. **Symlink traversal across volumes**: If a tool creates a symlink inside the workspace that points to a target outside the workspace, subsequent operations must resolve links carefully. The canonicalization checks in `DockerSandbox::build_docker_args` verify targets on the host before constructing container paths.
4. **Host socket exposure**: The Docker daemon socket (`/var/run/docker.sock`) must never be mounted into a sandbox. Doing so grants full root access to the host machine.
5. **HostSandbox has no filesystem virtualization**: `HostSandbox` is not a container. While it verifies working directories and scrubs environment variables, any process running under `HostSandbox` shares the host filesystem permissions of the running user. For untrusted code execution, `DockerSandbox` must be used.
