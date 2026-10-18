use std::path::{Path, PathBuf};

use concats_core::error::{Error, Result};

use crate::{HandlerAction, InstallScope, json_config};

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
        .map(|h| h.join(".cursor").join("hooks.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}

// NOTE: the config transform, tested on its own; install only says where the file is.
// coland-ignore: @coland/single_caller_private_function
fn install_hooks(mut value: serde_json::Value, binary: &Path) -> Result<serde_json::Value> {
    json_config::root(&mut value, AGENT)?.insert("version".into(), serde_json::json!(1));
    json_config::install_hooks(
        value,
        AGENT,
        &["beforeSubmitPrompt", "afterFileEdit"],
        |_, command| serde_json::json!({ "command": command }),
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
}
