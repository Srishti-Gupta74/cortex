//! In-memory thread-safe registry for managing active Agent Skills.

use super::metadata::Skill;
use crate::tool::ToolRegistry;
use cortex_core::{CortexError, Result};
use std::collections::HashMap;
use std::sync::RwLock;

/// Thread-safe registry for storing, querying, and validating Agent Skills.
#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: RwLock<HashMap<String, Skill>>,
}

impl Clone for SkillRegistry {
    fn clone(&self) -> Self {
        let cloned_map = self.skills.read().map(|s| s.clone()).unwrap_or_default();
        Self {
            skills: RwLock::new(cloned_map),
        }
    }
}

impl SkillRegistry {
    /// Construct a new empty [`SkillRegistry`].
    pub fn new() -> Self {
        Self {
            skills: RwLock::new(HashMap::new()),
        }
    }

    /// Register a new skill in the registry.
    ///
    /// Returns [`CortexError::Validation`] if a skill with the same name is already registered.
    pub fn register(&self, skill: Skill) -> Result<()> {
        skill.metadata.validate()?;
        let name = skill.name().to_string();

        let mut skills = self.skills.write().map_err(|e| {
            CortexError::Internal(format!(
                "failed to acquire skill registry write lock: {}",
                e
            ))
        })?;

        if skills.contains_key(&name) {
            return Err(CortexError::Validation(format!(
                "skill '{}' is already registered",
                name
            )));
        }

        skills.insert(name, skill);
        Ok(())
    }

    /// Register a skill, replacing an existing entry if one exists with the same name.
    ///
    /// Returns the previously registered skill if one was replaced.
    pub fn register_or_replace(&self, skill: Skill) -> Result<Option<Skill>> {
        skill.metadata.validate()?;
        let name = skill.name().to_string();

        let mut skills = self.skills.write().map_err(|e| {
            CortexError::Internal(format!(
                "failed to acquire skill registry write lock: {}",
                e
            ))
        })?;

        Ok(skills.insert(name, skill))
    }

    /// Retrieve a clone of a registered skill by name.
    pub fn get(&self, name: &str) -> Option<Skill> {
        self.skills.read().ok()?.get(name).cloned()
    }

    /// Check if a skill exists with the given name.
    pub fn contains(&self, name: &str) -> bool {
        self.skills
            .read()
            .map(|s| s.contains_key(name))
            .unwrap_or(false)
    }

    /// Remove and return a skill by name if present.
    pub fn remove(&self, name: &str) -> Option<Skill> {
        self.skills.write().ok()?.remove(name)
    }

    /// List all registered skills sorted alphabetically by name.
    pub fn list(&self) -> Vec<Skill> {
        let mut list: Vec<Skill> = self
            .skills
            .read()
            .map(|s| s.values().cloned().collect())
            .unwrap_or_default();
        list.sort_by(|a, b| a.name().cmp(b.name()));
        list
    }

    /// Find skills matching a specific tag (case-insensitive).
    pub fn find_by_tag(&self, tag: &str) -> Vec<Skill> {
        let tag_lower = tag.to_lowercase();
        let mut matches = Vec::new();

        if let Ok(skills) = self.skills.read() {
            for skill in skills.values() {
                if skill.tags().iter().any(|t| t.to_lowercase() == tag_lower) {
                    matches.push(skill.clone());
                }
            }
        }

        matches.sort_by(|a, b| a.name().cmp(b.name()));
        matches
    }

    /// Find skills that declare a specific tool in their allowed tool list.
    pub fn find_by_tool(&self, tool: &str) -> Vec<Skill> {
        let mut matches = Vec::new();

        if let Ok(skills) = self.skills.read() {
            for skill in skills.values() {
                if skill.allowed_tools().iter().any(|t| t == tool) {
                    matches.push(skill.clone());
                }
            }
        }

        matches.sort_by(|a, b| a.name().cmp(b.name()));
        matches
    }

    /// Count of registered skills.
    pub fn len(&self) -> usize {
        self.skills.read().map(|s| s.len()).unwrap_or(0)
    }

    /// Check whether the registry has no registered skills.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear all registered skills from the registry.
    pub fn clear(&self) {
        if let Ok(mut skills) = self.skills.write() {
            skills.clear();
        }
    }

    /// Validate that all declared tools across all registered skills are allowed and available.
    ///
    /// Fails with [`CortexError::PermissionDenied`] if any skill references an unauthorized or missing tool.
    pub fn validate_all_permissions(&self, registry: &ToolRegistry) -> Result<()> {
        if let Ok(skills) = self.skills.read() {
            for skill in skills.values() {
                skill.validate_permissions(registry)?;
            }
        }
        Ok(())
    }

    /// Format all registered skills into a prompt context block for agent inference.
    pub fn format_prompt_context(&self) -> String {
        let list = self.list();
        if list.is_empty() {
            return String::new();
        }

        let mut output = String::from("## Available Agent Skills\n\n");
        for skill in list {
            output.push_str(&skill.to_prompt());
            output.push('\n');
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::metadata::SkillMetadata;
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

    fn sample_skill(name: &str, desc: &str) -> Skill {
        Skill::new(SkillMetadata::new(name, desc), "Instructions").unwrap()
    }

    #[test]
    fn test_register_and_get() {
        let registry = SkillRegistry::new();
        let s = sample_skill("test-skill", "Testing registry");
        registry.register(s.clone()).unwrap();

        assert!(registry.contains("test-skill"));
        let retrieved = registry.get("test-skill").unwrap();
        assert_eq!(retrieved.name(), "test-skill");
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
    }

    #[test]
    fn test_register_duplicate_fails() {
        let registry = SkillRegistry::new();
        let s1 = sample_skill("dup", "First");
        let s2 = sample_skill("dup", "Second");
        registry.register(s1).unwrap();
        let err = registry.register(s2).unwrap_err();
        assert!(matches!(err, CortexError::Validation(_)));
    }

    #[test]
    fn test_register_or_replace() {
        let registry = SkillRegistry::new();
        let s1 = sample_skill("rep", "First");
        let s2 = sample_skill("rep", "Second");

        let prev = registry.register_or_replace(s1).unwrap();
        assert!(prev.is_none());

        let prev = registry.register_or_replace(s2).unwrap();
        assert_eq!(prev.unwrap().description(), "First");
        assert_eq!(registry.get("rep").unwrap().description(), "Second");
    }

    #[test]
    fn test_remove_and_clear() {
        let registry = SkillRegistry::new();
        registry
            .register(sample_skill("skill-a", "Description A"))
            .unwrap();
        registry
            .register(sample_skill("skill-b", "Description B"))
            .unwrap();

        let removed = registry.remove("skill-a").unwrap();
        assert_eq!(removed.name(), "skill-a");
        assert_eq!(registry.len(), 1);

        registry.clear();
        assert!(registry.is_empty());
    }

    #[test]
    fn test_list_sorted() {
        let registry = SkillRegistry::new();
        registry.register(sample_skill("zebra", "Z")).unwrap();
        registry.register(sample_skill("alpha", "A")).unwrap();
        registry.register(sample_skill("mango", "M")).unwrap();

        let names: Vec<String> = registry
            .list()
            .into_iter()
            .map(|s| s.name().to_string())
            .collect();
        assert_eq!(names, vec!["alpha", "mango", "zebra"]);
    }

    #[test]
    fn test_find_by_tag() {
        let registry = SkillRegistry::new();
        let s1 = Skill::new(
            SkillMetadata::new("s1", "d1").with_tags(vec!["rust".into(), "git".into()]),
            "i1",
        )
        .unwrap();
        let s2 = Skill::new(
            SkillMetadata::new("s2", "d2").with_tags(vec!["Python".into(), "git".into()]),
            "i2",
        )
        .unwrap();
        registry.register(s1).unwrap();
        registry.register(s2).unwrap();

        let git_matches = registry.find_by_tag("git");
        assert_eq!(git_matches.len(), 2);

        let py_matches = registry.find_by_tag("python");
        assert_eq!(py_matches.len(), 1);
        assert_eq!(py_matches[0].name(), "s2");
    }

    #[test]
    fn test_find_by_tool() {
        let registry = SkillRegistry::new();
        let s1 = Skill::new(
            SkillMetadata::new("s1", "d1").with_allowed_tools(vec!["read_file".into()]),
            "i1",
        )
        .unwrap();
        let s2 = Skill::new(
            SkillMetadata::new("s2", "d2").with_allowed_tools(vec!["write_file".into()]),
            "i2",
        )
        .unwrap();
        registry.register(s1).unwrap();
        registry.register(s2).unwrap();

        let tool_matches = registry.find_by_tool("read_file");
        assert_eq!(tool_matches.len(), 1);
        assert_eq!(tool_matches[0].name(), "s1");
    }

    #[test]
    fn test_validate_all_permissions() {
        let tool_reg = ToolRegistry::new();
        tool_reg.register_tool(TestTool("read_file")).unwrap();

        let registry = SkillRegistry::new();
        let s1 = Skill::new(
            SkillMetadata::new("s1", "d1").with_allowed_tools(vec!["read_file".into()]),
            "i1",
        )
        .unwrap();
        registry.register(s1).unwrap();
        assert!(registry.validate_all_permissions(&tool_reg).is_ok());

        let s2 = Skill::new(
            SkillMetadata::new("s2", "d2").with_allowed_tools(vec!["missing_tool".into()]),
            "i2",
        )
        .unwrap();
        registry.register(s2).unwrap();
        assert!(registry.validate_all_permissions(&tool_reg).is_err());
    }

    #[test]
    fn test_format_prompt_context() {
        let registry = SkillRegistry::new();
        assert_eq!(registry.format_prompt_context(), "");

        registry
            .register(sample_skill("skill-one", "First skill"))
            .unwrap();
        let prompt = registry.format_prompt_context();
        assert!(prompt.contains("## Available Agent Skills"));
        assert!(prompt.contains("### Skill: skill-one"));
    }
}
