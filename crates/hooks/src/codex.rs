use std::{path::Path, rc::Rc};

use concats_core::{
    Repository,
    error::{Error, Result},
};
use serde::Deserialize;

use crate::{InstallScope, find_worktree_root, handler, toml_config};

const AGENT: &str = "codex";

pub(crate) struct CodexAgent;

impl crate::Agent for CodexAgent {
    fn name(&self) -> &'static str {
        AGENT
    }

    fn is_detected(&self) -> bool {
        dirs::home_dir().is_some_and(|home| home.join(".codex").is_dir())
    }

    fn dispatch(&self, _event: Option<&str>, payload_json: &str) -> Result<()> {
        let payload: Payload = serde_json::from_str(payload_json)
            .map_err(|error| Error::session(format!("invalid Codex payload: {error}")))?;
        let session_id = payload.session_id.as_deref().unwrap_or("codex-default");
        let worktree_root = find_worktree_root(payload.cwd.as_deref().map(Path::new))?;
        let repo = Rc::new(Repository::open(&worktree_root)?);

        handler::on_files_changed(repo.clone(), session_id, "Codex")?;

        if let Some(transcript) = &payload.transcript_path {
            match std::fs::read_to_string(transcript) {
                Ok(response) => handler::on_stop(repo, session_id, "Codex", &response)?,
                Err(error) => {
                    eprintln!("warning: failed to read codex transcript at {transcript}: {error}");
                }
            }
        }

        Ok(())
    }

    fn install(&self, binary: &Path, scope: &InstallScope) -> Result<()> {
        let _ = scope;
        toml_config::apply(&config_path()?, |value| install_hooks(value, binary))
    }

    fn uninstall(&self, scope: &InstallScope) -> Result<()> {
        let _ = scope;
        let path = config_path()?;
        if !path.exists() {
            return Ok(());
        }
        toml_config::apply(&path, |v| Ok(remove_hooks(v)))
    }

    fn is_installed(&self, scope: &InstallScope) -> bool {
        let _ = scope;
        config_path()
            .ok()
            .is_some_and(|p| std::fs::read_to_string(p).is_ok_and(|s| s.contains("concats")))
    }
}

#[derive(Debug, Deserialize)]
struct Payload {
    #[serde(alias = "thread_id", alias = "thread-id")]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
}

fn config_path() -> Result<std::path::PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".codex").join("config.toml"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}

fn install_hooks(mut value: ::toml::Value, binary: &Path) -> Result<::toml::Value> {
    let table = value
        .as_table_mut()
        .ok_or_else(|| Error::session("codex config root is not a table"))?;
    let hooks = table
        .entry("hooks")
        .or_insert_with(|| ::toml::Value::Table(::toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| Error::session("codex hooks is not a table"))?;
    hooks.insert(
        "notify".into(),
        ::toml::Value::Array(vec![
            ::toml::Value::String(binary.display().to_string()),
            ::toml::Value::String("hook".into()),
            ::toml::Value::String("codex".into()),
        ]),
    );
    Ok(value)
}

fn remove_hooks(mut value: ::toml::Value) -> ::toml::Value {
    if let Some(hooks) = value
        .as_table_mut()
        .and_then(|t| t.get_mut("hooks"))
        .and_then(|h| h.as_table_mut())
    {
        hooks.remove("notify");
    }
    value
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::path::Path;

    use super::*;

    fn empty() -> ::toml::Value {
        ::toml::Value::Table(::toml::Table::new())
    }

    mod install_hooks {
        use super::*;

        #[test]
        fn adds_notify_hook() {
            let value = install_hooks(empty(), Path::new("concats")).unwrap();

            let notify = value["hooks"]["notify"].as_array().unwrap();
            assert_eq!(notify[0].as_str().unwrap(), "concats");
            assert_eq!(notify[1].as_str().unwrap(), "hook");
            assert_eq!(notify[2].as_str().unwrap(), "codex");
        }

        #[test]
        fn replaces_existing_notify_hook() {
            let value = install_hooks(empty(), Path::new("old-binary")).unwrap();
            let value = install_hooks(value, Path::new("concats")).unwrap();

            let notify = value["hooks"]["notify"].as_array().unwrap();
            assert_eq!(notify[0].as_str().unwrap(), "concats");
        }
    }

    mod remove_hooks {
        use super::*;

        #[test]
        fn removes_notify_hook() {
            let value = install_hooks(empty(), Path::new("concats")).unwrap();
            let value = remove_hooks(value);

            let hooks = value["hooks"].as_table().unwrap();
            assert!(!hooks.contains_key("notify"));
        }
    }
}
