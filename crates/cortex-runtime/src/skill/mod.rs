//! Agent Skills loader, parser, discovery engine, and runtime registry.
//!
//! Provides support for the Agent Skill Open Standard, parsing `SKILL.md` documents
//! containing YAML frontmatter and markdown instructions, discovering skills across
//! project roots and user home directories, and enforcing tool permission boundaries.

pub mod loader;
pub mod metadata;
pub mod registry;

pub use loader::{SkillDiagnostic, SkillLoader};
pub use metadata::{Skill, SkillMetadata};
pub use registry::SkillRegistry;
