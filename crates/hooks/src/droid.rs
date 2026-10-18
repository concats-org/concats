use std::path::{Path, PathBuf};

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

    fn install(&self, binary: &Path, _: &InstallScope) -> Result<()> {
        json_config::apply(&config_path()?, |value| install_hooks(value, binary))
    }

    fn uninstall(&self, _: &InstallScope) -> Result<()> {
        json_config::uninstall(&config_path()?, AGENT)
    }

    fn is_installed(&self, _: &InstallScope) -> bool {
        config_path().is_ok_and(|path| json_config::is_installed(&path, AGENT))
    }
}

fn config_path() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".factory").join("settings.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}

fn install_hooks(mut value: serde_json::Value, binary: &Path) -> Result<serde_json::Value> {
    json_config::root(&mut value, AGENT)?
        .insert("claudeHooksImported".into(), serde_json::json!(true));
    json_config::install_hooks(
        value,
        AGENT,
        &["PostToolUse"],
        |_, command| {
            serde_json::json!({
                "matcher": MATCHER,
                "hooks": [{ "type": "command", "command": command }],
            })
        },
        binary,
    )
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
            assert!(!data.contains("concats hook droid PreToolUse"));
            assert!(data.contains("\"claudeHooksImported\":true"));
        }
    }
}
