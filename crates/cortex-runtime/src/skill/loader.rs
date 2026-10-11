//! Filesystem discovery and loading for Agent Skill documents.

use super::metadata::Skill;
use super::registry::SkillRegistry;
use cortex_core::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Diagnostic warning or error encountered during skill discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiagnostic {
    /// Filesystem path of the file that triggered the diagnostic.
    pub path: PathBuf,
    /// Explanation of the failure or parsing error.
    pub message: String,
}

impl SkillDiagnostic {
    /// Construct a new [`SkillDiagnostic`].
    pub fn new(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Discovers and loads Agent Skills from configured directory paths.
#[derive(Debug, Clone)]
pub struct SkillLoader {
    search_paths: Vec<PathBuf>,
}

impl Default for SkillLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillLoader {
    /// Create a [`SkillLoader`] configured with standard search paths:
    /// - `./skills`
    /// - `./.cortex/skills`
    /// - `~/.cortex/skills`
    pub fn new() -> Self {
        let mut paths = vec![PathBuf::from("skills"), PathBuf::from(".cortex/skills")];

        if let Some(home) = home_directory() {
            paths.push(home.join(".cortex").join("skills"));
        }

        Self {
            search_paths: paths,
        }
    }

    /// Create an empty [`SkillLoader`] with no initial search paths.
    pub fn empty() -> Self {
        Self {
            search_paths: Vec::new(),
        }
    }

    /// Add a search path returning `self`.
    pub fn with_search_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.search_paths.push(path.into());
        self
    }

    /// Append a search path to the loader.
    pub fn add_search_path(&mut self, path: impl Into<PathBuf>) {
        self.search_paths.push(path.into());
    }

    /// View currently configured search paths.
    pub fn search_paths(&self) -> &[PathBuf] {
        &self.search_paths
    }

    /// Discover skills from all search paths, returning parsed skills.
    ///
    /// Skips nonexistent search roots and logs or ignores malformed documents.
    pub fn discover(&self) -> Result<Vec<Skill>> {
        let (skills, _) = self.discover_with_diagnostics();
        Ok(skills)
    }

    /// Discover skills from all search paths, returning skills and diagnostics.
    pub fn discover_with_diagnostics(&self) -> (Vec<Skill>, Vec<SkillDiagnostic>) {
        let mut skills = Vec::new();
        let mut diagnostics = Vec::new();
        let mut seen_names = HashSet::new();

        for root in &self.search_paths {
            if !root.exists() {
                continue;
            }

            // If root points directly to a file
            if root.is_file() {
                match Skill::from_file(root) {
                    Ok(skill) => {
                        if seen_names.insert(skill.name().to_string()) {
                            skills.push(skill);
                        }
                    }
                    Err(e) => {
                        diagnostics.push(SkillDiagnostic::new(root.clone(), e.to_string()));
                    }
                }
                continue;
            }

            // Inspect the directory
            self.scan_directory(root, &mut skills, &mut diagnostics, &mut seen_names);
        }

        (skills, diagnostics)
    }

    /// Load discovered skills directly into a [`SkillRegistry`].
    ///
    /// Returns the number of newly registered skills.
    pub fn load_into_registry(&self, registry: &SkillRegistry) -> Result<usize> {
        let skills = self.discover()?;
        let mut count = 0;
        for skill in skills {
            if !registry.contains(skill.name()) {
                registry.register(skill)?;
                count += 1;
            }
        }
        Ok(count)
    }

    fn scan_directory(
        &self,
        dir: &Path,
        skills: &mut Vec<Skill>,
        diagnostics: &mut Vec<SkillDiagnostic>,
        seen_names: &mut HashSet<String>,
    ) {
        // Check for SKILL.md directly inside dir
        for direct_name in ["SKILL.md", "skill.md"] {
            let direct_file = dir.join(direct_name);
            if direct_file.is_file() {
                match Skill::from_file(&direct_file) {
                    Ok(skill) => {
                        if seen_names.insert(skill.name().to_string()) {
                            skills.push(skill);
                        }
                    }
                    Err(e) => {
                        diagnostics.push(SkillDiagnostic::new(direct_file, e.to_string()));
                    }
                }
                break;
            }
        }

        // Iterate child entries
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                diagnostics.push(SkillDiagnostic::new(
                    dir.to_path_buf(),
                    format!("failed to read directory: {}", e),
                ));
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Look for child/SKILL.md or child/skill.md
                for skill_filename in ["SKILL.md", "skill.md"] {
                    let candidate = path.join(skill_filename);
                    if candidate.is_file() {
                        match Skill::from_file(&candidate) {
                            Ok(skill) => {
                                if seen_names.insert(skill.name().to_string()) {
                                    skills.push(skill);
                                }
                            }
                            Err(e) => {
                                diagnostics.push(SkillDiagnostic::new(candidate, e.to_string()));
                            }
                        }
                        break;
                    }
                }
            } else if path.is_file() {
                // Check standalone *.skill.md
                if let Some(file_name) = path.file_name().and_then(|f| f.to_str()) {
                    if file_name.ends_with(".skill.md") {
                        match Skill::from_file(&path) {
                            Ok(skill) => {
                                if seen_names.insert(skill.name().to_string()) {
                                    skills.push(skill);
                                }
                            }
                            Err(e) => {
                                diagnostics.push(SkillDiagnostic::new(path, e.to_string()));
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Resolve user home directory using environment variables.
fn home_directory() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loader_paths() {
        let loader = SkillLoader::new();
        assert!(loader.search_paths().contains(&PathBuf::from("skills")));
        assert!(loader
            .search_paths()
            .contains(&PathBuf::from(".cortex/skills")));

        let empty = SkillLoader::empty().with_search_path("custom/path");
        assert_eq!(empty.search_paths(), &[PathBuf::from("custom/path")]);
    }

    #[test]
    fn test_discover_skills_in_temp_dir() {
        let tmp = std::env::temp_dir().join(format!("cortex_skill_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // 1. Subdirectory with SKILL.md
        let skill1_dir = tmp.join("skill-one");
        std::fs::create_dir_all(&skill1_dir).unwrap();
        let skill1_file = skill1_dir.join("SKILL.md");
        std::fs::write(
            &skill1_file,
            "---\nname: skill-one\ndescription: First test skill\ntags:\n  - test\n---\nRun step 1.",
        )
        .unwrap();

        // 2. Subdirectory with lowercase skill.md
        let skill2_dir = tmp.join("skill-two");
        std::fs::create_dir_all(&skill2_dir).unwrap();
        let skill2_file = skill2_dir.join("skill.md");
        std::fs::write(
            &skill2_file,
            "---\nname: skill-two\ndescription: Second test skill\n---\nRun step 2.",
        )
        .unwrap();

        // 3. Standalone *.skill.md file
        let skill3_file = tmp.join("helper.skill.md");
        std::fs::write(
            &skill3_file,
            "---\nname: helper-skill\ndescription: Helper skill\n---\nRun helper.",
        )
        .unwrap();

        // 4. Invalid file triggering diagnostic
        let bad_file = tmp.join("bad.skill.md");
        std::fs::write(&bad_file, "---\nmalformed yaml\n---\nBody").unwrap();

        let loader = SkillLoader::empty().with_search_path(&tmp);
        let (skills, diagnostics) = loader.discover_with_diagnostics();

        assert_eq!(skills.len(), 3);
        let names: Vec<&str> = skills.iter().map(|s| s.name()).collect();
        assert!(names.contains(&"skill-one"));
        assert!(names.contains(&"skill-two"));
        assert!(names.contains(&"helper-skill"));

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].path, bad_file);

        let registry = SkillRegistry::new();
        let count = loader.load_into_registry(&registry).unwrap();
        assert_eq!(count, 3);
        assert_eq!(registry.len(), 3);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_deduplication_first_search_path_wins() {
        let tmp1 = std::env::temp_dir().join(format!("cortex_dedup_test1_{}", std::process::id()));
        let tmp2 = std::env::temp_dir().join(format!("cortex_dedup_test2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp1);
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp1).unwrap();
        std::fs::create_dir_all(&tmp2).unwrap();

        std::fs::write(
            tmp1.join("same.skill.md"),
            "---\nname: conflict\ndescription: from tmp1\n---\n1",
        )
        .unwrap();
        std::fs::write(
            tmp2.join("same.skill.md"),
            "---\nname: conflict\ndescription: from tmp2\n---\n2",
        )
        .unwrap();

        let loader = SkillLoader::empty()
            .with_search_path(&tmp1)
            .with_search_path(&tmp2);

        let skills = loader.discover().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description(), "from tmp1");

        let _ = std::fs::remove_dir_all(&tmp1);
        let _ = std::fs::remove_dir_all(&tmp2);
    }
}
