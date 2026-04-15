use std::{
    fs,
    path::{Path, PathBuf},
};

use concats_core::error::{Error, Result};

/// Read a JSON config file, returning `{}` if the file does not exist.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn read_json_config(path: &Path) -> Result<serde_json::Value> {
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
pub fn write_json_config(path: &Path, value: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(value)
        .map_err(|error| Error::session(format!("failed to serialize config: {error}")))?;
    fs::write(path, format!("{data}\n"))?;
    Ok(())
}

/// Read a TOML config file, returning an empty table if the file does not
/// exist.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn read_toml_config(path: &Path) -> Result<toml::Value> {
    if path.exists() {
        let data = fs::read_to_string(path)?;
        toml::from_str::<toml::Value>(&data)
            .map_err(|error| Error::session(format!("invalid TOML at {}: {error}", path.display())))
    } else {
        Ok(toml::Value::Table(toml::Table::new()))
    }
}

/// Write a TOML value to a config file, creating parent directories as needed.
///
/// # Errors
///
/// Returns an error if directories cannot be created, the value cannot be
/// serialized, or the file cannot be written.
pub fn write_toml_config(path: &Path, value: &toml::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = toml::to_string_pretty(value)
        .map_err(|error| Error::session(format!("failed to serialize TOML: {error}")))?;
    fs::write(path, data)?;
    Ok(())
}

/// Write a plugin file, creating parent directories as needed.
///
/// # Errors
///
/// Returns an error if directories cannot be created or the file cannot be
/// written.
pub fn write_plugin_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

/// Return the canonicalized path to the current executable, falling back to
/// `"concats"` if it cannot be determined.
#[must_use]
pub fn binary_path() -> PathBuf {
    std::env::current_exe().map_or_else(
        |_| PathBuf::from("concats"),
        |p| p.canonicalize().unwrap_or(p),
    )
}

/// Remove entries from a JSON hooks array whose command contains the given
/// marker string (e.g. `"concats hook"`).
pub fn remove_matching_entries(array: &mut Vec<serde_json::Value>, marker: &str) {
    array.retain(|entry| {
        // Flat command string
        if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
            return !cmd.contains(marker);
        }
        // Nested hooks array (Claude/Gemini/Droid style)
        if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
            return !hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains(marker))
            });
        }
        true
    });
}

/// Callback that mutates the root JSON object before hook entries are
/// inserted (e.g. to set Gemini's `tools.enableHooks` flag).
pub type PrepareRoot = fn(&mut serde_json::Map<String, serde_json::Value>) -> Result<()>;

/// Callback that builds a single hook entry for a given event and binary.
pub type BuildEntry = fn(binary: &Path, event: &str) -> serde_json::Value;

/// Declarative description of a JSON-config based hook installation.
///
/// A spec captures everything the agent modules used to copy-paste:
/// which events to register, the marker string used to identify concats
/// entries, an optional callback to set extra root-level fields
/// (e.g. Gemini's `tools.enableHooks`), and a builder for the per-event
/// hook entry JSON.
pub struct JsonHookSpec {
    pub marker: &'static str,
    pub events: &'static [&'static str],
    pub prepare_root: Option<PrepareRoot>,
    pub entry: BuildEntry,
}

/// Install concats hook entries into a JSON config file described by `spec`.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, mutated in place,
/// or written back.
pub fn install_json_hooks(path: &Path, spec: &JsonHookSpec, binary: &Path) -> Result<()> {
    let mut config = read_json_config(path)?;
    let root = config
        .as_object_mut()
        .ok_or_else(|| Error::session(format!("{} root is not an object", path.display())))?;
    if let Some(prepare) = spec.prepare_root {
        prepare(root)?;
    }
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| Error::session("hooks is not an object"))?;
    for event in spec.events {
        let entries = hooks
            .entry((*event).to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| Error::session(format!("hooks.{event} is not an array")))?;
        remove_matching_entries(entries, spec.marker);
        entries.push((spec.entry)(binary, event));
    }
    write_json_config(path, &config)
}

/// Remove concats hook entries from a JSON config file by marker string.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or written back.
pub fn uninstall_json_hooks(path: &Path, marker: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut config = read_json_config(path)?;
    if let Some(hooks) = config
        .as_object_mut()
        .and_then(|o| o.get_mut("hooks"))
        .and_then(|h| h.as_object_mut())
    {
        for entries in hooks.values_mut() {
            if let Some(arr) = entries.as_array_mut() {
                remove_matching_entries(arr, marker);
            }
        }
    }
    write_json_config(path, &config)
}

/// Check whether a JSON config file contains the given concats marker.
#[must_use]
pub fn is_json_hooks_installed(path: &Path, marker: &str) -> bool {
    fs::read_to_string(path).is_ok_and(|s| s.contains(marker))
}

/// Install a plugin template file, interpolating `{{BINARY_PATH}}`.
///
/// # Errors
///
/// Returns an error if parent directories cannot be created or the file
/// cannot be written.
pub fn install_plugin(path: &Path, template: &str, binary: &Path) -> Result<()> {
    let content = template.replace("{{BINARY_PATH}}", &binary.display().to_string());
    write_plugin_file(path, &content)
}

/// Remove a plugin file if it exists.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be removed.
pub fn uninstall_plugin(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Check whether a plugin file exists at `path`.
#[must_use]
pub fn is_plugin_installed(path: &Path) -> bool {
    path.exists()
}
