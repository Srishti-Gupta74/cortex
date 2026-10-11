//! Declarative YAML manifest schema and parser for Cortex agents.
//!
//! Provides serde-based serialization and deserialization for `agent.yaml` files,
//! including schema validation, descriptive error reporting, and conversion to [`AgentManifest`].

use super::manager::{AgentManifest, AgentModelConfig, AgentPermissions};
use cortex_core::{AgentId, CortexError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Permissive representation of permission settings in YAML manifests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum YamlPermissionValue {
    /// Boolean flag (e.g., `shell: false`).
    Boolean(bool),
    /// String shorthand (e.g., `filesystem: "workspace_only"`).
    String(String),
    /// Explicit list of allowed operations or patterns.
    List(Vec<String>),
    /// Key-value map of capability options (e.g., `git: { read: true, push: false }`).
    Map(std::collections::HashMap<String, YamlPermissionValue>),
}

/// Declarative permissions block for `agent.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AgentYamlPermissions {
    /// Filesystem permission (e.g., `"workspace_only"`, boolean, or list of paths).
    #[serde(default)]
    pub filesystem: Option<YamlPermissionValue>,
    /// Shell execution permission (e.g., `false`, `true`, or list of commands).
    #[serde(default)]
    pub shell: Option<YamlPermissionValue>,
    /// Git push authorization flag.
    #[serde(default)]
    pub git_push: Option<bool>,
    /// Git capability permission (e.g., boolean, list, or map `{ read: true, push: false }`).
    #[serde(default)]
    pub git: Option<YamlPermissionValue>,
    /// Network destination permission.
    #[serde(default)]
    pub network: Option<YamlPermissionValue>,
    /// Interactive approval mode for high-risk operations ("prompt", "deny", "allow").
    #[serde(default, alias = "approval_policy")]
    pub approval: Option<cortex_core::permissions::ApprovalMode>,
    /// Explicit approval mode field.
    #[serde(default)]
    pub approval_mode: Option<cortex_core::permissions::ApprovalMode>,
}

impl AgentYamlPermissions {
    /// Convert declarative YAML permissions into runtime [`AgentPermissions`].
    pub fn to_runtime_permissions(&self) -> AgentPermissions {
        let filesystem = match &self.filesystem {
            Some(YamlPermissionValue::Boolean(true)) => {
                vec!["read".to_string(), "write".to_string()]
            }
            Some(YamlPermissionValue::Boolean(false)) => Vec::new(),
            Some(YamlPermissionValue::String(s)) => {
                if s == "workspace_only" || s == "workspace" {
                    vec![
                        "workspace_only".to_string(),
                        "read".to_string(),
                        "write".to_string(),
                    ]
                } else if s == "read_only" {
                    vec!["read".to_string()]
                } else {
                    vec![s.clone()]
                }
            }
            Some(YamlPermissionValue::List(list)) => list.clone(),
            Some(YamlPermissionValue::Map(map)) => {
                let mut ops = Vec::new();
                for (k, v) in map {
                    let enabled = match v {
                        YamlPermissionValue::Boolean(b) => *b,
                        YamlPermissionValue::String(s) => s != "false" && s != "deny",
                        _ => true,
                    };
                    if enabled {
                        ops.push(k.clone());
                    }
                }
                ops
            }
            None => vec!["read".to_string(), "write".to_string()],
        };

        let shell = match &self.shell {
            Some(YamlPermissionValue::Boolean(true)) => vec!["*".to_string()],
            Some(YamlPermissionValue::Boolean(false)) => Vec::new(),
            Some(YamlPermissionValue::String(s)) => {
                if s == "none" || s == "false" || s == "deny" {
                    Vec::new()
                } else {
                    vec![s.clone()]
                }
            }
            Some(YamlPermissionValue::List(list)) => list.clone(),
            Some(YamlPermissionValue::Map(_)) => Vec::new(),
            None => Vec::new(),
        };

        let network = match &self.network {
            Some(YamlPermissionValue::Boolean(true)) => vec!["allow".to_string()],
            Some(YamlPermissionValue::Boolean(false)) => Vec::new(),
            Some(YamlPermissionValue::String(s)) => {
                if s == "none" || s == "false" || s == "deny" {
                    Vec::new()
                } else {
                    vec![s.clone()]
                }
            }
            Some(YamlPermissionValue::List(list)) => list.clone(),
            Some(YamlPermissionValue::Map(_)) => Vec::new(),
            None => Vec::new(),
        };

        let mut git = match &self.git {
            Some(YamlPermissionValue::Boolean(true)) => {
                vec!["read".to_string(), "commit".to_string()]
            }
            Some(YamlPermissionValue::Boolean(false)) => Vec::new(),
            Some(YamlPermissionValue::String(s)) => {
                if s == "read_only" {
                    vec!["read".to_string()]
                } else if s == "none" || s == "false" || s == "deny" {
                    Vec::new()
                } else {
                    vec![s.clone()]
                }
            }
            Some(YamlPermissionValue::List(list)) => list.clone(),
            Some(YamlPermissionValue::Map(map)) => {
                let mut ops = Vec::new();
                for (k, v) in map {
                    let enabled = match v {
                        YamlPermissionValue::Boolean(b) => *b,
                        YamlPermissionValue::String(s) => s != "false" && s != "deny",
                        _ => true,
                    };
                    if enabled {
                        ops.push(k.clone());
                    }
                }
                ops
            }
            None => vec!["read".to_string(), "commit".to_string()],
        };

        if let Some(false) = self.git_push {
            git.retain(|op| op != "push" && op != "*");
        } else if let Some(true) = self.git_push {
            if !git.contains(&"push".to_string()) {
                git.push("push".to_string());
            }
        }

        let approval_mode = self.approval_mode.or(self.approval).unwrap_or_default();

        AgentPermissions {
            filesystem,
            network,
            shell,
            git,
            approval_mode,
        }
    }
}

/// Model specification within a YAML manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentYamlModel {
    /// Simple model identifier string (e.g. `"claude-3-5-sonnet-20241022"` or `"gpt-4o"`).
    Simple(String),
    /// Detailed model configuration object with provider, temperature, etc.
    Detailed(AgentModelConfig),
}

impl AgentYamlModel {
    /// Convert to runtime [`AgentModelConfig`], inferring provider if necessary.
    pub fn to_model_config(&self) -> AgentModelConfig {
        match self {
            AgentYamlModel::Simple(name) => {
                let lower = name.to_lowercase();
                let provider = if lower.contains("claude") {
                    "anthropic"
                } else if lower.contains("gpt") || lower.contains("o1") || lower.contains("o3") {
                    "openai"
                } else if lower.contains("gemini") {
                    "google"
                } else if lower.contains("mock") {
                    "mock"
                } else {
                    "default"
                };
                AgentModelConfig::new(provider, name.as_str())
            }
            AgentYamlModel::Detailed(config) => config.clone(),
        }
    }

    /// Return the model name string.
    pub fn model_name(&self) -> &str {
        match self {
            AgentYamlModel::Simple(name) => name.as_str(),
            AgentYamlModel::Detailed(config) => config.model.as_str(),
        }
    }
}

/// Declarative YAML agent manifest representing `agent.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentYamlManifest {
    /// Optional explicitly assigned agent ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<AgentId>,
    /// Unique human-readable agent name.
    pub name: String,
    /// Model provider or identifier.
    pub model: AgentYamlModel,
    /// Workspace root directory.
    pub workspace: PathBuf,
    /// Operational policy instructions (synonymous with system prompt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    /// Agent domain role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Explicit system prompt instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Configured tool capabilities.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Granted permission boundaries.
    #[serde(default)]
    pub permissions: AgentYamlPermissions,
    /// Interactive approval mode override for high-risk operations ("prompt", "deny", "allow").
    #[serde(default, alias = "approval")]
    pub approval_mode: Option<cortex_core::permissions::ApprovalMode>,
    /// Whether to automatically resume on daemon restart if interrupted.
    #[serde(default)]
    pub auto_resume: bool,
}

impl AgentYamlManifest {
    /// Parse and validate an `AgentYamlManifest` from a YAML string.
    pub fn from_yaml_str(yaml_str: &str) -> Result<Self> {
        let manifest: Self = serde_yaml::from_str(yaml_str).map_err(|e| {
            CortexError::Validation(format!("failed to parse YAML manifest: {}", e))
        })?;

        manifest.validate()?;
        Ok(manifest)
    }

    /// Parse and validate an `AgentYamlManifest` from a file path.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
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

    /// Validate the manifest schema and fields, returning descriptive error messages.
    pub fn validate(&self) -> Result<()> {
        let trimmed_name = self.name.trim();
        if trimmed_name.is_empty() {
            return Err(CortexError::Validation(
                "YAML manifest validation error: 'name' is required and cannot be empty"
                    .to_string(),
            ));
        }

        // Validate name format: alphanumeric, hyphens, and underscores only
        if !trimmed_name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Err(CortexError::Validation(format!(
                "YAML manifest validation error: 'name' ('{}') contains invalid characters; only alphanumeric, hyphens, and underscores are permitted",
                trimmed_name
            )));
        }

        let model_name = self.model.model_name().trim();
        if model_name.is_empty() {
            return Err(CortexError::Validation(
                "YAML manifest validation error: 'model' is required and cannot be empty"
                    .to_string(),
            ));
        }

        if self.workspace.as_os_str().is_empty() {
            return Err(CortexError::Validation(
                "YAML manifest validation error: 'workspace' path is required and cannot be empty"
                    .to_string(),
            ));
        }

        let has_policy = self
            .policy
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let has_role = self
            .role
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let has_prompt = self
            .system_prompt
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);

        if !has_policy && !has_role && !has_prompt {
            return Err(CortexError::Validation(
                "YAML manifest validation error: at least one of 'policy', 'role', or 'system_prompt' must be provided".to_string(),
            ));
        }

        Ok(())
    }

    /// Convert this declarative YAML manifest into a runtime [`AgentManifest`].
    pub fn into_manifest(self) -> Result<AgentManifest> {
        self.validate()?;

        let role = self
            .role
            .or_else(|| self.policy.clone())
            .unwrap_or_else(|| self.name.clone());

        let system_prompt = self.system_prompt.or(self.policy);

        let mut manifest = AgentManifest::new(
            self.name,
            role,
            self.workspace,
            self.model.to_model_config(),
        );

        if let Some(id) = self.id {
            manifest = manifest.with_id(id);
        }

        if let Some(prompt) = system_prompt {
            manifest = manifest.with_system_prompt(prompt);
        }

        let mut runtime_perms = self.permissions.to_runtime_permissions();
        if let Some(mode) = self.approval_mode {
            runtime_perms.approval_mode = mode;
        }
        let mut tools = self.tools;
        // If git_push is not permitted by capability permissions, strip git_push from tools
        if !runtime_perms.allows_git("push") {
            tools.retain(|t| t != "git_push");
        }

        manifest = manifest
            .with_tools(tools)
            .with_permissions(runtime_perms)
            .with_auto_resume(self.auto_resume);

        Ok(manifest)
    }

    /// Serialize this YAML manifest to a pretty-printed YAML string.
    pub fn to_yaml_string(&self) -> Result<String> {
        serde_yaml::to_string(self)
            .map_err(|e| CortexError::Internal(format!("failed to serialize YAML manifest: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_yaml_permissions_parsing_map_and_booleans() {
        let yaml = r#"
name: test_agent
role: Tester
workspace: /tmp
model: mock-gpt
permissions:
  filesystem: workspace
  shell: false
  network: false
  git:
    read: true
    commit: true
    push: false
"#;
        let manifest = AgentYamlManifest::from_yaml_str(yaml).unwrap();
        let runtime_manifest = manifest.into_manifest().unwrap();
        let perms = runtime_manifest.permissions;

        assert!(perms.allows_filesystem_read());
        assert!(perms.allows_filesystem_write());
        assert!(!perms.allows_shell(Some("cargo test")));
        assert!(!perms.allows_network(None));
        assert!(perms.allows_git("read"));
        assert!(perms.allows_git("commit"));
        assert!(!perms.allows_git("push"));
    }
}
