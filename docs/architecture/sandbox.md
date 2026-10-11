# Sandbox Architecture

The sandbox is a security boundary between model-generated actions and the host system. It enforces defense in depth, ensuring that model-generated prompts cannot exceed explicitly granted capability boundaries.

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
   - `workspace` / `workspace_only`: Restricts mutations to the assigned workspace root and strictly forbids path traversal (`..`).
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

## Runtime Enforcement Pipeline

Runtime authorization is decoupled from model inference:

```
+-------------------+      ToolCall      +-------------------+
|   Model Provider  | -----------------> |     AgentLoop     |
+-------------------+                    +-------------------+
                                                   |
                                       Active Permissions Checked
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

1. **Pre-execution Interception**: Prior to argument schema validation or subprocess invocation, `ToolRegistry::execute_with_permissions` calls `AgentPermissions::validate_tool_call`.
2. **Inviolable Security Boundary**: Model system prompts or user instructions cannot bypass capability enforcement. Prohibited tool requests immediately fail with `CortexError::PermissionDenied`.
3. **Structured Auditability**: All permission denials are recorded as `ExecutionEvent::ToolFailed` events within persistent SQLite storage (`RunStore`), providing a verifiable audit log.

## Process and Container Sandboxing

In addition to capability filtering, the runtime enforces execution boundaries:

- **Host Containment**:
  - Windows: Nested process tree tracking via Windows Job Objects prevents orphaned subprocesses.
  - Unix: Process group sessions (`setpgid`) ensure clean group reaping upon timeout or cancellation.
  - Environment Scrubbing: Sensitive host API keys and tokens (`AWS_SECRET_ACCESS_KEY`, `OPENAI_API_KEY`, `GITHUB_TOKEN`, etc.) are stripped before subprocess spawning.
- **Docker-backed Sandboxing**:
  - Workspace volume mounts (read-only where configured)
  - Network isolation policies (`none`, `bridge`, or proxy filtering)
  - Execution timeouts and CPU/memory resource limits
  - Non-root UID/GID execution where practical

Sandboxing should always be treated as defense in depth, combining declarative manifest permissions, runtime capability gating, and kernel/container isolation.
