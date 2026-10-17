use std::path::{Path, PathBuf};

use concats_core::error::{Error, Result};

use crate::{HandlerAction, InstallScope, json_config};

const AGENT: &str = "windsurf";

pub(crate) struct WindsurfAgent;

impl crate::Agent for WindsurfAgent {
    fn name(&self) -> &'static str {
        AGENT
    }

    fn is_detected(&self) -> bool {
        dirs::home_dir().is_some_and(|home| home.join(".codeium").join("windsurf").is_dir())
    }

    fn dispatch(&self, event: Option<&str>, payload_json: &str) -> Result<()> {
        let event =
            event.ok_or_else(|| Error::session(format!("{AGENT} requires an event name")))?;
        crate::dispatch_simple(
            "Windsurf",
            "windsurf-default",
            event,
            payload_json,
            |event| match event {
                "pre_write_code" | "post_write_code" => Ok(HandlerAction::FilesChanged),
                "post_cascade_response_with_transcript" => Ok(HandlerAction::Stop),
                _ => Err(Error::session(format!(
                    "unknown Windsurf hook event: {event}"
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
        .map(|h| h.join(".codeium").join("windsurf").join("hooks.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}

fn install_hooks(value: serde_json::Value, binary: &Path) -> Result<serde_json::Value> {
    json_config::install_hooks(
        value,
        AGENT,
        &[
            "pre_write_code",
            "post_write_code",
            "post_cascade_response_with_transcript",
        ],
        |_, command| serde_json::json!({ "command": command, "show_output": false }),
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
                    "post_write_code": [
                        { "command": "other-tool hook" },
                        { "command": "concats hook windsurf post_write_code", "show_output": false }
                    ]
                }
            });

            let config = install_hooks(config, Path::new("concats")).unwrap();

            let data = serde_json::to_string(&config).unwrap();
            assert!(data.contains("other-tool hook"));
            assert!(data.contains("concats hook windsurf post_write_code"));
            assert!(data.contains("concats hook windsurf pre_write_code"));
        }
    }
}
