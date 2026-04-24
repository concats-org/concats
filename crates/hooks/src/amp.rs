use std::path::{Path, PathBuf};

use concats_core::error::{Error, Result};

use crate::{HandlerAction, InstallScope, plugin};

const AGENT: &str = "amp";
const PLUGIN_TEMPLATE: &str = include_str!("../plugins/amp.ts");

pub(crate) struct AmpAgent;

impl crate::Agent for AmpAgent {
    fn name(&self) -> &'static str {
        AGENT
    }

    fn is_detected(&self) -> bool {
        dirs::home_dir().is_some_and(|home| home.join(".config").join("amp").is_dir())
    }

    fn dispatch(&self, event: Option<&str>, payload_json: &str) -> Result<()> {
        let event =
            event.ok_or_else(|| Error::session(format!("{AGENT} requires an event name")))?;
        crate::dispatch_simple(
            "Amp",
            "amp-default",
            event,
            payload_json,
            |event| match event {
                "session.start" => Ok(HandlerAction::SessionStarted),
                "agent.start" => Ok(HandlerAction::PromptSubmitted),
                "agent.end" => Ok(HandlerAction::Stop),
                "tool.result" => Ok(HandlerAction::FilesChanged),
                "tool.call" => Ok(HandlerAction::Ignore),
                _ => Err(Error::session(format!("unknown Amp hook event: {event}"))),
            },
        )
    }

    fn install(&self, binary: &Path, scope: &InstallScope) -> Result<()> {
        let _ = scope;
        plugin::write(&plugin_path()?, PLUGIN_TEMPLATE, binary)
    }

    fn uninstall(&self, scope: &InstallScope) -> Result<()> {
        let _ = scope;
        plugin::remove(&plugin_path()?)
    }

    fn is_installed(&self, scope: &InstallScope) -> bool {
        let _ = scope;
        plugin_path().ok().is_some_and(|p| plugin::exists(&p))
    }
}

fn plugin_path() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| {
            h.join(".config")
                .join("amp")
                .join("plugins")
                .join("concats.ts")
        })
        .ok_or_else(|| Error::session("cannot determine home directory"))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::path::Path;

    use super::*;

    mod install {
        use super::*;

        #[test]
        fn writes_plugin_with_binary_path() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("concats.ts");
            plugin::write(&path, PLUGIN_TEMPLATE, Path::new("/usr/bin/concats")).unwrap();

            let data = std::fs::read_to_string(&path).unwrap();
            assert!(data.contains("const BINARY = \"/usr/bin/concats\""));
            assert!(!data.contains("{{BINARY_PATH}}"));
        }
    }

    mod uninstall {
        use super::*;

        #[test]
        fn removes_plugin() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("concats.ts");
            plugin::write(&path, PLUGIN_TEMPLATE, Path::new("concats")).unwrap();
            assert!(path.exists());

            plugin::remove(&path).unwrap();
            assert!(!path.exists());
        }
    }
}
