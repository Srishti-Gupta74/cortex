# Threat Model

## Assets

- source code
- credentials
- environment variables
- Git repositories
- filesystem data
- network access
- agent state
- execution history

## Adversaries

- malicious or compromised model output
- malicious tool arguments
- malicious MCP server
- malicious Agent Skill
- untrusted repository contents
- prompt injection
- compromised dependencies

## Core defenses

- capability-based permissions
- workspace boundaries
- sandboxing
- environment filtering
- tool validation
- timeouts
- resource limits
- audit/event tracing
- security CI

## Important principle

Prompt instructions are not a security boundary.

Security must be enforced by the runtime.

## Cancellation cleanup

Cancelling a built-in model request drops pending HTTP I/O. Cancelling shell
execution terminates its Unix process group or Windows job and reaps the direct
child. Output readers do not block the caller's cancellation indefinitely.
Already-cancelled tool dispatch is rejected before execution. Cancellation does
not grant capabilities or replace workspace and permission validation.

Process groups and job objects support cleanup; they are not containment against
malicious descendants that escape a group or spawn during Windows job attachment.
Container isolation remains a separate security boundary.

## Inter-Agent Boundaries & Multi-Agent Threat Model

- **Boundary Containment**: Delegating a task cannot grant capabilities the child worker lacks. Agent permissions are strictly bound to individual manifests (`AgentPermissions`); task delegation never copies, inherits, or expands capabilities across hierarchy boundaries.
- **Privilege Escalation Prevention**: An agent cannot forge its `sender` identity in the message envelope. `AgentManager` binds sender identities exclusively through issued `AgentEndpoint` handles in trusted host code; deserialized envelopes cannot claim unverified authority.
- **Prompt Injection Defense**: Inter-agent messages are treated as untrusted data inputs, never as runtime execution authority. Enqueued message payloads (`AgentMessagePayload`) do not trigger automatic tool dispatch or shell execution; all actions pass through runtime capability policies.
- **Hierarchy Integrity**: Supervisor-worker relationships form a directed acyclic graph. Direct and transitive cycles (A → B → A) are rejected at configuration time. Rogue agents cannot delegate tasks to workers assigned to other supervisors.
- **In-Process Scope**: Inter-agent messaging is an in-process runtime coordination abstraction; it does not isolate malicious host code with direct memory access to `AgentManager`.

## Sandbox Penetration Testing & Breakout Defenses

Cortex maintains an automated penetration test suite (`crates/cortex-runtime/tests/sandbox_security_test.rs`) that continuously tests adversarial payload delivery against execution boundaries.

### 1. Directory Traversal & Filesystem Escapes
- **Relative traversal (`../`)**: Path resolution verifies logical and canonical ancestor paths against the workspace boundary. Sequences like `../../../../etc/passwd` or `src/../../outside` are rejected with `PermissionDenied`.
- **Absolute escapes**: Absolute paths pointing outside the workspace root are detected and blocked before filesystem access.
- **Null-byte injections**: Paths containing embedded null bytes (`\0`) are rejected immediately with `Validation` errors before delegating to OS APIs.
- **Symlink escapes**: Symlinks pointing outside the workspace root resolve to their canonical target and are rejected with `PermissionDenied`.
- **Unicode containment**: Valid Unicode paths inside the workspace resolve correctly, while directory traversal sequences wrapped inside Unicode directory structures remain strictly contained.

### 2. Environment Variable Scrubbing
Commands dispatched to local subprocesses via `ShellTool` or `HostSandbox` explicitly scrub sensitive environment variables prior to spawning children:
- Cloud & API credentials: `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `CORTEX_API_KEY`
- SCM credentials: `GITHUB_TOKEN`, `GH_TOKEN`
- SSH agent handles: `SSH_AUTH_SOCK`, `SSH_AGENT_PID`

This ensures compromised model outputs or malicious repository scripts cannot exfiltrate host credentials.

### 3. Container Network Isolation
The container sandbox (`DockerSandbox`) enforces egress boundaries:
- **Offline policy**: Passes `--network none` to isolate container execution completely from host and external networks.
- **Intranet-only policy**: Passes `--network internal` to prevent outbound internet access while allowing communication with designated local services.
- **Full internet policy**: Passes `--network bridge` for outbound internet access when explicitly permitted by agent capability policy.

### 4. Adversarial Process Quotas & Lifecycle Limits
- **Fork bomb prevention**: Containers apply `--pids-limit` (default: 100, configurable) to cap process table growth.
- **Memory & CPU caps**: Containers apply `--memory` and `--cpus` to prevent denial-of-service via resource exhaustion.
- **Filesystem immutability**: Root container filesystems are mounted with `--read-only`, restricting writes to the mounted `/workspace` directory and a restricted, ephemeral tmpfs (`/tmp:rw,noexec,nosuid,size=64m`).
- **Capability reduction**: Containers drop all Linux capabilities (`--cap-drop ALL`) and disable privilege escalation (`--security-opt no-new-privileges:true`).
- **Timeout and cancellation enforcement**: Long-running or spinning commands are terminated via `CancellationToken`, which terminates Unix process groups or Windows process trees and reaps child processes without leaking zombie processes.
