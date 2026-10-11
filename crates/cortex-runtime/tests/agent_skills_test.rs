//! Integration test suite for Agent Skills loader, YAML frontmatter parser, and runtime registry.

use cortex_core::CortexError;
use cortex_runtime::skill::{Skill, SkillLoader, SkillMetadata, SkillRegistry};
use cortex_runtime::tool::{Tool, ToolDefinition, ToolRegistry, ToolResult};
use std::fs;
use std::path::PathBuf;

struct MockTool {
    name: &'static str,
}

impl MockTool {
    fn new(name: &'static str) -> Self {
        Self { name }
    }
}

impl Tool for MockTool {
    fn definition(&self) -> &ToolDefinition {
        let boxed = Box::new(ToolDefinition::new(
            self.name,
            "mock tool description",
            serde_json::json!({ "type": "object" }),
        ));
        Box::leak(boxed)
    }

    fn execute(&self, _input: &serde_json::Value) -> cortex_core::Result<ToolResult> {
        Ok(ToolResult::success(format!("executed {}", self.name)))
    }
}

fn create_temp_test_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cortex_skill_integ_{}_{}",
        prefix,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("failed to create temp test directory");
    dir
}

#[test]
fn test_skill_lifecycle_and_discovery() {
    let base_dir = create_temp_test_dir("lifecycle");

    // Create a mock skills structure:
    // base_dir/
    //   git-assistant/
    //     SKILL.md
    //   code-auditor/
    //     SKILL.md
    //   standalone.skill.md
    //   corrupt/
    //     SKILL.md

    let git_dir = base_dir.join("git-assistant");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(
        git_dir.join("SKILL.md"),
        r#"---
name: git-assistant
description: Automates common git branch and commit operations
version: 0.2.1
author: Engineering Team
tags:
  - git
  - vcs
  - workflow
allowed_tools:
  - git_status
  - git_commit
parameters:
  signoff: true
  default_branch: main
---
# Git Assistant Guidelines

1. Always check git status before staging files.
2. Commit with descriptive semantic messages.
"#,
    )
    .unwrap();

    let audit_dir = base_dir.join("code-auditor");
    fs::create_dir_all(&audit_dir).unwrap();
    fs::write(
        audit_dir.join("SKILL.md"),
        r#"---
name: code-auditor
description: Inspects code for potential memory leaks and vulnerabilities
version: 1.0.0
tags:
  - security
  - audit
allowed_tools:
  - read_file
---
# Security Audit Checklist

Review untrusted input parsing and ensure bounds checking.
"#,
    )
    .unwrap();

    let standalone_path = base_dir.join("quick-lint.skill.md");
    fs::write(
        &standalone_path,
        r#"---
name: quick-lint
description: Fast linter check
tags:
  - lint
allowed_tools:
  - read_file
---
Run cargo clippy on touched files.
"#,
    )
    .unwrap();

    let corrupt_dir = base_dir.join("corrupt");
    fs::create_dir_all(&corrupt_dir).unwrap();
    fs::write(
        corrupt_dir.join("SKILL.md"),
        r#"---
name:
  - not-a-string
---
Instructions
"#,
    )
    .unwrap();

    let loader = SkillLoader::empty().with_search_path(&base_dir);
    let (skills, diagnostics) = loader.discover_with_diagnostics();

    // 3 valid skills discovered
    assert_eq!(skills.len(), 3);
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0]
        .message
        .contains("failed to parse skill YAML frontmatter"));

    // Register into SkillRegistry
    let registry = SkillRegistry::new();
    let loaded = loader.load_into_registry(&registry).unwrap();
    assert_eq!(loaded, 3);
    assert_eq!(registry.len(), 3);

    // Verify retrieval
    let git_skill = registry
        .get("git-assistant")
        .expect("git-assistant must exist");
    assert_eq!(git_skill.name(), "git-assistant");
    assert_eq!(git_skill.tags(), &["git", "vcs", "workflow"]);
    assert_eq!(git_skill.allowed_tools(), &["git_status", "git_commit"]);
    assert_eq!(
        git_skill.parameters().get("signoff"),
        Some(&serde_json::json!(true))
    );
    assert!(git_skill
        .instructions()
        .contains("# Git Assistant Guidelines"));

    // Verify tag filtering
    let vcs_skills = registry.find_by_tag("vcs");
    assert_eq!(vcs_skills.len(), 1);
    assert_eq!(vcs_skills[0].name(), "git-assistant");

    let audit_skills = registry.find_by_tag("security");
    assert_eq!(audit_skills.len(), 1);
    assert_eq!(audit_skills[0].name(), "code-auditor");

    // Verify tool filtering
    let read_file_users = registry.find_by_tool("read_file");
    assert_eq!(read_file_users.len(), 2);
    let names: Vec<&str> = read_file_users.iter().map(|s| s.name()).collect();
    assert!(names.contains(&"code-auditor"));
    assert!(names.contains(&"quick-lint"));

    // Format prompt context
    let prompt_context = registry.format_prompt_context();
    assert!(prompt_context.contains("## Available Agent Skills"));
    assert!(prompt_context.contains("### Skill: git-assistant"));
    assert!(prompt_context.contains("### Skill: code-auditor"));
    assert!(prompt_context.contains("### Skill: quick-lint"));

    let _ = fs::remove_dir_all(&base_dir);
}

#[test]
fn test_skill_runtime_permission_enforcement() {
    let tool_registry = ToolRegistry::new();
    tool_registry
        .register_tool(MockTool::new("read_file"))
        .unwrap();
    tool_registry
        .register_tool(MockTool::new("git_status"))
        .unwrap();

    let skill_registry = SkillRegistry::new();

    // Skill 1: uses only registered tools
    let compliant_skill = Skill::new(
        SkillMetadata::new("compliant-agent", "Uses legitimate registered tools")
            .with_allowed_tools(vec!["read_file".into(), "git_status".into()]),
        "Compliant instructions",
    )
    .unwrap();
    skill_registry.register(compliant_skill).unwrap();

    assert!(skill_registry
        .validate_all_permissions(&tool_registry)
        .is_ok());

    // Skill 2: attempts to declare an unauthorized/unregistered tool (e.g. bash or arbitrary exec)
    let rogue_skill = Skill::new(
        SkillMetadata::new(
            "rogue-agent",
            "Attempts to bypass permissions with forbidden tools",
        )
        .with_allowed_tools(vec!["arbitrary_exec".into()]),
        "Malicious instructions",
    )
    .unwrap();
    skill_registry.register(rogue_skill).unwrap();

    // Verification must fail with PermissionDenied
    let res = skill_registry.validate_all_permissions(&tool_registry);
    assert!(res.is_err());
    match res.unwrap_err() {
        CortexError::PermissionDenied(msg) => {
            assert!(msg.contains("rogue-agent"));
            assert!(msg.contains("arbitrary_exec"));
        }
        other => panic!("expected PermissionDenied, got {:?}", other),
    }
}

#[test]
fn test_frontmatter_edge_cases_and_recovery() {
    // 1. Whitespace around delimiters
    let doc_with_whitespace =
        "   \n---\nname: whitespace-test\ndescription: Delimiter with leading spaces\n---\nBody";
    let skill = Skill::parse_str(doc_with_whitespace).expect("should handle leading whitespace");
    assert_eq!(skill.name(), "whitespace-test");

    // 2. Trailing spaces on closing delimiter
    let doc_trailing_spaces = "---\nname: trailing\ndescription: Trailing spaces\n---   \nBody";
    let skill = Skill::parse_str(doc_trailing_spaces)
        .expect("should handle trailing spaces on closing delimiter");
    assert_eq!(skill.name(), "trailing");

    // 3. Multiline instructions with code blocks and frontmatter-like text
    let doc_complex_body = r#"---
name: complex-body
description: Contains markdown code fences and dashes in body
---
# Guide

Here is some YAML example:
```yaml
---
key: value
---
```
And some more text with --- horizontal rule.
"#;
    let skill = Skill::parse_str(doc_complex_body).expect("should parse complex body");
    assert_eq!(skill.name(), "complex-body");
    assert!(skill.instructions().contains("```yaml"));
    assert!(skill.instructions().contains("key: value"));
}
