# Agent skills guide

Cortex implements the Agent Skill Open Standard, giving autonomous workers discoverable workflows, domain instructions, and tool constraints.

Skills are defined as markdown documents containing YAML frontmatter and instructional markdown content.

## Skill properties

Skills in Cortex satisfy five properties:

- **Discoverable**: Loaded automatically from workspace and user search paths.
- **Configurable**: Accept custom parameters and tag-based filtering.
- **Attachable**: Can be bound to agent execution loops and prompt contexts.
- **Documented**: Structured markdown body gives step-by-step guidance to the model.
- **Permission-aware**: Declare allowed tools that are validated against runtime tool security policies.

Skills cannot bypass tool permission levels or runtime execution tracing.

## File structure

Skills live either in dedicated directories with a `SKILL.md` file, or as standalone `.skill.md` files:

```text
skills/
  code-review/
    SKILL.md
    references/
      style-guide.md
  git-workflow/
    SKILL.md
  quick-lint.skill.md
```

Each skill document begins with YAML frontmatter delimited by `---`:

```markdown
---
name: code-review
description: Inspect pull request changes for safety and style
version: 1.0.0
author: Cortex Contributors
tags:
  - review
  - quality
allowed_tools:
  - read_file
  - git_diff
parameters:
  strict_mode: true
---

# Code Review Instructions

1. Inspect modified lines and understand the intent.
2. Check for missing validation or edge-case handling.
3. Verify that new tests accompany logic changes.
```

## Frontmatter fields

The YAML frontmatter supports the following fields:

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | Yes | Unique identifier (alphanumeric, hyphens, underscores, dots). |
| `description` | string | Yes | Short explanation of the skill. |
| `version` | string | No | Semantic version of the skill definition. |
| `author` | string | No | Creator or maintainer attribution. |
| `tags` | list of strings | No | Keywords used for classification and filtering. |
| `allowed_tools` | list of strings | No | Tool names required or allowed by the skill. |
| `parameters` | key-value map | No | Optional configuration settings. |

## Discovery paths

The `SkillLoader` scans the following default locations in order:

1. `./skills` (project root)
2. `./.cortex/skills` (workspace directory)
3. `~/.cortex/skills` (user home directory)

Custom paths can also be added programmatically using `loader.with_search_path(path)`.

When multiple search roots contain a skill with the same name, the first discovered instance takes precedence. Corrupted documents produce diagnostics rather than halting discovery of remaining skills.

## Security boundaries and permission checks

Skills declare which tools they intend to use via `allowed_tools`.

During runtime registration:
1. `validate_permissions` cross-references declared tools with the `ToolRegistry`.
2. If a skill declares a tool that does not exist in the active registry, validation fails with `CortexError::PermissionDenied`.
3. If an agent policy restricts capabilities (such as read-only filesystem), a skill cannot grant write permissions or override sandbox restrictions.

Model outputs never have authority to alter skill definitions or elevate tool permissions.

## Rust API usage

### Parsing a skill

```rust
use cortex_runtime::skill::Skill;

let doc = r#"---
name: reviewer
description: Pull request reviewer
tags:
  - git
allowed_tools:
  - read_file
---
Examine changes carefully.
"#;

let skill = Skill::parse_str(doc)?;
assert_eq!(skill.name(), "reviewer");
```

### Loading skills from disk

```rust
use cortex_runtime::skill::{SkillLoader, SkillRegistry};

let loader = SkillLoader::new();
let registry = SkillRegistry::new();

let count = loader.load_into_registry(&registry)?;
println!("Loaded {} skills", count);
```

### Querying skills and generating prompt context

```rust
use cortex_runtime::skill::SkillRegistry;

let registry = SkillRegistry::new();
// ... populate registry ...

// Find by tag
let git_skills = registry.find_by_tag("git");

// Find by tool
let file_readers = registry.find_by_tool("read_file");

// Format prompt context for model instructions
let prompt = registry.format_prompt_context();
```
