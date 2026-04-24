use std::{
    fs,
    path::{Path, PathBuf},
};

use concats_core::error::{Error, Result};

use crate::{ find_worktree_root, HandlerAction, InstallScope, json_config, };

const AGENT: &str = "cursor";

pub(crate) struct CursorAgent;

impl crate::Agent for CursorAgent {
    fn name(&self) -> &'static str {
        AGENT
    }

    fn is_detected(&self) -> bool {
        dirs::home_dir().is_some_and(|home| home.join(".cursor").is_dir())
    }

    fn dispatch(&self, event: Option<&str>, payload_json: &str) -> Result<()> {
        let event =
            event.ok_or_else(|| Error::session(format!("{AGENT} requires an event name")))?;
        crate::dispatch_simple(
            "Cursor",
            "cursor-default",
            event,
            payload_json,
            |event| match event {
                "afterFileEdit" => Ok(HandlerAction::FilesChanged),
                "beforeSubmitPrompt" => Ok(HandlerAction::PromptSubmitted),
                _ => Err(Error::session(format!(
                    "unknown Cursor hook event: {event}"
                ))),
            },
        )
    }

    fn install(&self, binary: &Path, scope: &InstallScope) -> Result<()> {
        let _ = scope;
        json_config::apply(&config_path()?, |value| install_hooks(value, binary))
    }

    fn uninstall(&self, scope: &InstallScope) -> Result<()> {
        let _ = scope;
        let path = config_path()?;
        if !path.exists() {
            return Ok(());
        }
        json_config::apply(&path, |v| Ok(remove_hooks(v)))
    }

    fn is_installed(&self, scope: &InstallScope) -> bool {
        let _ = scope;
        config_path().ok().is_some_and(|path| {
            fs::read_to_string(path)
                .is_ok_and(|data| data.contains(&format!("concats hook {AGENT}")))
        })
    }
}

fn config_path() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".cursor").join("hooks.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}

fn install_hooks(mut value: serde_json::Value, binary: &Path) -> Result<serde_json::Value> {
    let root = value
        .as_object_mut()
        .ok_or_else(|| Error::session("cursor config root is not an object"))?;
    root.insert("version".into(), serde_json::json!(1));

    let command_prefix = format!("concats hook {AGENT}");
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| Error::session("cursor hooks is not an object"))?;

    for event in ["beforeSubmitPrompt", "afterFileEdit"] {
        let entries = hooks
            .entry(event.to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| Error::session(format!("cursor hooks.{event} is not an array")))?;
        entries.retain(|entry| {
            entry
                .get("command")
                .and_then(|c| c.as_str())
                .is_none_or(|c| !c.contains(&command_prefix))
        });
        entries.push(serde_json::json!({
            "command": format!("{} hook {AGENT} {event}", binary.display()),
        }));
    }

    Ok(value)
}

fn remove_hooks(mut value: serde_json::Value) -> serde_json::Value {
    let command_prefix = format!("concats hook {AGENT}");
    if let Some(hooks) = value.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for entries in hooks.values_mut() {
            if let Some(arr) = entries.as_array_mut() {
                arr.retain(|entry| {
                    entry
                        .get("command")
                        .and_then(|c| c.as_str())
                        .is_none_or(|c| !c.contains(&command_prefix))
                });
            }
        }
    }
    value
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::path::Path;

    use super::*;

    mod install_hooks {
        use super::*;

        #[test]
        fn preserves_other_commands() {
            let config = serde_json::json!({
                "hooks": {
                    "afterFileEdit": [
                        { "command": "other-tool hook" },
                        { "command": "concats hook cursor afterFileEdit" }
                    ]
                }
            });

            let config = install_hooks(config, Path::new("concats")).unwrap();

            let data = serde_json::to_string(&config).unwrap();
            assert!(data.contains("other-tool hook"));
            assert!(data.contains("concats hook cursor afterFileEdit"));
        }
    }

    mod remove_hooks {
        use super::*;

        #[test]
        fn removes_only_concats_hooks() {
            let config = serde_json::json!({
                "hooks": {
                    "afterFileEdit": [
                        { "command": "other-tool hook" },
                        { "command": "concats hook cursor afterFileEdit" }
                    ]
                }
            });

            let config = remove_hooks(config);

            let data = serde_json::to_string(&config).unwrap();
            assert!(data.contains("other-tool hook"));
            assert!(!data.contains("concats hook cursor"));
        }
    }
}
