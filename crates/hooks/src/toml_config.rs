use std::{fs, path::Path};

use concats_core::error::{Error, Result};

/// Read a TOML config file, returning an empty table if the file does not
/// exist.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn read(path: &Path) -> Result<::toml::Value> {
    if path.exists() {
        let data = fs::read_to_string(path)?;
        ::toml::from_str::<::toml::Value>(&data)
            .map_err(|error| Error::session(format!("invalid TOML at {}: {error}", path.display())))
    } else {
        Ok(::toml::Value::Table(::toml::Table::new()))
    }
}

/// Write a TOML value to a config file, creating parent directories as needed.
///
/// # Errors
///
/// Returns an error if directories cannot be created, the value cannot be
/// serialized, or the file cannot be written.
pub fn write(path: &Path, value: &::toml::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = ::toml::to_string_pretty(value)
        .map_err(|error| Error::session(format!("failed to serialize TOML: {error}")))?;
    fs::write(path, data)?;
    Ok(())
}

/// Read a TOML config, apply `transform`, and write the result back.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, written, or if
/// `transform` fails.
pub fn apply<F>(path: &Path, transform: F) -> Result<()>
where
    F: FnOnce(::toml::Value) -> Result<::toml::Value>,
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
            let path = dir.path().join("config.toml");

            apply(&path, |mut value| {
                let table = value.as_table_mut().unwrap();
                table.insert("hooks".into(), ::toml::Value::Table(::toml::Table::new()));
                Ok(value)
            })
            .unwrap();

            let data = fs::read_to_string(&path).unwrap();
            assert!(data.contains("hooks"));
        }
    }
}
