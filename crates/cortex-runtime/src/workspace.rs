//! Workspace boundary enforcement and path containment.

use cortex_core::{CortexError, Result};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Manages and enforces the filesystem boundary for agent execution.
///
/// Any file access, directory listing, or subprocess execution must be confined
/// to the canonical root of this workspace. Any attempt to traverse outside
/// via `..`, symlinks, or absolute paths will result in [`CortexError::PermissionDenied`].
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Create a new [`Workspace`] pinned to the specified root directory.
    ///
    /// The root directory will be created if it does not already exist, and
    /// canonicalized to resolve symlinks and relative components.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let path = root.as_ref();
        if !path.exists() {
            fs::create_dir_all(path).map_err(|e| {
                CortexError::Internal(format!(
                    "failed to create workspace root '{}': {}",
                    path.display(),
                    e
                ))
            })?;
        }

        let canonical_root = fs::canonicalize(path).map_err(|e| {
            CortexError::Internal(format!(
                "failed to canonicalize workspace root '{}': {}",
                path.display(),
                e
            ))
        })?;

        if !canonical_root.is_dir() {
            return Err(CortexError::Validation(format!(
                "workspace root '{}' is not a directory",
                canonical_root.display()
            )));
        }

        Ok(Self {
            root: canonical_root,
        })
    }

    /// Access the canonical root directory of this workspace.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve and validate a requested path, ensuring it stays within workspace boundaries.
    ///
    /// Supports both relative paths and absolute paths that point within the workspace root.
    /// Returns [`CortexError::PermissionDenied`] if the path escapes the workspace.
    pub fn resolve_path(&self, requested: impl AsRef<Path>) -> Result<PathBuf> {
        let requested_path = requested.as_ref();

        // Reject null byte injection attempts
        if requested_path.as_os_str().to_string_lossy().contains('\0') {
            return Err(CortexError::Validation(
                "null-byte injection detected in path".to_string(),
            ));
        }

        // Prevent obvious directory traversal tokens in relative paths
        for component in requested_path.components() {
            if component == Component::ParentDir {
                // Check if normalizing escapes workspace
                let candidate = self.normalize_path(&self.root.join(requested_path));
                if !candidate.starts_with(&self.root) {
                    return Err(CortexError::PermissionDenied(format!(
                        "path traversal detected outside workspace: '{}'",
                        requested_path.display()
                    )));
                }
            }
        }

        let joined = if requested_path.is_absolute() {
            self.normalize_path(requested_path)
        } else {
            self.normalize_path(&self.root.join(requested_path))
        };

        // If the path exists on disk, canonicalize it directly
        if joined.exists() {
            let canonical = fs::canonicalize(&joined).map_err(|e| {
                CortexError::Internal(format!(
                    "failed to canonicalize path '{}': {}",
                    joined.display(),
                    e
                ))
            })?;

            if !canonical.starts_with(&self.root) {
                return Err(CortexError::PermissionDenied(format!(
                    "path '{}' resolves outside workspace root '{}'",
                    canonical.display(),
                    self.root.display()
                )));
            }

            Ok(canonical)
        } else {
            // If the path does not exist yet (e.g. creating a new file),
            // canonicalize the existing parent directory and append the filename.
            let mut ancestors = joined.ancestors();
            let mut existing_ancestor = None;
            let mut missing_components = Vec::new();

            for ancestor in ancestors.by_ref() {
                if ancestor.exists() {
                    existing_ancestor = Some(ancestor);
                    break;
                }
                if let Some(file_name) = ancestor.file_name() {
                    missing_components.push(file_name);
                }
            }

            let base = match existing_ancestor {
                Some(existing) => fs::canonicalize(existing).map_err(|e| {
                    CortexError::Internal(format!(
                        "failed to canonicalize ancestor '{}': {}",
                        existing.display(),
                        e
                    ))
                })?,
                None => self.root.clone(),
            };

            if !base.starts_with(&self.root) {
                return Err(CortexError::PermissionDenied(format!(
                    "path '{}' escapes workspace root '{}'",
                    joined.display(),
                    self.root.display()
                )));
            }

            let mut final_path = base;
            for part in missing_components.into_iter().rev() {
                final_path.push(part);
            }

            if !final_path.starts_with(&self.root) {
                return Err(CortexError::PermissionDenied(format!(
                    "path '{}' escapes workspace root '{}'",
                    final_path.display(),
                    self.root.display()
                )));
            }

            Ok(final_path)
        }
    }

    /// Convert a full path into a relative path from the workspace root.
    pub fn relative_path(&self, full_path: impl AsRef<Path>) -> Result<PathBuf> {
        let resolved = self.resolve_path(full_path)?;
        resolved
            .strip_prefix(&self.root)
            .map(|p| p.to_path_buf())
            .map_err(|e| CortexError::Internal(format!("failed to compute relative path: {}", e)))
    }

    /// Internal helper to normalize path components logically without requiring disk access.
    fn normalize_path(&self, path: &Path) -> PathBuf {
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Prefix(prefix) => normalized.push(Component::Prefix(prefix)),
                Component::RootDir => normalized.push(Component::RootDir),
                Component::CurDir => {}
                Component::ParentDir => {
                    normalized.pop();
                }
                Component::Normal(c) => normalized.push(c),
            }
        }
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn test_workspace_creation_and_containment() {
        let temp_dir = std::env::temp_dir().join(format!("cortex_test_ws_{}", std::process::id()));
        let ws = Workspace::new(&temp_dir).unwrap();
        assert!(ws.root().is_dir());

        // Resolve normal relative file
        let file_path = ws.resolve_path("src/main.rs").unwrap();
        assert!(file_path.starts_with(ws.root()));
        assert!(file_path.ends_with("src/main.rs"));

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_workspace_traversal_prevention() {
        let temp_dir =
            std::env::temp_dir().join(format!("cortex_test_ws_traversal_{}", std::process::id()));
        let ws = Workspace::new(&temp_dir).unwrap();

        // Traversal attempt with ../../
        let err = ws.resolve_path("../../etc/passwd").unwrap_err();
        match err {
            CortexError::PermissionDenied(msg) => {
                assert!(msg.contains("path traversal detected") || msg.contains("escapes"));
            }
            _ => panic!(
                "expected PermissionDenied for traversal attempt, got: {:?}",
                err
            ),
        }

        // Absolute path outside workspace
        let external_path = std::env::temp_dir().join("some_other_dir/secret.txt");
        if !external_path.starts_with(ws.root()) {
            let err = ws.resolve_path(&external_path).unwrap_err();
            match err {
                CortexError::PermissionDenied(_) => {}
                _ => panic!("expected PermissionDenied for absolute path outside workspace"),
            }
        }

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_workspace_existing_file_resolution() {
        let temp_dir =
            std::env::temp_dir().join(format!("cortex_test_ws_file_{}", std::process::id()));
        let ws = Workspace::new(&temp_dir).unwrap();

        let test_file = ws.root().join("hello.txt");
        File::create(&test_file).unwrap();

        let resolved = ws.resolve_path("hello.txt").unwrap();
        assert_eq!(resolved, test_file);

        let rel = ws.relative_path("hello.txt").unwrap();
        assert_eq!(rel, Path::new("hello.txt"));

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
