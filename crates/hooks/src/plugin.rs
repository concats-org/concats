use std::{fs, path::Path};

use concats_core::error::Result;

/// Render and write a plugin file, creating parent directories as needed.
///
/// # Errors
///
/// Returns an error if directories cannot be created or the file cannot be
/// written.
pub fn write(path: &Path, template: &str, binary: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = template.replace("{{BINARY_PATH}}", &binary.display().to_string());
    fs::write(path, content)?;
    Ok(())
}

/// Remove a plugin file if it exists.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be removed.
pub fn remove(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Check whether a plugin file exists at `path`.
#[must_use]
pub fn exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    mod write {
        use super::*;

        #[test]
        fn renders_binary_path() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("plugins").join("concats.ts");

            write(&path, "exec {{BINARY_PATH}}", Path::new("concats")).unwrap();
            assert!(exists(&path));
            assert_eq!(fs::read_to_string(&path).unwrap(), "exec concats");
        }
    }

    mod remove {
        use super::*;

        #[test]
        fn deletes_plugin_file() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("plugins").join("concats.ts");

            write(&path, "exec {{BINARY_PATH}}", Path::new("concats")).unwrap();
            assert!(exists(&path));

            super::remove(&path).unwrap();
            assert!(!exists(&path));
        }
    }
}
