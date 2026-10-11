//! User settings and global configuration management (`~/.cortex/settings.json`).

use crate::{CortexError, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

fn default_model() -> String {
    "gpt-4o-mini".to_string()
}

fn default_max_iterations() -> usize {
    25
}

fn default_true() -> bool {
    true
}

fn default_approval_mode() -> crate::permissions::ApprovalMode {
    crate::permissions::ApprovalMode::Prompt
}

/// Primary user settings loaded from `~/.cortex/settings.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserSettings {
    /// Schema reference URL for editor validation.
    #[serde(default, rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,

    /// Human-readable explanation comment.
    #[serde(default, rename = "_comment", skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,

    /// Default model identifier (e.g. "gpt-4o-mini", "claude-3-5-sonnet-20241022", "ollama/llama3.1").
    #[serde(default = "default_model")]
    pub model: String,

    /// General API key for model inference providers.
    #[serde(default, alias = "key")]
    pub api_key: Option<String>,

    /// Custom base URL for model inference endpoints (e.g. `http://localhost:11434/v1` or `https://openrouter.ai/api/v1`).
    #[serde(default, alias = "url", alias = "api_base")]
    pub base_url: Option<String>,

    /// Optional provider identifier override (e.g. "openai", "anthropic", "ollama").
    #[serde(default)]
    pub provider: Option<String>,

    /// Dedicated OpenAI API key override.
    #[serde(default)]
    pub openai_api_key: Option<String>,

    /// Dedicated Anthropic API key override.
    #[serde(default)]
    pub anthropic_api_key: Option<String>,

    /// Dedicated Ollama base URL override.
    #[serde(default)]
    pub ollama_base_url: Option<String>,

    /// Sampling temperature for model completions.
    #[serde(default)]
    pub temperature: Option<f64>,

    /// Maximum autonomous tool-execution iterations before stopping.
    #[serde(
        default = "default_max_iterations",
        alias = "max_steps",
        alias = "max_iter"
    )]
    pub max_iterations: usize,

    /// Default system prompt prepended to agent instructions.
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Active UI theme (e.g. "blue", "codex", "gemini"). Defaults to "blue".
    #[serde(default)]
    pub theme: Option<String>,

    /// Whether to automatically persist chat sessions to `~/.cortex/sessions/`.
    #[serde(default = "default_true")]
    pub auto_save_sessions: bool,

    /// Interactive approval mode for high-risk operations ("prompt", "deny", "allow"). Defaults to "prompt".
    #[serde(
        default = "default_approval_mode",
        alias = "approval",
        alias = "approval_policy"
    )]
    pub approval_mode: crate::permissions::ApprovalMode,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            schema: Some(
                "https://raw.githubusercontent.com/cortex-ai/cortex/main/schemas/settings.json"
                    .to_string(),
            ),
            comment: Some(
                "Cortex settings: configure model, api_key, base_url, or provider keys below"
                    .to_string(),
            ),
            model: default_model(),
            api_key: None,
            base_url: None,
            provider: None,
            openai_api_key: None,
            anthropic_api_key: None,
            ollama_base_url: None,
            temperature: None,
            max_iterations: default_max_iterations(),
            system_prompt: None,
            theme: Some("blue".to_string()),
            auto_save_sessions: true,
            approval_mode: crate::permissions::ApprovalMode::Prompt,
        }
    }
}

/// Resolve the canonical Cortex home directory (`~/.cortex` or `$CORTEX_HOME`).
pub fn cortex_home_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CORTEX_HOME") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join(".cortex");
        }
    }
    PathBuf::from(".cortex")
}

/// Resolve the path to the user settings file (`~/.cortex/settings.json`).
pub fn settings_path() -> PathBuf {
    cortex_home_dir().join("settings.json")
}

/// Ensure the Cortex home directory exists on disk.
pub fn ensure_cortex_home() -> Result<PathBuf> {
    let dir = cortex_home_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| {
            CortexError::Internal(format!(
                "failed to create Cortex home directory '{}': {}",
                dir.display(),
                e
            ))
        })?;
    }
    Ok(dir)
}

/// Generate default pretty-printed JSON contents for `settings.json`.
pub fn default_settings_json() -> String {
    let default_settings = UserSettings::default();
    serde_json::to_string_pretty(&default_settings).unwrap_or_else(|_| {
        r#"{
  "model": "gpt-4o-mini",
  "api_key": null,
  "base_url": null,
  "provider": null,
  "openai_api_key": null,
  "anthropic_api_key": null,
  "ollama_base_url": null,
  "temperature": null,
  "max_iterations": 25,
  "theme": "gemini",
  "auto_save_sessions": true
}
"#
        .to_string()
    })
}

/// Ensure `settings.json` exists in `~/.cortex`. Returns path and whether a new file was created.
pub fn ensure_settings_file() -> Result<(PathBuf, bool)> {
    let dir = ensure_cortex_home()?;
    let path = dir.join("settings.json");
    if path.is_file() {
        Ok((path, false))
    } else {
        let content = default_settings_json();
        fs::write(&path, content).map_err(|e| {
            CortexError::Internal(format!(
                "failed to write default settings to '{}': {}",
                path.display(),
                e
            ))
        })?;
        Ok((path, true))
    }
}

impl UserSettings {
    /// Load user settings from `~/.cortex/settings.json`, creating both directory and file if absent.
    pub fn load_or_create() -> Result<Self> {
        let (path, created) = ensure_settings_file()?;
        if created {
            return Ok(Self::default());
        }
        Self::from_file(&path)
    }

    /// Load user settings or fall back silently to default settings on error.
    pub fn load_or_default() -> Self {
        Self::load_or_create().unwrap_or_else(|e| {
            tracing::warn!("Failed to load settings.json, using defaults: {}", e);
            Self::default()
        })
    }

    /// Load user settings from an explicit file path.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let p = path.as_ref();
        let content = fs::read_to_string(p).map_err(|e| {
            CortexError::Internal(format!(
                "failed to read settings file '{}': {}",
                p.display(),
                e
            ))
        })?;

        serde_json::from_str::<Self>(&content).map_err(|e| {
            CortexError::Validation(format!(
                "invalid JSON in settings file '{}': {}",
                p.display(),
                e
            ))
        })
    }

    /// Save current settings to `~/.cortex/settings.json`.
    pub fn save(&self) -> Result<()> {
        let dir = ensure_cortex_home()?;
        let path = dir.join("settings.json");
        self.save_to(&path)
    }

    /// Save current settings to the specified path.
    pub fn save_to<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let p = path.as_ref();
        if let Some(parent) = p.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    CortexError::Internal(format!(
                        "failed to create settings parent directory '{}': {}",
                        parent.display(),
                        e
                    ))
                })?;
            }
        }

        let serialized = serde_json::to_string_pretty(self).map_err(|e| {
            CortexError::Internal(format!("failed to serialize user settings: {}", e))
        })?;

        fs::write(p, serialized).map_err(|e| {
            CortexError::Internal(format!(
                "failed to write settings file '{}': {}",
                p.display(),
                e
            ))
        })?;

        Ok(())
    }

    /// Resolve the effective API key for a specified model identifier.
    pub fn resolve_api_key(&self, model: &str) -> Option<String> {
        let lower = model.to_lowercase();
        if lower.starts_with("claude") {
            self.anthropic_api_key
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| self.api_key.clone().filter(|s| !s.trim().is_empty()))
        } else if lower.starts_with("ollama/") {
            None
        } else {
            self.openai_api_key
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| self.api_key.clone().filter(|s| !s.trim().is_empty()))
        }
    }

    /// Resolve the effective base URL for a specified model identifier.
    pub fn resolve_base_url(&self, model: &str) -> Option<String> {
        let lower = model.to_lowercase();
        if lower.starts_with("ollama/") {
            self.ollama_base_url
                .clone()
                .filter(|s| !s.trim().is_empty())
        } else if let Some(url) = &self.base_url {
            let u = url.trim();
            if u.is_empty() {
                return None;
            }
            if u.contains("generativelanguage.googleapis.com") {
                if lower.contains("gemini")
                    || lower.contains("gemma")
                    || self.model.eq_ignore_ascii_case(model)
                {
                    Some(u.to_string())
                } else {
                    None
                }
            } else if u.contains("deepseek") {
                if lower.contains("deepseek") || self.model.eq_ignore_ascii_case(model) {
                    Some(u.to_string())
                } else {
                    None
                }
            } else if self.model.eq_ignore_ascii_case(model) {
                Some(u.to_string())
            } else if lower.starts_with("gpt-")
                || lower.starts_with("o1")
                || lower.starts_with("o3")
                || lower.starts_with("claude")
            {
                None
            } else {
                Some(u.to_string())
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let settings = UserSettings::default();
        assert_eq!(settings.model, "gpt-4o-mini");
        assert_eq!(settings.max_iterations, 25);
        assert!(settings.auto_save_sessions);
        assert!(settings.api_key.is_none());
        assert!(settings.base_url.is_none());
    }

    #[test]
    fn test_deserialize_with_aliases() {
        let json = r#"{
            "model": "claude-3-5-sonnet",
            "key": "sk-ant-test123",
            "url": "https://api.anthropic.com/v1",
            "max_steps": 40
        }"#;

        let settings: UserSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.model, "claude-3-5-sonnet");
        assert_eq!(settings.api_key, Some("sk-ant-test123".to_string()));
        assert_eq!(
            settings.base_url,
            Some("https://api.anthropic.com/v1".to_string())
        );
        assert_eq!(settings.max_iterations, 40);
        assert!(settings.auto_save_sessions);
    }

    #[test]
    fn test_resolve_keys_and_urls() {
        let settings = UserSettings {
            openai_api_key: Some("sk-openai".to_string()),
            anthropic_api_key: Some("sk-anthropic".to_string()),
            ollama_base_url: Some("http://localhost:11434/v1".to_string()),
            ..Default::default()
        };

        assert_eq!(
            settings.resolve_api_key("gpt-4o"),
            Some("sk-openai".to_string())
        );
        assert_eq!(
            settings.resolve_api_key("claude-3-5-sonnet"),
            Some("sk-anthropic".to_string())
        );
        assert_eq!(settings.resolve_api_key("ollama/llama3.1"), None);

        assert_eq!(
            settings.resolve_base_url("ollama/llama3.1"),
            Some("http://localhost:11434/v1".to_string())
        );
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let temp_dir =
            std::env::temp_dir().join(format!("cortex_settings_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        let file_path = temp_dir.join("settings.json");

        let settings = UserSettings {
            model: "deepseek-coder".to_string(),
            api_key: Some("sk-secret".to_string()),
            base_url: Some("https://api.deepseek.com/v1".to_string()),
            max_iterations: 30,
            ..Default::default()
        };

        settings.save_to(&file_path).unwrap();

        let loaded = UserSettings::from_file(&file_path).unwrap();
        assert_eq!(loaded.model, "deepseek-coder");
        assert_eq!(loaded.api_key, Some("sk-secret".to_string()));
        assert_eq!(
            loaded.base_url,
            Some("https://api.deepseek.com/v1".to_string())
        );
        assert_eq!(loaded.max_iterations, 30);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_approval_mode_settings() {
        use crate::permissions::ApprovalMode;

        let json_prompt = r#"{"model": "gpt-4o", "approval_mode": "prompt"}"#;
        let s1: UserSettings = serde_json::from_str(json_prompt).unwrap();
        assert_eq!(s1.approval_mode, ApprovalMode::Prompt);

        let json_deny = r#"{"model": "gpt-4o", "approval": "deny"}"#;
        let s2: UserSettings = serde_json::from_str(json_deny).unwrap();
        assert_eq!(s2.approval_mode, ApprovalMode::Deny);

        let json_allow = r#"{"model": "gpt-4o", "approval_policy": "allow"}"#;
        let s3: UserSettings = serde_json::from_str(json_allow).unwrap();
        assert_eq!(s3.approval_mode, ApprovalMode::Allow);

        let json_default = r#"{"model": "gpt-4o"}"#;
        let s4: UserSettings = serde_json::from_str(json_default).unwrap();
        assert_eq!(s4.approval_mode, ApprovalMode::Prompt);
    }
}
