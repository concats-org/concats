use std::path::Path;

use concats_core::error::{Error, Result};

use crate::{HandlerAction, InstallScope, json_config};

const AGENT: &str = "copilot";

pub(crate) struct CopilotAgent;

impl crate::Agent for CopilotAgent {
    fn name(&self) -> &'static str {
        AGENT
    }

    fn is_detected(&self) -> bool {
        dirs::home_dir().is_some_and(|home| home.join(".config").join("gh").is_dir())
    }

    fn dispatch(&self, event: Option<&str>, payload_json: &str) -> Result<()> {
        let event =
            event.ok_or_else(|| Error::session(format!("{AGENT} requires an event name")))?;
        crate::dispatch_simple("Copilot", "copilot-default", event, payload_json, |event| {
            match event {
                "PostToolUse" => Ok(HandlerAction::FilesChanged),
                "PreToolUse" => Ok(HandlerAction::Ignore),
                _ => Err(Error::session(format!(
                    "unknown Copilot hook event: {event}"
                ))),
            }
        })
    }

    fn install(&self, binary: &Path, scope: &InstallScope) -> Result<()> {
        let _ = scope;
        let path = config_path()?;
        write_config(&path, binary)
    }

    fn uninstall(&self, scope: &InstallScope) -> Result<()> {
        let _ = scope;
        let path = config_path()?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    fn is_installed(&self, scope: &InstallScope) -> bool {
        let _ = scope;
        config_path().ok().is_some_and(|p| p.exists())
    }
}

fn write_config(path: &Path, binary: &Path) -> Result<()> {
    let bin = binary.display();
    let config = serde_json::json!({
        "PreToolUse": [format!("{bin} hook copilot PreToolUse")],
        "PostToolUse": [format!("{bin} hook copilot PostToolUse")]
    });
    json_config::write(path, &config)
}

fn config_path() -> Result<std::path::PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".github").join("hooks").join("concats.json"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::path::Path;

    use super::*;

    mod write_config {
        use super::*;

        #[test]
        fn creates_hook_commands() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("concats.json");
            write_config(&path, Path::new("concats")).unwrap();

            let data = std::fs::read_to_string(&path).unwrap();
            assert!(data.contains("concats hook copilot PreToolUse"));
            assert!(data.contains("concats hook copilot PostToolUse"));
        }
    }

    mod uninstall {
        use super::*;

        #[test]
        fn removes_config_file() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("concats.json");
            write_config(&path, Path::new("concats")).unwrap();
            assert!(path.exists());

            std::fs::remove_file(&path).unwrap();
            assert!(!path.exists());
        }
    }
}
