//! AgentManager coordinator for managing persistent agent worker lifecycles.

use super::lifecycle::{AgentLifecycleEvent, AgentState};
use crate::storage::{AgentCheckpointRecord, AgentRecord, RunStore};
use chrono::Utc;
use cortex_core::{AgentId, CortexError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};

/// Configuration for an agent's model provider and inference parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentModelConfig {
    /// Model identifier or name (e.g., "claude-3-5-sonnet", "gpt-4o", "mock").
    pub model: String,
    /// Model provider identifier (e.g., "anthropic", "openai", "mock").
    pub provider: String,
    /// Sampling temperature (0.0 to 2.0).
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Maximum completion tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    /// Additional provider-specific parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

fn default_temperature() -> f32 {
    0.7
}

impl AgentModelConfig {
    /// Create a new [`AgentModelConfig`] with default temperature and parameters.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            temperature: default_temperature(),
            max_tokens: None,
            parameters: None,
        }
    }

    /// Set a custom sampling temperature.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = temperature;
        self
    }

    /// Set a maximum token limit.
    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

/// Operational permission boundaries for an agent.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentPermissions {
    /// Allowed filesystem operations (e.g. `["read", "write"]`, `["read"]`, `["workspace_only"]`).
    #[serde(default)]
    pub filesystem: Vec<String>,
    /// Allowed network destinations or hosts (e.g. `["allow"]`, `[]`, or hostnames).
    #[serde(default)]
    pub network: Vec<String>,
    /// Allowed shell commands or command patterns (e.g. `["*"]`, `["cargo", "git"]`, `[]`).
    #[serde(default)]
    pub shell: Vec<String>,
    /// Allowed git operations (e.g. `["read", "commit"]`, `["read"]`, `["push"]`).
    #[serde(default)]
    pub git: Vec<String>,
}

impl AgentPermissions {
    /// Create full permissions with read/write filesystem, network access, and standard git operations.
    pub fn standard() -> Self {
        Self {
            filesystem: vec!["read".to_string(), "write".to_string()],
            network: vec!["allow".to_string()],
            shell: vec!["*".to_string()],
            git: vec!["read".to_string(), "commit".to_string()],
        }
    }

    /// Create read-only permissions restricting mutations.
    pub fn read_only() -> Self {
        Self {
            filesystem: vec!["read".to_string()],
            network: Vec::new(),
            shell: Vec::new(),
            git: vec!["read".to_string()],
        }
    }

    /// Check if filesystem read operations are permitted.
    pub fn allows_filesystem_read(&self) -> bool {
        if self.filesystem.is_empty() {
            return false;
        }
        self.filesystem.iter().any(|f| {
            f == "read" || f == "write" || f == "workspace_only" || f == "workspace" || f == "*"
        })
    }

    /// Check if filesystem write operations are permitted.
    pub fn allows_filesystem_write(&self) -> bool {
        if self.filesystem.is_empty() {
            return false;
        }
        if self.filesystem.iter().any(|f| f == "read_only") {
            return false;
        }
        self.filesystem.iter().any(|f| f == "write" || f == "*")
    }

    /// Check if shell execution is permitted, optionally checking a specific command string.
    pub fn allows_shell(&self, command: Option<&str>) -> bool {
        if self.shell.is_empty() {
            return false;
        }
        if self
            .shell
            .iter()
            .any(|s| s == "*" || s == "true" || s == "allow")
        {
            return true;
        }
        if let Some(cmd) = command {
            let trimmed = cmd.trim();
            let first_word = trimmed.split_whitespace().next().unwrap_or(trimmed);
            self.shell.iter().any(|s| s == trimmed || s == first_word)
        } else {
            true
        }
    }

    /// Check if network operations are permitted, optionally checking a destination hostname or URL.
    pub fn allows_network(&self, destination: Option<&str>) -> bool {
        if self.network.is_empty() {
            return false;
        }
        if self
            .network
            .iter()
            .any(|n| n == "*" || n == "allow" || n == "true")
        {
            return true;
        }
        if let Some(dest) = destination {
            self.network.iter().any(|n| n == dest || dest.contains(n))
        } else {
            true
        }
    }

    /// Check if the specified git operation is permitted.
    ///
    /// Git push operations are strictly prohibited unless `"push"` is explicitly listed.
    pub fn allows_git(&self, op: &str) -> bool {
        match op {
            "push" => self.git.iter().any(|g| g == "push"),
            "commit" | "write" => self
                .git
                .iter()
                .any(|g| g == "commit" || g == "write" || g == "*" || g == "allow" || g == "true"),
            "read" => self.git.iter().any(|g| {
                g == "read"
                    || g == "commit"
                    || g == "write"
                    || g == "*"
                    || g == "allow"
                    || g == "true"
            }),
            _ => self.git.iter().any(|g| g == op || g == "*"),
        }
    }

    /// Validate a requested filesystem path against workspace boundary constraints.
    pub fn validate_filesystem_path(&self, path: &str) -> Result<()> {
        if self
            .filesystem
            .iter()
            .any(|f| f == "workspace_only" || f == "workspace")
            && path.contains("..")
        {
            return Err(CortexError::PermissionDenied(format!(
                "path traversal ('..') is prohibited by workspace_only policy: '{}'",
                path
            )));
        }
        Ok(())
    }

    /// Validate a proposed tool call against these permissions prior to execution.
    ///
    /// Returns [`CortexError::PermissionDenied`] if the action violates configured boundaries.
    pub fn validate_tool_call(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        level: crate::tool::PermissionLevel,
    ) -> Result<()> {
        match tool_name {
            "read_file" | "list_dir" | "list_directory" => {
                if !self.allows_filesystem_read() {
                    return Err(CortexError::PermissionDenied(format!(
                        "filesystem read operations are not permitted (tool '{}')",
                        tool_name
                    )));
                }
                if let Some(path) = arguments.get("path").and_then(|p| p.as_str()) {
                    self.validate_filesystem_path(path)?;
                }
            }
            "write_file" | "edit_file" | "delete_file" => {
                if !self.allows_filesystem_write() {
                    return Err(CortexError::PermissionDenied(format!(
                        "filesystem write operations are not permitted (tool '{}')",
                        tool_name
                    )));
                }
                if let Some(path) = arguments.get("path").and_then(|p| p.as_str()) {
                    self.validate_filesystem_path(path)?;
                }
            }
            "shell" | "bash" | "execute_command" => {
                let cmd = arguments.get("command").and_then(|c| c.as_str());
                if !self.allows_shell(cmd) {
                    let cmd_desc = cmd.unwrap_or("<unknown>");
                    return Err(CortexError::PermissionDenied(format!(
                        "shell command execution is not permitted: '{}'",
                        cmd_desc
                    )));
                }
            }
            "git_push" => {
                if !self.allows_git("push") {
                    return Err(CortexError::PermissionDenied(
                        "git push is not permitted by capability permissions".to_string(),
                    ));
                }
            }
            "git_commit" => {
                if !self.allows_git("commit") {
                    return Err(CortexError::PermissionDenied(
                        "git commit is not permitted by capability permissions".to_string(),
                    ));
                }
            }
            "git_branch" => {
                let is_create = arguments
                    .get("create")
                    .and_then(|c| c.as_bool())
                    .unwrap_or(false);
                if is_create {
                    if !self.allows_git("commit") {
                        return Err(CortexError::PermissionDenied(
                            "git branch creation is not permitted by capability permissions"
                                .to_string(),
                        ));
                    }
                } else if !self.allows_git("read") {
                    return Err(CortexError::PermissionDenied(
                        "git branch inspection is not permitted by capability permissions"
                            .to_string(),
                    ));
                }
            }
            "git_status" | "git_diff" | "git_log" => {
                if !self.allows_git("read") {
                    return Err(CortexError::PermissionDenied(format!(
                        "git read operations are not permitted (tool '{}')",
                        tool_name
                    )));
                }
            }
            _ => {
                // Check if this appears to be a network-related tool
                if tool_name.contains("network")
                    || tool_name.contains("http")
                    || tool_name.contains("fetch")
                    || tool_name.contains("curl")
                    || tool_name.contains("web_search")
                    || arguments.get("url").is_some()
                {
                    let url = arguments.get("url").and_then(|u| u.as_str());
                    if !self.allows_network(url) {
                        return Err(CortexError::PermissionDenied(format!(
                            "network access is not permitted for tool '{}'",
                            tool_name
                        )));
                    }
                }

                // Fall back to PermissionLevel checks
                match level {
                    crate::tool::PermissionLevel::WorkspaceWrite => {
                        if !self.allows_filesystem_write() {
                            return Err(CortexError::PermissionDenied(format!(
                                "workspace write operations are not permitted for tool '{}'",
                                tool_name
                            )));
                        }
                    }
                    crate::tool::PermissionLevel::Execute => {
                        let cmd = arguments.get("command").and_then(|c| c.as_str());
                        if !self.allows_shell(cmd) {
                            return Err(CortexError::PermissionDenied(format!(
                                "subprocess execution is not permitted for tool '{}'",
                                tool_name
                            )));
                        }
                    }
                    crate::tool::PermissionLevel::Danger => {
                        return Err(CortexError::PermissionDenied(format!(
                            "dangerous tool operations are not permitted for tool '{}'",
                            tool_name
                        )));
                    }
                    crate::tool::PermissionLevel::ReadOnly => {}
                }
            }
        }
        Ok(())
    }
}

fn default_role() -> String {
    "Autonomous Worker".to_string()
}

fn deserialize_model_config<'de, D>(
    deserializer: D,
) -> std::result::Result<AgentModelConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Helper {
        Full(AgentModelConfig),
        Simple(String),
    }

    match Helper::deserialize(deserializer)? {
        Helper::Full(cfg) => Ok(cfg),
        Helper::Simple(model_name) => {
            let provider = if model_name.starts_with("claude") {
                "anthropic"
            } else if model_name.starts_with("ollama") {
                "ollama"
            } else {
                "openai"
            };
            Ok(AgentModelConfig::new(provider, model_name))
        }
    }
}

/// Declarative manifest used to instantiate a new agent worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentManifest {
    /// Optional explicitly specified ID (generated if not provided).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<AgentId>,
    /// Human-readable agent worker name.
    pub name: String,
    /// Operational role and task domain (e.g., "Code Reviewer", "Software Engineer").
    #[serde(default = "default_role")]
    pub role: String,
    /// System prompt or custom operational policy instructions.
    #[serde(default, alias = "policy", skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Assigned workspace directory boundary.
    pub workspace: PathBuf,
    /// Model provider and parameter configuration.
    #[serde(deserialize_with = "deserialize_model_config")]
    pub model: AgentModelConfig,
    /// Allowed tool definitions or names.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Capability and operational permission boundaries.
    #[serde(default)]
    pub permissions: AgentPermissions,
    /// Whether to automatically resume on runtime boot if interrupted.
    #[serde(default)]
    pub auto_resume: bool,
}

impl AgentManifest {
    /// Create a new basic manifest with default configurations.
    pub fn new(
        name: impl Into<String>,
        role: impl Into<String>,
        workspace: impl AsRef<Path>,
        model: AgentModelConfig,
    ) -> Self {
        Self {
            id: None,
            name: name.into(),
            role: role.into(),
            system_prompt: None,
            workspace: workspace.as_ref().to_path_buf(),
            model,
            tools: Vec::new(),
            permissions: AgentPermissions::standard(),
            auto_resume: false,
        }
    }

    /// Attach an explicit [`AgentId`].
    pub fn with_id(mut self, id: AgentId) -> Self {
        self.id = Some(id);
        self
    }

    /// Attach a custom system prompt.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Attach tool names to the manifest.
    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.tools = tools;
        self
    }

    /// Attach permissions to the manifest.
    pub fn with_permissions(mut self, permissions: AgentPermissions) -> Self {
        self.permissions = permissions;
        self
    }

    /// Attach an auto-resume flag on daemon restart.
    pub fn with_auto_resume(mut self, auto_resume: bool) -> Self {
        self.auto_resume = auto_resume;
        self
    }

    /// Deserialize an [`AgentManifest`] from a YAML string.
    pub fn from_yaml_str(yaml_str: &str) -> Result<Self> {
        if let Ok(yaml_manifest) = super::manifest::AgentYamlManifest::from_yaml_str(yaml_str) {
            return yaml_manifest.into_manifest();
        }

        serde_yaml::from_str(yaml_str)
            .map_err(|e| CortexError::Validation(format!("invalid yaml manifest: {}", e)))
    }

    /// Deserialize an [`AgentManifest`] from a YAML file.
    pub fn from_yaml_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|e| {
            CortexError::NotFound(format!(
                "failed to read manifest file '{}': {}",
                path.display(),
                e
            ))
        })?;
        Self::from_yaml_str(&content)
    }

    /// Serialize this [`AgentManifest`] to a YAML string.
    pub fn to_yaml_string(&self) -> Result<String> {
        serde_yaml::to_string(self).map_err(|e| {
            CortexError::Internal(format!("failed to serialize manifest to yaml: {}", e))
        })
    }

    /// Deserialize an [`AgentManifest`] from a JSON string.
    pub fn from_json_str(json_str: &str) -> Result<Self> {
        serde_json::from_str(json_str)
            .map_err(|e| CortexError::Validation(format!("invalid json manifest: {}", e)))
    }

    /// Deserialize an [`AgentManifest`] from a TOML string.
    pub fn from_toml_str(toml_str: &str) -> Result<Self> {
        toml::from_str(toml_str)
            .map_err(|e| CortexError::Validation(format!("invalid toml manifest: {}", e)))
    }

    /// Serialize this [`AgentManifest`] to a pretty-printed JSON string.
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|e| {
            CortexError::Internal(format!("failed to serialize manifest to json: {}", e))
        })
    }

    /// Serialize this [`AgentManifest`] to a TOML string.
    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| {
            CortexError::Internal(format!("failed to serialize manifest to toml: {}", e))
        })
    }

    /// Load and deserialize an [`AgentManifest`] from a file (supports JSON, YAML, and TOML).
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let p = path.as_ref();
        if !p.is_file() {
            return Err(CortexError::NotFound(format!(
                "manifest file '{}' not found",
                p.display()
            )));
        }
        let content = std::fs::read_to_string(p).map_err(|e| {
            CortexError::Internal(format!(
                "failed to read manifest file '{}': {}",
                p.display(),
                e
            ))
        })?;

        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        match ext.to_ascii_lowercase().as_str() {
            "json" => Self::from_json_str(&content),
            "toml" => Self::from_toml_str(&content),
            "yaml" | "yml" => Self::from_yaml_str(&content),
            _ => {
                // Try JSON, then YAML, then TOML
                if let Ok(m) = Self::from_json_str(&content) {
                    Ok(m)
                } else if let Ok(m) = Self::from_yaml_str(&content) {
                    Ok(m)
                } else {
                    Self::from_toml_str(&content)
                }
            }
        }
    }
}

/// A persistent agent worker managed by the runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    /// Unique stable identifier.
    pub id: AgentId,
    /// Human-readable name.
    pub name: String,
    /// Operational role definition.
    pub role: String,
    /// Custom system prompt or operational instructions.
    pub system_prompt: Option<String>,
    /// Current lifecycle state.
    pub state: AgentState,
    /// Assigned workspace boundary path.
    pub workspace: PathBuf,
    /// Model provider configuration.
    pub model: AgentModelConfig,
    /// Configured tool capabilities.
    pub tools: Vec<String>,
    /// Granted permission boundaries.
    pub permissions: AgentPermissions,
    /// Whether to automatically resume execution upon runtime reboot.
    #[serde(default)]
    pub auto_resume: bool,
    /// Creation timestamp in ISO 8601 UTC.
    pub created_at: String,
    /// Last state transition timestamp in ISO 8601 UTC.
    pub updated_at: String,
}

impl Agent {
    /// Create an execution [`AgentContext`](super::execution::AgentContext) preconfigured
    /// with this agent's operational permissions, workspace boundary path, and prompt policy.
    pub fn create_context(&self, task: impl Into<String>) -> super::execution::AgentContext {
        let mut ctx = super::execution::AgentContext::new(task);
        if let Some(ref prompt) = self.system_prompt {
            ctx.messages = vec![
                super::execution::ChatMessage::System(prompt.clone()),
                super::execution::ChatMessage::User(ctx.task.clone()),
            ];
        }
        ctx.current_dir = self.workspace.clone();
        ctx.with_permissions(self.permissions.clone())
    }
}

/// Thread-safe manager responsible for agent workers and their lifecycle transitions.
#[derive(Debug, Clone, Default)]
pub struct AgentManager {
    pub(super) agents: Arc<RwLock<HashMap<AgentId, Agent>>>,
    pub(super) coordination: Arc<Mutex<super::coordination::CoordinationState>>,
    pub(super) registry: Arc<super::registry::AgentRegistry>,
    events: Arc<RwLock<Vec<AgentLifecycleEvent>>>,
    subscribers: Arc<Mutex<Vec<Sender<AgentLifecycleEvent>>>>,
    store: Option<Arc<RunStore>>,
}

impl AgentManager {
    /// Create a new, empty [`AgentManager`].
    pub fn new() -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            coordination: Arc::new(Mutex::new(super::coordination::CoordinationState::default())),
            registry: Arc::new(super::registry::AgentRegistry::new()),
            events: Arc::new(RwLock::new(Vec::new())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            store: None,
        }
    }

    /// Access the [`AgentRegistry`](super::registry::AgentRegistry) associated with this manager.
    pub fn registry(&self) -> Arc<super::registry::AgentRegistry> {
        Arc::clone(&self.registry)
    }
    /// Create an [`AgentManager`] attached to persistent SQLite storage.
    pub fn new_with_store(store: Arc<RunStore>) -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            coordination: Arc::new(Mutex::new(super::coordination::CoordinationState::default())),
            registry: Arc::new(super::registry::AgentRegistry::new()),
            events: Arc::new(RwLock::new(Vec::new())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            store: Some(store),
        }
    }

    /// Attach persistent storage to an existing [`AgentManager`].
    pub fn with_store(mut self, store: Arc<RunStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Access attached persistent storage if available.
    pub fn store(&self) -> Option<&Arc<RunStore>> {
        self.store.as_ref()
    }

    /// Load and rehydrate all agents from persistent storage, performing restart recovery.
    pub fn load_from_store(store: Arc<RunStore>) -> Result<Self> {
        let records = store.list_agents()?;
        let manager = Self::new_with_store(Arc::clone(&store));

        for record in records {
            let manifest = match AgentManifest::from_yaml_str(&record.manifest_yaml) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("Skipping unparseable agent record '{}': {}", record.id, e);
                    continue;
                }
            };

            let mut state = record
                .status
                .parse::<AgentState>()
                .unwrap_or(AgentState::Stopped);
            let mut updated_at = record.updated_at.clone();

            // Reconcile interrupted Running agents on restart
            if state == AgentState::Running {
                if manifest.auto_resume {
                    tracing::info!(
                        "Agent '{}' ({}) configured for auto-resume",
                        record.name,
                        record.id
                    );
                } else {
                    state = AgentState::Stopped;
                    updated_at = Utc::now().to_rfc3339();
                    let _ = store.update_agent_status(&record.id, "stopped");
                }
            }

            let agent = Agent {
                id: record.id.clone(),
                name: record.name,
                role: manifest.role,
                system_prompt: manifest.system_prompt,
                state,
                workspace: manifest.workspace,
                model: manifest.model,
                tools: manifest.tools,
                permissions: manifest.permissions,
                auto_resume: manifest.auto_resume,
                created_at: record.created_at,
                updated_at,
            };

            if let Ok(mut lock) = manager.agents.write() {
                lock.insert(record.id, agent);
            }
        }

        Ok(manager)
    }

    /// Register a new agent worker from a manifest.
    ///
    /// Validates the manifest and transitions the new agent to [`AgentState::Created`].
    pub fn create(&self, manifest: AgentManifest) -> Result<Agent> {
        let name = manifest.name.trim();
        if name.is_empty() {
            return Err(CortexError::Validation(
                "agent name cannot be empty".to_string(),
            ));
        }

        let role = manifest.role.trim();
        if role.is_empty() {
            return Err(CortexError::Validation(
                "agent role cannot be empty".to_string(),
            ));
        }

        let id = manifest.id.clone().unwrap_or_else(AgentId::generate);
        let now = Utc::now().to_rfc3339();

        let agent = Agent {
            id: id.clone(),
            name: name.to_string(),
            role: role.to_string(),
            system_prompt: manifest.system_prompt.clone(),
            state: AgentState::Created,
            workspace: manifest.workspace.clone(),
            model: manifest.model.clone(),
            tools: manifest.tools.clone(),
            permissions: manifest.permissions.clone(),
            auto_resume: manifest.auto_resume,
            created_at: now.clone(),
            updated_at: now.clone(),
        };

        if let Some(ref store) = self.store {
            let yaml = manifest.to_yaml_string().unwrap_or_default();
            let record = AgentRecord {
                id: id.clone(),
                name: name.to_string(),
                manifest_yaml: yaml,
                status: AgentState::Created.as_str().to_string(),
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            store.save_agent(&record)?;
        }

        {
            let mut lock = self
                .agents
                .write()
                .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
            if lock.contains_key(&id) {
                return Err(CortexError::Validation(format!(
                    "agent with id '{}' already exists",
                    id
                )));
            }
            lock.insert(id.clone(), agent.clone());
        }

        let _ = self
            .registry
            .upsert(super::registry::AgentDescriptor::from_agent(&agent));

        self.emit_event(AgentLifecycleEvent::AgentCreated {
            agent_id: id,
            name: name.to_string(),
            role: role.to_string(),
            timestamp: now,
        });

        Ok(agent)
    }

    /// Prepare an agent for execution, moving it from [`AgentState::Created`] to [`AgentState::Ready`].
    pub fn prepare(&self, agent_id: &AgentId) -> Result<()> {
        let mut lock = self
            .agents
            .write()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        let agent = lock
            .get_mut(agent_id)
            .ok_or_else(|| CortexError::NotFound(format!("agent '{}' not found", agent_id)))?;

        if agent.state == AgentState::Ready {
            return Ok(());
        }

        let mut coordination = self
            .coordination
            .lock()
            .map_err(super::coordination::lock_error)?;
        agent.state.transition_to(AgentState::Ready)?;
        let now = Utc::now().to_rfc3339();
        agent.updated_at = now.clone();

        let _ = self.registry.update_status(agent_id, AgentState::Ready);

        let waker = coordination.revoke(agent_id);
        drop(coordination);

        if let Some(ref store) = self.store {
            let _ = store.update_agent_status(agent_id, "ready");
        }

        self.emit_event(AgentLifecycleEvent::AgentReady {
            agent_id: agent_id.clone(),
            timestamp: now,
        });
        drop(lock);
        if let Some(waker) = waker {
            waker.wake();
        }

        Ok(())
    }

    /// Transition an agent to [`AgentState::Running`].
    ///
    /// Permitted from [`AgentState::Created`], [`AgentState::Ready`], or [`AgentState::Paused`].
    /// Stopped agents cannot be started directly and must be restarted first.
    pub fn start(&self, agent_id: &AgentId) -> Result<()> {
        let mut lock = self
            .agents
            .write()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        let agent = lock
            .get_mut(agent_id)
            .ok_or_else(|| CortexError::NotFound(format!("agent '{}' not found", agent_id)))?;

        match agent.state {
            AgentState::Running => {
                // Already running, idempotent success.
                return Ok(());
            }
            AgentState::Stopped => {
                return Err(CortexError::Validation(format!(
                    "cannot start stopped agent '{}'; use restart() to reset before running",
                    agent_id
                )));
            }
            AgentState::Failed => {
                return Err(CortexError::Validation(format!(
                    "cannot start failed agent '{}'; use restart() to reset before running",
                    agent_id
                )));
            }
            AgentState::Created | AgentState::Ready | AgentState::Paused => {
                agent.state.transition_to(AgentState::Running)?;
                let now = Utc::now().to_rfc3339();
                agent.updated_at = now.clone();

                let _ = self.registry.update_status(agent_id, AgentState::Running);
                if let Some(ref store) = self.store {
                    let _ = store.update_agent_status(agent_id, "running");
                }

                self.emit_event(AgentLifecycleEvent::AgentStarted {
                    agent_id: agent_id.clone(),
                    timestamp: now,
                });
            }
        }

        Ok(())
    }

    /// Transition an agent to [`AgentState::Paused`].
    ///
    /// Only running agents may be paused.
    pub fn pause(&self, agent_id: &AgentId) -> Result<()> {
        let mut lock = self
            .agents
            .write()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        let agent = lock
            .get_mut(agent_id)
            .ok_or_else(|| CortexError::NotFound(format!("agent '{}' not found", agent_id)))?;

        if agent.state == AgentState::Paused {
            return Ok(());
        }

        if agent.state != AgentState::Running {
            return Err(CortexError::Validation(format!(
                "cannot pause agent '{}' in state '{}'; only running agents can be paused",
                agent_id, agent.state
            )));
        }

        let mut coordination = self
            .coordination
            .lock()
            .map_err(super::coordination::lock_error)?;
        agent.state.transition_to(AgentState::Paused)?;
        let now = Utc::now().to_rfc3339();
        agent.updated_at = now.clone();

        let _ = self.registry.update_status(agent_id, AgentState::Paused);

        let waker = coordination.notify(agent_id);
        drop(coordination);

        if let Some(ref store) = self.store {
            let _ = store.update_agent_status(agent_id, "paused");
        }

        self.emit_event(AgentLifecycleEvent::AgentPaused {
            agent_id: agent_id.clone(),
            timestamp: now,
        });
        drop(lock);
        if let Some(waker) = waker {
            waker.wake();
        }

        Ok(())
    }

    /// Resume a paused agent back to [`AgentState::Running`].
    pub fn resume(&self, agent_id: &AgentId) -> Result<()> {
        let mut lock = self
            .agents
            .write()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        let agent = lock
            .get_mut(agent_id)
            .ok_or_else(|| CortexError::NotFound(format!("agent '{}' not found", agent_id)))?;

        if agent.state == AgentState::Running {
            return Ok(());
        }

        if agent.state != AgentState::Paused {
            return Err(CortexError::Validation(format!(
                "cannot resume agent '{}' in state '{}'; only paused agents can be resumed",
                agent_id, agent.state
            )));
        }

        agent.state.transition_to(AgentState::Running)?;
        let now = Utc::now().to_rfc3339();
        agent.updated_at = now.clone();

        let _ = self.registry.update_status(agent_id, AgentState::Running);
        if let Some(ref store) = self.store {
            let _ = store.update_agent_status(agent_id, "running");
        }

        self.emit_event(AgentLifecycleEvent::AgentResumed {
            agent_id: agent_id.clone(),
            timestamp: now,
        });

        Ok(())
    }

    /// Stop an agent worker, moving it to [`AgentState::Stopped`].
    pub fn stop(&self, agent_id: &AgentId) -> Result<()> {
        let mut lock = self
            .agents
            .write()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        let agent = lock
            .get_mut(agent_id)
            .ok_or_else(|| CortexError::NotFound(format!("agent '{}' not found", agent_id)))?;

        if agent.state == AgentState::Stopped {
            return Ok(());
        }

        let mut coordination = self
            .coordination
            .lock()
            .map_err(super::coordination::lock_error)?;
        agent.state.transition_to(AgentState::Stopped)?;
        let now = Utc::now().to_rfc3339();
        agent.updated_at = now.clone();

        let _ = self.registry.update_status(agent_id, AgentState::Stopped);

        let waker = coordination.revoke(agent_id);
        drop(coordination);

        if let Some(ref store) = self.store {
            let _ = store.update_agent_status(agent_id, "stopped");
        }

        self.emit_event(AgentLifecycleEvent::AgentStopped {
            agent_id: agent_id.clone(),
            reason: None,
            timestamp: now,
        });
        drop(lock);
        if let Some(waker) = waker {
            waker.wake();
        }

        Ok(())
    }

    /// Restart a stopped or failed agent, resetting its lifecycle state to [`AgentState::Ready`].
    pub fn restart(&self, agent_id: &AgentId) -> Result<()> {
        let mut lock = self
            .agents
            .write()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        let agent = lock
            .get_mut(agent_id)
            .ok_or_else(|| CortexError::NotFound(format!("agent '{}' not found", agent_id)))?;

        let mut coordination = self
            .coordination
            .lock()
            .map_err(super::coordination::lock_error)?;
        agent.state.transition_to(AgentState::Ready)?;
        let now = Utc::now().to_rfc3339();
        agent.updated_at = now.clone();

        let _ = self.registry.update_status(agent_id, AgentState::Ready);

        let waker = coordination.revoke(agent_id);
        drop(coordination);

        if let Some(ref store) = self.store {
            let _ = store.update_agent_status(agent_id, "ready");
        }
        self.emit_event(AgentLifecycleEvent::AgentReady {
            agent_id: agent_id.clone(),
            timestamp: now,
        });
        drop(lock);
        if let Some(waker) = waker {
            waker.wake();
        }

        Ok(())
    }

    /// Mark an agent as failed due to an unrecoverable execution error.
    pub fn fail(&self, agent_id: &AgentId, error: impl Into<String>) -> Result<()> {
        let err_str = error.into();
        let mut lock = self
            .agents
            .write()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        let agent = lock
            .get_mut(agent_id)
            .ok_or_else(|| CortexError::NotFound(format!("agent '{}' not found", agent_id)))?;

        let mut coordination = self
            .coordination
            .lock()
            .map_err(super::coordination::lock_error)?;
        agent.state.transition_to(AgentState::Failed)?;
        let now = Utc::now().to_rfc3339();
        agent.updated_at = now.clone();

        let _ = self.registry.update_status(agent_id, AgentState::Failed);

        let waker = coordination.revoke(agent_id);
        drop(coordination);

        if let Some(ref store) = self.store {
            let _ = store.update_agent_status(agent_id, "failed");
        }
        self.emit_event(AgentLifecycleEvent::AgentFailed {
            agent_id: agent_id.clone(),
            error: err_str,
            timestamp: now,
        });
        drop(lock);
        if let Some(waker) = waker {
            waker.wake();
        }

        Ok(())
    }

    /// Inspect details, configuration, and state for a single agent.
    pub fn inspect(&self, agent_id: &AgentId) -> Result<Agent> {
        let lock = self
            .agents
            .read()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        lock.get(agent_id)
            .cloned()
            .ok_or_else(|| CortexError::NotFound(format!("agent '{}' not found", agent_id)))
    }

    /// List all registered agents.
    pub fn list(&self) -> Vec<Agent> {
        let lock = self.agents.read().unwrap_or_else(|e| e.into_inner());
        let mut list: Vec<Agent> = lock.values().cloned().collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }

    /// Remove a stopped or created agent from management.
    ///
    /// Running or paused agents cannot be removed until stopped.
    pub fn remove(&self, agent_id: &AgentId) -> Result<Agent> {
        let mut lock = self
            .agents
            .write()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;

        if let Some(agent) = lock.get(agent_id) {
            if agent.state.is_active() {
                return Err(CortexError::Validation(format!(
                    "cannot remove agent '{}' while in active state '{}'; stop it first",
                    agent_id, agent.state
                )));
            }
        }

        let mut coordination = self
            .coordination
            .lock()
            .map_err(super::coordination::lock_error)?;
        let removed = lock
            .remove(agent_id)
            .ok_or_else(|| CortexError::NotFound(format!("agent '{}' not found", agent_id)))?;
        let _ = self.registry.deregister(agent_id);
        let waker = coordination.remove(agent_id);
        drop(coordination);
        drop(lock);

        if let Some(ref store) = self.store {
            let _ = store.delete_agent(agent_id);
        }

        if let Some(waker) = waker {
            waker.wake();
        }

        Ok(removed)
    }

    /// Record a persistence execution checkpoint for the given agent.
    pub fn save_checkpoint(
        &self,
        agent_id: &AgentId,
        step: i64,
        data_json: Option<String>,
    ) -> Result<i64> {
        let agent = self.inspect(agent_id)?;
        let store = self.store.as_ref().ok_or_else(|| {
            CortexError::Internal(
                "cannot save checkpoint: no persistent storage attached".to_string(),
            )
        })?;

        let now = Utc::now().to_rfc3339();
        let checkpoint = AgentCheckpointRecord {
            id: None,
            agent_id: agent_id.clone(),
            step,
            state: agent.state.as_str().to_string(),
            data_json,
            created_at: now,
        };

        store.save_agent_checkpoint(&checkpoint)
    }

    /// List recorded execution checkpoints for the given agent.
    pub fn list_checkpoints(&self, agent_id: &AgentId) -> Result<Vec<AgentCheckpointRecord>> {
        let store = self.store.as_ref().ok_or_else(|| {
            CortexError::Internal(
                "cannot list checkpoints: no persistent storage attached".to_string(),
            )
        })?;

        store.list_agent_checkpoints(agent_id)
    }

    /// Subscribe to the live stream of agent lifecycle events.
    pub fn subscribe(&self) -> Receiver<AgentLifecycleEvent> {
        let (tx, rx) = channel();
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.push(tx);
        }
        rx
    }

    /// Access the historical sequence of recorded lifecycle events.
    pub fn events(&self) -> Vec<AgentLifecycleEvent> {
        let lock = self.events.read().unwrap_or_else(|e| e.into_inner());
        lock.clone()
    }

    /// Retrieve recorded lifecycle events specific to the given [`AgentId`].
    pub fn events_for_agent(&self, agent_id: &AgentId) -> Vec<AgentLifecycleEvent> {
        let lock = self.events.read().unwrap_or_else(|e| e.into_inner());
        lock.iter()
            .filter(|e| e.agent_id() == agent_id)
            .cloned()
            .collect()
    }

    /// Check whether an agent is in an execution state permitting tool execution.
    ///
    /// Tool execution is permitted only when an agent is in [`AgentState::Running`].
    pub fn can_execute_tool(&self, agent_id: &AgentId) -> Result<bool> {
        let lock = self
            .agents
            .read()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        let agent = lock
            .get(agent_id)
            .ok_or_else(|| CortexError::NotFound(format!("agent '{}' not found", agent_id)))?;
        Ok(agent.state == AgentState::Running)
    }

    /// Authorize a tool execution for the specified agent.
    ///
    /// Returns a validation error if the agent is not in [`AgentState::Running`], or if the tool is not permitted.
    pub fn authorize_tool_execution(&self, agent_id: &AgentId, tool_name: &str) -> Result<()> {
        let lock = self
            .agents
            .read()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        let agent = lock
            .get(agent_id)
            .ok_or_else(|| CortexError::NotFound(format!("agent '{}' not found", agent_id)))?;

        if agent.state != AgentState::Running {
            return Err(CortexError::Validation(format!(
                "cannot execute tool '{}' on agent '{}' in state '{}'; only running agents can execute tools",
                tool_name, agent_id, agent.state
            )));
        }

        if !agent.tools.is_empty() && !agent.tools.iter().any(|t| t == tool_name) {
            return Err(CortexError::Validation(format!(
                "tool '{}' is not authorized for agent '{}'",
                tool_name, agent_id
            )));
        }

        Ok(())
    }

    /// Reconcile agents left in active or interrupted states after an unhandled process termination or crash.
    ///
    /// Transitions all [`AgentState::Running`] agents to [`AgentState::Stopped`], unless configured
    /// with `auto_resume: true`. Updates persistent storage if attached.
    pub fn reconcile_crashed_agents(&self) -> Result<usize> {
        let mut lock = self
            .agents
            .write()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        let mut reconciled = 0;
        let now = Utc::now().to_rfc3339();
        let mut auto_resume_ids = Vec::new();

        for agent in lock.values_mut() {
            if agent.state == AgentState::Running {
                if agent.auto_resume {
                    auto_resume_ids.push(agent.id.clone());
                } else {
                    agent.state = AgentState::Stopped;
                    agent.updated_at = now.clone();
                    reconciled += 1;
                }
            }
        }

        if let Some(ref store) = self.store {
            let _ = store.reconcile_crashed_agents_in_store(&auto_resume_ids);
        }

        Ok(reconciled)
    }

    /// Load registered agents from a persistent JSON file.
    pub fn load_from_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(());
        }
        let content = std::fs::read_to_string(path).map_err(|e| {
            CortexError::Internal(format!(
                "failed to read agents file '{}': {}",
                path.display(),
                e
            ))
        })?;
        if content.trim().is_empty() {
            return Ok(());
        }
        let loaded: Vec<Agent> = serde_json::from_str(&content).map_err(|e| {
            CortexError::Internal(format!(
                "failed to deserialize agents from '{}': {}",
                path.display(),
                e
            ))
        })?;
        let mut lock = self
            .agents
            .write()
            .map_err(|e| CortexError::Internal(format!("agents lock poisoned: {}", e)))?;
        for agent in loaded {
            let desc = super::registry::AgentDescriptor::from_agent(&agent);
            let _ = self.registry.upsert(desc);
            lock.insert(agent.id.clone(), agent);
        }
        Ok(())
    }

    /// Save all registered agents to a persistent JSON file.
    pub fn save_to_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let list = self.list();
        let json_str = serde_json::to_string_pretty(&list)
            .map_err(|e| CortexError::Internal(format!("failed to serialize agents: {}", e)))?;
        std::fs::write(path, json_str).map_err(|e| {
            CortexError::Internal(format!(
                "failed to write agents to '{}': {}",
                path.display(),
                e
            ))
        })?;
        Ok(())
    }

    fn emit_event(&self, event: AgentLifecycleEvent) {
        if let Ok(mut events) = self.events.write() {
            events.push(event.clone());
        }

        if let Ok(mut subs) = self.subscribers.lock() {
            subs.retain(|tx| tx.send(event.clone()).is_ok());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manifest(name: &str) -> AgentManifest {
        AgentManifest::new(
            name,
            "Rust Developer",
            "/tmp/workspace",
            AgentModelConfig::new("mock", "gpt-4o"),
        )
    }

    #[test]
    fn test_agent_creation_and_inspect() {
        let manager = AgentManager::new();
        let manifest = test_manifest("reviewer");
        let agent = manager.create(manifest).expect("agent creation");

        assert_eq!(agent.name, "reviewer");
        assert_eq!(agent.role, "Rust Developer");
        assert_eq!(agent.state, AgentState::Created);

        let inspected = manager.inspect(&agent.id).expect("inspect");
        assert_eq!(inspected.id, agent.id);
        assert_eq!(inspected.state, AgentState::Created);
    }

    #[test]
    fn test_lifecycle_flow_start_pause_resume_stop() {
        let manager = AgentManager::new();
        let agent = manager.create(test_manifest("worker")).unwrap();
        let id = agent.id;

        // Start
        manager.start(&id).unwrap();
        assert_eq!(manager.inspect(&id).unwrap().state, AgentState::Running);

        // Pause
        manager.pause(&id).unwrap();
        assert_eq!(manager.inspect(&id).unwrap().state, AgentState::Paused);

        // Resume
        manager.resume(&id).unwrap();
        assert_eq!(manager.inspect(&id).unwrap().state, AgentState::Running);

        // Stop
        manager.stop(&id).unwrap();
        assert_eq!(manager.inspect(&id).unwrap().state, AgentState::Stopped);

        // Cannot start directly when stopped
        let err = manager.start(&id).unwrap_err();
        assert!(err.to_string().contains("cannot start stopped agent"));

        // Restart to Ready
        manager.restart(&id).unwrap();
        assert_eq!(manager.inspect(&id).unwrap().state, AgentState::Ready);

        // Start from Ready
        manager.start(&id).unwrap();
        assert_eq!(manager.inspect(&id).unwrap().state, AgentState::Running);
    }

    #[test]
    fn test_event_streaming_subscription() {
        let manager = AgentManager::new();
        let rx = manager.subscribe();

        let agent = manager.create(test_manifest("stream-test")).unwrap();
        let id = agent.id;
        manager.start(&id).unwrap();
        manager.stop(&id).unwrap();

        let ev1 = rx.recv().unwrap();
        assert_eq!(ev1.event_type(), "AgentCreated");

        let ev2 = rx.recv().unwrap();
        assert_eq!(ev2.event_type(), "AgentStarted");

        let ev3 = rx.recv().unwrap();
        assert_eq!(ev3.event_type(), "AgentStopped");

        let history = manager.events_for_agent(&id);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn test_thread_safe_concurrent_operations() {
        use std::thread;

        let manager = Arc::new(AgentManager::new());
        let mut handles = Vec::new();

        for i in 0..10 {
            let m = Arc::clone(&manager);
            handles.push(thread::spawn(move || {
                let manifest = test_manifest(&format!("worker_{}", i));
                let agent = m.create(manifest).unwrap();
                m.start(&agent.id).unwrap();
                m.inspect(&agent.id).unwrap();
                m.stop(&agent.id).unwrap();
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(manager.list().len(), 10);
    }

    #[test]
    fn test_remove_validation() {
        let manager = AgentManager::new();
        let agent = manager.create(test_manifest("removable")).unwrap();

        // Start agent
        manager.start(&agent.id).unwrap();

        // Removing active agent fails
        let err = manager.remove(&agent.id).unwrap_err();
        assert!(err.to_string().contains("cannot remove agent"));

        // Stop and remove succeeds
        manager.stop(&agent.id).unwrap();
        assert!(manager.remove(&agent.id).is_ok());
        assert!(manager.inspect(&agent.id).is_err());
    }

    #[test]
    fn test_yaml_manifest_parsing() {
        let yaml = r#"
name: "reviewer"
role: "Code Reviewer"
workspace: "./src"
policy: "Review code changes"
model:
  provider: "anthropic"
  model: "claude-3-5-sonnet"
tools:
  - read_file
  - git_diff
"#;
        let manifest = AgentManifest::from_yaml_str(yaml).unwrap();
        assert_eq!(manifest.name, "reviewer");
        assert_eq!(manifest.role, "Code Reviewer");
        assert_eq!(
            manifest.system_prompt.as_deref(),
            Some("Review code changes")
        );
        assert_eq!(manifest.model.model, "claude-3-5-sonnet");
        assert_eq!(manifest.model.provider, "anthropic");
        assert_eq!(manifest.tools, vec!["read_file", "git_diff"]);
    }

    #[test]
    fn test_agent_manager_file_persistence() {
        let tmp_dir = std::env::temp_dir().join(format!("cortex_mgr_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp_dir);
        let file_path = tmp_dir.join("agents.json");

        let manager1 = AgentManager::new();
        let manifest = test_manifest("persisted-agent");
        let agent = manager1.create(manifest).unwrap();
        manager1.start(&agent.id).unwrap();
        manager1.save_to_file(&file_path).unwrap();

        let manager2 = AgentManager::new();
        manager2.load_from_file(&file_path).unwrap();
        let loaded = manager2.inspect(&agent.id).unwrap();
        assert_eq!(loaded.name, "persisted-agent");
        assert_eq!(loaded.state, AgentState::Running);

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_agent_permissions_boundaries() {
        let std_perms = AgentPermissions::standard();
        assert!(std_perms.allows_filesystem_read());
        assert!(std_perms.allows_filesystem_write());
        assert!(std_perms.allows_shell(Some("cargo test")));
        assert!(std_perms.allows_network(Some("https://api.github.com")));
        assert!(std_perms.allows_git("read"));
        assert!(std_perms.allows_git("commit"));
        assert!(!std_perms.allows_git("push")); // push forbidden by default

        let ro_perms = AgentPermissions::read_only();
        assert!(ro_perms.allows_filesystem_read());
        assert!(!ro_perms.allows_filesystem_write());
        assert!(!ro_perms.allows_shell(Some("cargo test")));
        assert!(!ro_perms.allows_network(None));
        assert!(ro_perms.allows_git("read"));
        assert!(!ro_perms.allows_git("commit"));
        assert!(!ro_perms.allows_git("push"));

        let custom_shell = AgentPermissions {
            shell: vec!["cargo".to_string(), "git".to_string()],
            ..Default::default()
        };
        assert!(custom_shell.allows_shell(Some("cargo build")));
        assert!(custom_shell.allows_shell(Some("git status")));
        assert!(!custom_shell.allows_shell(Some("rm -rf /")));
    }
}
