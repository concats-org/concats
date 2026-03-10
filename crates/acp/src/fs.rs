use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// File system operations scoped to a workspace root.
pub struct FileSystem {
    workspace_root: PathBuf,
}

impl FileSystem {
    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    /// Resolve and validate a path, ensuring it stays within the workspace root.
    fn validate_path(&self, path: &Path) -> Result<PathBuf> {
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        };

        // NOTE: We canonicalize the workspace root at validation time to handle symlinks.
        // The target path may not exist yet (for writes), so we canonicalize its parent.
        let canonical_root = self
            .workspace_root
            .canonicalize()
            .map_err(|e| Error::Io { source: e })?;

        let canonical_path = if resolved.exists() {
            resolved
                .canonicalize()
                .map_err(|e| Error::Io { source: e })?
        } else {
            // For non-existent paths, canonicalize the parent and append the filename.
            let parent = resolved.parent().ok_or_else(|| Error::PathEscape {
                path: resolved.clone(),
                root: canonical_root.clone(),
            })?;
            let parent_canonical = parent.canonicalize().map_err(|e| Error::Io { source: e })?;
            match resolved.file_name() {
                Some(name) => parent_canonical.join(name),
                None => parent_canonical,
            }
        };

        if !canonical_path.starts_with(&canonical_root) {
            return Err(Error::PathEscape {
                path: resolved,
                root: canonical_root,
            });
        }

        Ok(canonical_path)
    }

    /// Read a UTF-8 text file inside the workspace root.
    ///
    /// # Errors
    ///
    /// Returns an error if the path escapes the workspace root or the file
    /// cannot be read as text.
    pub async fn read_text_file(&self, path: &Path) -> Result<String> {
        let validated = self.validate_path(path)?;
        let content = tokio::fs::read_to_string(&validated).await?;
        Ok(content)
    }

    /// Write a UTF-8 text file inside the workspace root.
    ///
    /// # Errors
    ///
    /// Returns an error if the path escapes the workspace root, parent
    /// directories cannot be created, or the file cannot be written.
    pub async fn write_text_file(&self, path: &Path, content: &str) -> Result<()> {
        let validated = self.validate_path(path)?;
        if let Some(parent) = validated.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&validated, content).await?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_write_within_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileSystem::new(dir.path().to_path_buf());

        fs.write_text_file(Path::new("test.txt"), "hello")
            .await
            .unwrap();
        let content = fs.read_text_file(Path::new("test.txt")).await.unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn rejects_path_escape() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileSystem::new(dir.path().to_path_buf());

        let result = fs.read_text_file(Path::new("../../../etc/passwd")).await;
        assert!(result.is_err());
    }
}
