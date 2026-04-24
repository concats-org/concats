use std::{fs, path::Path};

use concats_core::error::{Error, Result};

/// Read a JSON config file, returning `{}` if the file does not exist.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn read(path: &Path) -> Result<serde_json::Value> {
    if path.exists() {
        let data = fs::read_to_string(path)?;
        serde_json::from_str(&data)
            .map_err(|error| Error::session(format!("invalid JSON at {}: {error}", path.display())))
    } else {
        Ok(serde_json::json!({}))
    }
}

/// Write a JSON value to a config file, creating parent directories as needed.
///
/// # Errors
///
/// Returns an error if directories cannot be created, the value cannot be
/// serialized, or the file cannot be written.
pub fn write(path: &Path, value: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(value)
        .map_err(|error| Error::session(format!("failed to serialize config: {error}")))?;
    fs::write(path, format!("{data}\n"))?;
    Ok(())
}

/// Read a JSON config, apply `transform`, and write the result back.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, written, or if
/// `transform` fails.
pub fn apply<F>(path: &Path, transform: F) -> Result<()>
where
    F: FnOnce(serde_json::Value) -> Result<serde_json::Value>,
{
    let value = read(path)?;
    let value = transform(value)?;
    write(path, &value)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    mod apply {
        use super::*;

        #[test]
        fn applies_transform() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("settings.json");

            apply(&path, |mut value| {
                let root = value.as_object_mut().unwrap();
                root.insert("hooks".into(), serde_json::json!({}));
                Ok(value)
            })
            .unwrap();

            let data = fs::read_to_string(&path).unwrap();
            assert!(data.contains("\"hooks\""));
        }
    }
}
