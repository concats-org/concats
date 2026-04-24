use std::{
    fs,
    path::{Path, PathBuf},
};

use concats_core::error::{Error, Result};

use crate::{HandlerAction, InstallScope, json_config};

const AGENT: &str = "droid";
const MATCHER: &str = "^(Edit|Write|Create|ApplyPatch)$";

pub(crate) struct DroidAgent;

impl crate::Agent for DroidAgent {
    fn name(&self) -> &'static str {
        AGENT
    }

    fn is_detected(&self) -> bool {
        dirs::home_dir().is_some_and(|home| home.join(".factory").is_dir())
    }

    fn dispatch(&self, event: Option<&str>, payload_json: &str) -> Result<()> {
        let event =
            event.ok_or_else(|| Error::session(format!("{AGENT} requires an event name")))?;
        crate::dispatch_simple(
            "Droid",
            "droid-default",
            event,
            payload_json,
            |event| match event {
                "PostToolUse" => Ok(HandlerAction::FilesChanged),
                "PreToolUse" => Ok(HandlerAction::Ignore),
                _ => Err(Error::session(format!("unknown Droid hook event: {event}"))),
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
        .map(|h| h.join(".factory").join("settings.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}

fn install_hooks(mut value: serde_json::Value, binary: &Path) -> Result<serde_json::Value> {
    let root = value
        .as_object_mut()
        .ok_or_else(|| Error::session("droid config root is not an object"))?;
    root.insert("claudeHooksImported".into(), serde_json::json!(true));

    let command_prefix = format!("concats hook {AGENT}");
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| Error::session("droid hooks is not an object"))?;

    for event in ["PreToolUse", "PostToolUse"] {
        let entries = hooks
            .entry(event.to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| Error::session(format!("droid hooks.{event} is not an array")))?;
        entries.retain(|entry| {
            !entry
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.contains(&command_prefix))
                    })
                })
        });
        entries.push(serde_json::json!({
            "matcher": MATCHER,
            "hooks": [{
                "type": "command",
                "command": format!("{} hook {AGENT} {event}", binary.display()),
            }]
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
                    !entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .is_some_and(|hooks| {
                            hooks.iter().any(|hook| {
                                hook.get("command")
                                    .and_then(|c| c.as_str())
                                    .is_some_and(|c| c.contains(&command_prefix))
                            })
                        })
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
        fn preserves_other_hooks() {
            let config = serde_json::json!({
                "hooks": {
                    "PostToolUse": [
                        { "matcher": ".*", "hooks": [{ "type": "command", "command": "other-tool hook" }] },
                        { "matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [{ "type": "command", "command": "concats hook droid PostToolUse" }] }
                    ]
                }
            });

            let config = install_hooks(config, Path::new("concats")).unwrap();

            let data = serde_json::to_string(&config).unwrap();
            assert!(data.contains("other-tool hook"));
            assert!(data.contains("concats hook droid PostToolUse"));
            assert!(data.contains("concats hook droid PreToolUse"));
            assert!(data.contains("\"claudeHooksImported\":true"));
        }
    }

    mod remove_hooks {
        use super::*;

        #[test]
        fn removes_only_concats_hooks() {
            let config = serde_json::json!({
                "hooks": {
                    "PostToolUse": [
                        { "matcher": ".*", "hooks": [{ "type": "command", "command": "other-tool hook" }] },
                        { "matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [{ "type": "command", "command": "concats hook droid PostToolUse" }] }
                    ]
                }
            });

            let config = remove_hooks(config);

            let data = serde_json::to_string(&config).unwrap();
            assert!(data.contains("other-tool hook"));
            assert!(!data.contains("concats hook droid"));
        }
    }
}
