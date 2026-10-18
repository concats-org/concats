use std::{fs, path::Path};

use concats_core::error::{Error, Result};
use serde_json::{Map, Value, json};

/// Read a JSON config file, returning `{}` if the file does not exist.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn read(path: &Path) -> Result<Value> {
    if path.exists() {
        let data = fs::read_to_string(path)?;
        serde_json::from_str(&data)
            .map_err(|error| Error::session(format!("invalid JSON at {}: {error}", path.display())))
    } else {
        Ok(json!({}))
    }
}

/// Write a JSON value to a config file, creating parent directories as needed.
///
/// # Errors
///
/// Returns an error if directories cannot be created, the value cannot be
/// serialized, or the file cannot be written.
pub fn write(path: &Path, value: &Value) -> Result<()> {
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
    F: FnOnce(Value) -> Result<Value>,
{
    let value = read(path)?;
    let value = transform(value)?;
    write(path, &value)
}

/// The config's root object.
///
/// # Errors
///
/// Returns an error if the root is not an object.
pub fn root<'a>(value: &'a mut Value, agent: &str) -> Result<&'a mut Map<String, Value>> {
    value
        .as_object_mut()
        .ok_or_else(|| Error::session(format!("{agent} config root is not an object")))
}

/// The object at `root.<key>`, created empty if absent.
///
/// # Errors
///
/// Returns an error if the root or `root.<key>` is not an object.
pub fn object<'a>(
    value: &'a mut Value,
    agent: &str,
    key: &str,
) -> Result<&'a mut Map<String, Value>> {
    root(value, agent)?
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| Error::session(format!("{agent} {key} is not an object")))
}

/// Put one concats entry under `hooks.<event>` for each event, keeping
/// everyone else's. Ours from an earlier install go first, under every event,
/// so one this version no longer hooks stops firing on a reinstall.
///
/// # Errors
///
/// Returns an error if the root or `hooks` is not an object, or a
/// `hooks.<event>` is not an array.
pub fn install_hooks(
    value: Value,
    agent: &str,
    events: &[&str],
    entry: impl Fn(&str, String) -> Value,
    binary: &Path,
) -> Result<Value> {
    let mut value = remove_hooks(value, agent);
    let hooks = object(&mut value, agent, "hooks")?;
    for &event in events {
        let entries = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| Error::session(format!("{agent} hooks.{event} is not an array")))?;
        entries.push(entry(
            event,
            format!("{} hook {agent} {event}", binary.display()),
        ));
    }
    Ok(value)
}

/// Remove `agent`'s hooks from the config at `path`, if there is one.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or written.
pub fn uninstall(path: &Path, agent: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    apply(path, |value| Ok(remove_hooks(value, agent)))
}

/// Whether the config at `path` holds one of `agent`'s hook commands.
#[must_use]
pub fn is_installed(path: &Path, agent: &str) -> bool {
    fs::read_to_string(path).is_ok_and(|data| data.contains(&command_prefix(agent)))
}

fn remove_hooks(mut value: Value, agent: &str) -> Value {
    let prefix = command_prefix(agent);
    if let Some(hooks) = value.get_mut("hooks").and_then(Value::as_object_mut) {
        for entries in hooks.values_mut().filter_map(Value::as_array_mut) {
            entries.retain(|entry| !runs(entry, &prefix));
        }
    }
    value
}

fn command_prefix(agent: &str) -> String {
    format!("concats hook {agent}")
}

/// Whether a hook entry runs a command containing `prefix`, either its own
/// `command` (flat) or one in its `hooks` list (nested).
// NOTE: the one test for an entry being ours, across the flat and the nested shape.
// coland-ignore: @coland/single_caller_private_function
fn runs(entry: &Value, prefix: &str) -> bool {
    let command_runs = |hook: &Value| {
        hook.get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(prefix))
    };
    command_runs(entry)
        || entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hooks| hooks.iter().any(command_runs))
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

    mod remove_hooks {
        use super::*;

        #[test]
        fn removes_only_concats_hooks_nested_and_flat() {
            let config = json!({
                "hooks": {
                    "PostToolUse": [
                        { "matcher": ".*", "hooks": [{ "type": "command", "command": "other-tool hook" }] },
                        { "matcher": "Write", "hooks": [{ "type": "command", "command": "concats hook droid PostToolUse" }] }
                    ],
                    "afterFileEdit": [
                        { "command": "other-tool hook" },
                        { "command": "concats hook droid afterFileEdit" }
                    ]
                }
            });

            let config = remove_hooks(config, "droid");

            let data = serde_json::to_string(&config).unwrap();
            assert_eq!(data.matches("other-tool hook").count(), 2);
            assert!(!data.contains("concats hook droid"));
        }
    }

    mod install_hooks {
        use super::*;

        #[test]
        fn a_reinstall_drops_our_entries_for_events_no_longer_hooked() {
            let config = json!({
                "hooks": {
                    "PreToolUse": [
                        { "matcher": ".*", "hooks": [{ "type": "command", "command": "other-tool hook" }] },
                        { "matcher": "Write", "hooks": [{ "type": "command", "command": "concats hook droid PreToolUse" }] }
                    ],
                    "PostToolUse": [
                        { "matcher": "Write", "hooks": [{ "type": "command", "command": "concats hook droid PostToolUse" }] }
                    ]
                }
            });

            let config = install_hooks(
                config,
                "droid",
                &["PostToolUse"],
                |_, command| json!({ "matcher": "Write", "hooks": [{ "type": "command", "command": command }] }),
                Path::new("concats"),
            )
            .unwrap();

            assert_eq!(
                config["hooks"]["PreToolUse"],
                json!([{ "matcher": ".*", "hooks": [{ "type": "command", "command": "other-tool hook" }] }])
            );
            let data = serde_json::to_string(&config).unwrap();
            assert!(!data.contains("concats hook droid PreToolUse"));
            assert_eq!(data.matches("concats hook droid PostToolUse").count(), 1);
        }
    }
}
