use std::path::{Path, PathBuf};

use concats_core::error::{Error, Result};

use crate::{HandlerAction, InstallScope, json_config};

const AGENT: &str = "gemini";
const MATCHER: &str = "write_file|replace";

pub(crate) struct GeminiAgent;

impl crate::Agent for GeminiAgent {
    fn name(&self) -> &'static str {
        AGENT
    }

    fn is_detected(&self) -> bool {
        dirs::home_dir().is_some_and(|home| home.join(".gemini").is_dir())
    }

    fn dispatch(&self, event: Option<&str>, payload_json: &str) -> Result<()> {
        let event =
            event.ok_or_else(|| Error::session(format!("{AGENT} requires an event name")))?;
        crate::dispatch_simple(
            "Gemini",
            "gemini-default",
            event,
            payload_json,
            |event| match event {
                "AfterTool" | "BeforeTool" => Ok(HandlerAction::FilesChanged),
                _ => Err(Error::session(format!(
                    "unknown Gemini hook event: {event}"
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
        .map(|h| h.join(".gemini").join("settings.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}

fn install_hooks(mut value: serde_json::Value, binary: &Path) -> Result<serde_json::Value> {
    json_config::object(&mut value, AGENT, "tools")?
        .insert("enableHooks".into(), serde_json::json!(true));
    json_config::install_hooks(
        value,
        AGENT,
        &["BeforeTool", "AfterTool"],
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
                    "AfterTool": [
                        { "matcher": "read_file", "hooks": [{ "type": "command", "command": "other-tool hook" }] },
                        { "matcher": "write_file|replace", "hooks": [{ "type": "command", "command": "concats hook gemini AfterTool" }] }
                    ]
                }
            });

            let config = install_hooks(config, Path::new("concats")).unwrap();

            let data = serde_json::to_string(&config).unwrap();
            assert!(data.contains("other-tool hook"));
            assert!(data.contains("concats hook gemini AfterTool"));
            assert!(data.contains("concats hook gemini BeforeTool"));
            assert!(data.contains("\"enableHooks\":true"));
        }
    }
}
