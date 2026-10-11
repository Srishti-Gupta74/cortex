//! Skill metadata structures, validation, and frontmatter parsing.

use crate::tool::ToolRegistry;
use cortex_core::{CortexError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Metadata parsed from the YAML frontmatter of an Agent Skill document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMetadata {
    /// Unique identifier for the skill (e.g. "code-review", "git-workflow").
    pub name: String,

    /// Short explanation of what the skill does and when to apply it.
    pub description: String,

    /// Optional semantic version string (e.g. "0.1.0").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Optional author or maintainer attribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,

    /// Categorization tags for querying and filtering skills.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Explicit list of tool names this skill requires or allows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,

    /// Configuration parameters or user overrides for the skill.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub parameters: HashMap<String, serde_json::Value>,
}

impl SkillMetadata {
    /// Create new skill metadata with required name and description.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            version: None,
            author: None,
            tags: Vec::new(),
            allowed_tools: Vec::new(),
            parameters: HashMap::new(),
        }
    }

    /// Set the optional version.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Set the optional author.
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// Set categorization tags.
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Set allowed tool names.
    pub fn with_allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    /// Set parameter map.
    pub fn with_parameters(mut self, parameters: HashMap<String, serde_json::Value>) -> Self {
        self.parameters = parameters;
        self
    }

    /// Validate name format and required fields.
    pub fn validate(&self) -> Result<()> {
        let trimmed_name = self.name.trim();
        if trimmed_name.is_empty() {
            return Err(CortexError::Validation(
                "skill metadata 'name' cannot be empty".to_string(),
            ));
        }

        if !trimmed_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(CortexError::Validation(format!(
                "skill name '{}' contains invalid characters; must be alphanumeric, '-', '_', or '.'",
                trimmed_name
            )));
        }

        if self.description.trim().is_empty() {
            return Err(CortexError::Validation(format!(
                "skill '{}' description cannot be empty",
                trimmed_name
            )));
        }

        Ok(())
    }
}

/// In-memory representation of an Agent Skill document (`SKILL.md`).
///
/// Contains YAML frontmatter metadata and the subsequent markdown instructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// Skill metadata extracted from frontmatter.
    pub metadata: SkillMetadata,

    /// Markdown instructions and execution guidance.
    pub instructions: String,

    /// Filesystem path from which the skill was loaded, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl Skill {
    /// Create a new skill from metadata and markdown instructions.
    pub fn new(metadata: SkillMetadata, instructions: impl Into<String>) -> Result<Self> {
        metadata.validate()?;
        Ok(Self {
            metadata,
            instructions: instructions.into(),
            path: None,
        })
    }

    /// Parse a skill document from a string containing YAML frontmatter and markdown body.
    pub fn parse_str(content: &str) -> Result<Self> {
        let (frontmatter, instructions) = split_frontmatter(content)?;
        let metadata: SkillMetadata = serde_yaml::from_str(frontmatter).map_err(|e| {
            CortexError::Validation(format!("failed to parse skill YAML frontmatter: {}", e))
        })?;

        metadata.validate()?;

        Ok(Self {
            metadata,
            instructions: instructions.to_string(),
            path: None,
        })
    }

    /// Load and parse a skill document directly from a filesystem path.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path_ref = path.as_ref();
        let content = std::fs::read_to_string(path_ref).map_err(|e| {
            CortexError::NotFound(format!(
                "failed to read skill file at '{}': {}",
                path_ref.display(),
                e
            ))
        })?;

        let mut skill = Self::parse_str(&content)?;
        skill.path = Some(path_ref.to_path_buf());
        Ok(skill)
    }

    /// Name identifier of the skill.
    pub fn name(&self) -> &str {
        &self.metadata.name
    }

    /// Short description of the skill.
    pub fn description(&self) -> &str {
        &self.metadata.description
    }

    /// Instructions body containing markdown workflow guidance.
    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// List of tool names allowed or required by this skill.
    pub fn allowed_tools(&self) -> &[String] {
        &self.metadata.allowed_tools
    }

    /// Categorization tags for this skill.
    pub fn tags(&self) -> &[String] {
        &self.metadata.tags
    }

    /// Optional filesystem path where this skill document resides.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Configuration parameters for this skill.
    pub fn parameters(&self) -> &HashMap<String, serde_json::Value> {
        &self.metadata.parameters
    }

    /// Format this skill into a prompt section for agent execution context.
    pub fn to_prompt(&self) -> String {
        let mut out = format!("### Skill: {}\n{}\n\n", self.name(), self.description());
        if !self.allowed_tools().is_empty() {
            out.push_str(&format!(
                "Allowed Tools: {}\n\n",
                self.allowed_tools().join(", ")
            ));
        }

        let trimmed = self.instructions.trim();
        if !trimmed.is_empty() {
            out.push_str("#### Instructions\n");
            out.push_str(trimmed);
            out.push('\n');
        }

        out
    }

    /// Validate that all declared tools exist in the provided registry.
    ///
    /// Skills cannot invoke or declare tools that are not permitted by runtime policies.
    pub fn validate_permissions(&self, registry: &ToolRegistry) -> Result<()> {
        for tool_name in &self.metadata.allowed_tools {
            if !registry.contains(tool_name) {
                return Err(CortexError::PermissionDenied(format!(
                    "skill '{}' declares unpermitted or missing tool '{}'",
                    self.name(),
                    tool_name
                )));
            }
        }
        Ok(())
    }
}

/// Split YAML frontmatter and Markdown body.
///
/// Expects opening delimiter `---` on the first non-empty line and matching closing `---` or `...`.
fn split_frontmatter(content: &str) -> Result<(&str, &str)> {
    // Strip UTF-8 BOM if present
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);

    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Err(CortexError::Validation(
            "skill document must begin with '---' YAML frontmatter delimiter".to_string(),
        ));
    }

    // Locate the first newline after opening delimiter
    let after_open = match trimmed[3..].find('\n') {
        Some(idx) => &trimmed[3 + idx + 1..],
        None => {
            return Err(CortexError::Validation(
                "skill document has incomplete frontmatter header".to_string(),
            ))
        }
    };

    // Locate the closing delimiter on its own line: either "\n---" or "\n..."
    let mut search_idx = 0;
    while let Some(rel_idx) = after_open[search_idx..].find('\n') {
        let candidate_start = search_idx + rel_idx + 1;
        let line_rest = &after_open[candidate_start..];
        let trimmed_line = line_rest.trim_start();

        if let Some(after_delim) = trimmed_line
            .strip_prefix("---")
            .or_else(|| trimmed_line.strip_prefix("..."))
        {
            // Delimiter line must end with newline or be at end of string
            let after_delim_trimmed = after_delim.trim_start_matches([' ', '\t', '\r']);
            if after_delim_trimmed.is_empty() || after_delim_trimmed.starts_with('\n') {
                let frontmatter = &after_open[..search_idx + rel_idx];
                let instructions = if let Some(body_start) = after_delim_trimmed.find('\n') {
                    &after_delim_trimmed[body_start + 1..]
                } else {
                    ""
                };
                return Ok((frontmatter, instructions));
            }
        }

        search_idx = candidate_start;
    }

    Err(CortexError::Validation(
        "skill document missing closing '---' or '...' frontmatter delimiter".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Tool, ToolDefinition, ToolResult};

    struct TestTool(&'static str);
    impl Tool for TestTool {
        fn definition(&self) -> &ToolDefinition {
            let boxed = Box::new(ToolDefinition::new(
                self.0,
                "test tool",
                serde_json::json!({ "type": "object" }),
            ));
            Box::leak(boxed)
        }
        fn execute(&self, _input: &serde_json::Value) -> Result<ToolResult> {
            Ok(ToolResult::success("ok"))
        }
    }

    #[test]
    fn test_parse_valid_skill() {
        let doc = r#"---
name: code-review
description: Review pull request diffs for correctness and style
version: 1.2.0
author: Cortex Team
tags:
  - review
  - quality
allowed_tools:
  - read_file
  - git_diff
parameters:
  strict: true
---
# Code Review Instructions

Examine the diff carefully before approving.
"#;
        let skill = Skill::parse_str(doc).expect("should parse valid skill");
        assert_eq!(skill.name(), "code-review");
        assert_eq!(
            skill.description(),
            "Review pull request diffs for correctness and style"
        );
        assert_eq!(skill.metadata.version.as_deref(), Some("1.2.0"));
        assert_eq!(skill.metadata.author.as_deref(), Some("Cortex Team"));
        assert_eq!(skill.tags(), &["review", "quality"]);
        assert_eq!(skill.allowed_tools(), &["read_file", "git_diff"]);
        assert_eq!(
            skill.parameters().get("strict"),
            Some(&serde_json::json!(true))
        );
        assert!(skill.instructions().contains("# Code Review Instructions"));
    }

    #[test]
    fn test_parse_minimal_skill() {
        let doc = "---\nname: minimal\ndescription: A minimal skill\n---\nDo this.";
        let skill = Skill::parse_str(doc).expect("minimal skill should parse");
        assert_eq!(skill.name(), "minimal");
        assert_eq!(skill.description(), "A minimal skill");
        assert!(skill.tags().is_empty());
        assert!(skill.allowed_tools().is_empty());
        assert_eq!(skill.instructions().trim(), "Do this.");
    }

    #[test]
    fn test_parse_with_dots_delimiter() {
        let doc = "---\nname: dots\ndescription: Skill with dots delimiter\n...\nInstructions here";
        let skill = Skill::parse_str(doc).expect("skill with ... delimiter should parse");
        assert_eq!(skill.name(), "dots");
        assert_eq!(skill.instructions().trim(), "Instructions here");
    }

    #[test]
    fn test_parse_utf8_bom() {
        let doc = "\u{feff}---\nname: bom-skill\ndescription: Has BOM\n---\nHello";
        let skill = Skill::parse_str(doc).expect("should handle UTF-8 BOM");
        assert_eq!(skill.name(), "bom-skill");
    }

    #[test]
    fn test_parse_missing_opening_delimiter() {
        let doc = "name: bad\ndescription: missing header\n---\nContent";
        let err = Skill::parse_str(doc).unwrap_err();
        assert!(matches!(err, CortexError::Validation(_)));
    }

    #[test]
    fn test_parse_missing_closing_delimiter() {
        let doc = "---\nname: unclosed\ndescription: missing closing delimiter\n";
        let err = Skill::parse_str(doc).unwrap_err();
        assert!(matches!(err, CortexError::Validation(_)));
    }

    #[test]
    fn test_parse_malformed_yaml() {
        let doc = "---\nname: bad_yaml\n: invalid\n---\nBody";
        let err = Skill::parse_str(doc).unwrap_err();
        assert!(matches!(err, CortexError::Validation(_)));
    }

    #[test]
    fn test_validation_empty_name() {
        let doc = "---\nname: '  '\ndescription: Some desc\n---\nBody";
        let err = Skill::parse_str(doc).unwrap_err();
        assert!(matches!(err, CortexError::Validation(_)));
    }

    #[test]
    fn test_validation_invalid_name_characters() {
        let doc = "---\nname: 'bad name with spaces'\ndescription: Some desc\n---\nBody";
        let err = Skill::parse_str(doc).unwrap_err();
        assert!(matches!(err, CortexError::Validation(_)));
    }

    #[test]
    fn test_validation_empty_description() {
        let doc = "---\nname: valid-name\ndescription: ''\n---\nBody";
        let err = Skill::parse_str(doc).unwrap_err();
        assert!(matches!(err, CortexError::Validation(_)));
    }

    #[test]
    fn test_to_prompt() {
        let doc = r#"---
name: test-prompt
description: Formats into prompt
allowed_tools:
  - tool_a
  - tool_b
---
Follow rule 1 and rule 2.
"#;
        let skill = Skill::parse_str(doc).unwrap();
        let prompt = skill.to_prompt();
        assert!(prompt.contains("### Skill: test-prompt"));
        assert!(prompt.contains("Allowed Tools: tool_a, tool_b"));
        assert!(prompt.contains("Follow rule 1 and rule 2."));
    }

    #[test]
    fn test_validate_permissions() {
        let registry = ToolRegistry::new();
        registry.register_tool(TestTool("allowed_tool")).unwrap();

        let valid_doc =
            "---\nname: s1\ndescription: valid\nallowed_tools:\n  - allowed_tool\n---\n";
        let s1 = Skill::parse_str(valid_doc).unwrap();
        assert!(s1.validate_permissions(&registry).is_ok());

        let invalid_doc =
            "---\nname: s2\ndescription: invalid\nallowed_tools:\n  - forbidden_tool\n---\n";
        let s2 = Skill::parse_str(invalid_doc).unwrap();
        let err = s2.validate_permissions(&registry).unwrap_err();
        assert!(matches!(err, CortexError::PermissionDenied(_)));
    }
}
