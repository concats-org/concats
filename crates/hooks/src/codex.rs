use std::path::Path;

use concats_core::{
    error::{Error, Result},
    turn::TurnEntry,
};
use serde::Deserialize;

use crate::{InstallScope, handler, open_repo, toml_config};

const AGENT: &str = "codex";
const PROMPT_EVENT: &str = "UserPromptSubmit";
const TOOL_EVENT: &str = "PostToolUse";
const STOP_EVENT: &str = "Stop";
const EVENTS: [&str; 3] = [PROMPT_EVENT, TOOL_EVENT, STOP_EVENT];

pub(crate) struct CodexAgent;

impl crate::Agent for CodexAgent {
    fn name(&self) -> &'static str {
        AGENT
    }

    fn is_detected(&self) -> bool {
        config_path().is_ok_and(|path| path.parent().is_some_and(Path::is_dir))
    }

    fn dispatch(&self, event: Option<&str>, payload_json: &str) -> Result<()> {
        let payload: Payload = serde_json::from_str(payload_json)
            .map_err(|error| Error::session(format!("invalid Codex payload: {error}")))?;
        let event = event
            .or(payload.hook_event_name.as_deref())
            .ok_or_else(|| Error::session("Codex requires a hook event name"))?;
        if !EVENTS.contains(&event) {
            return Err(Error::session(format!("unknown Codex hook event: {event}")));
        }
        let repo = open_repo(Some(&payload.cwd))?;
        // NOTE: Opened after the prompt check, so a bad payload creates no
        // session.
        let session = || handler::load_or_create_session(repo, &payload.session_id);
        match event {
            PROMPT_EVENT => {
                let prompt = payload
                    .prompt
                    .as_deref()
                    .ok_or_else(|| Error::session("Codex UserPromptSubmit is missing prompt"))?;
                handler::on_prompt_submitted(&session()?, "Codex", prompt)
            }
            TOOL_EVENT => handler::on_files_changed(&session()?, "Codex"),
            _ => {
                let entries: Vec<_> = payload
                    .last_assistant_message
                    .map(TurnEntry::response_now)
                    .into_iter()
                    .collect();
                handler::on_stop(&session()?, "Codex", &entries)
            }
        }
    }

    fn install(&self, binary: &Path, _scope: &InstallScope) -> Result<()> {
        toml_config::apply(&config_path()?, |value| install_hooks(value, binary))
    }

    fn uninstall(&self, _scope: &InstallScope) -> Result<()> {
        let path = config_path()?;
        if !path.exists() {
            return Ok(());
        }
        toml_config::apply(&path, |value| Ok(remove_hooks(value)))
    }

    fn is_installed(&self, _scope: &InstallScope) -> bool {
        config_path().is_ok_and(|path| {
            toml_config::read(&path).is_ok_and(|value| {
                EVENTS.iter().all(|event| {
                    value
                        .get("hooks")
                        .and_then(|hooks| hooks.get(*event))
                        .and_then(::toml::Value::as_array)
                        .is_some_and(|groups| {
                            groups.iter().any(|group| {
                                group
                                    .get("hooks")
                                    .and_then(::toml::Value::as_array)
                                    .is_some_and(|hooks| hooks.iter().any(is_concats_hook))
                            })
                        })
                })
            })
        })
    }
}

#[derive(Debug, Deserialize)]
struct Payload {
    session_id: String,
    cwd: String,
    hook_event_name: Option<String>,
    prompt: Option<String>,
    last_assistant_message: Option<String>,
}

fn config_path() -> Result<std::path::PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .map(|home| home.join("config.toml"))
        .ok_or_else(|| Error::session("cannot determine home directory"))
}

fn install_hooks(value: ::toml::Value, binary: &Path) -> Result<::toml::Value> {
    let mut value = remove_hooks(value);
    let table = value
        .as_table_mut()
        .ok_or_else(|| Error::session("codex config root is not a table"))?;
    let hooks = table
        .entry("hooks")
        .or_insert_with(|| ::toml::Value::Table(::toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| Error::session("codex hooks is not a table"))?;
    let binary = binary
        .to_str()
        .ok_or_else(|| Error::session("Codex hook executable path is not UTF-8"))?;
    let quoted = shlex::try_quote(binary).map_err(|error| Error::session(error.to_string()))?;
    for event in EVENTS {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| ::toml::Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| Error::session(format!("codex hooks.{event} is not an array")))?;
        let group = ::toml::Value::try_from(serde_json::json!({
            "hooks": [{"type": "command", "command": format!("{quoted} hook {AGENT}")}]
        }))
        .map_err(|error| Error::session(format!("invalid Codex hook config: {error}")))?;
        groups.push(group);
    }
    Ok(value)
}

fn is_concats_hook(value: &::toml::Value) -> bool {
    value
        .get("command")
        .and_then(::toml::Value::as_str)
        .and_then(shlex::split)
        .is_some_and(|args| args.len() == 3 && args[1] == "hook" && args[2] == AGENT)
}

fn remove_notify(table: &mut ::toml::Table) {
    if table
        .get("notify")
        .and_then(::toml::Value::as_array)
        .is_some_and(|args| {
            args.get(1).and_then(::toml::Value::as_str) == Some("hook")
                && args.get(2).and_then(::toml::Value::as_str) == Some(AGENT)
        })
    {
        table.remove("notify");
    }
}

fn remove_hooks(mut value: ::toml::Value) -> ::toml::Value {
    if let Some(table) = value.as_table_mut() {
        remove_notify(table);
        if let Some(hooks) = table.get_mut("hooks").and_then(::toml::Value::as_table_mut) {
            remove_notify(hooks);
            for event in EVENTS {
                if let Some(groups) = hooks.get_mut(event).and_then(::toml::Value::as_array_mut) {
                    groups.retain_mut(|group| {
                        if let Some(handlers) =
                            group.get_mut("hooks").and_then(::toml::Value::as_array_mut)
                        {
                            handlers.retain(|hook| !is_concats_hook(hook));
                            return !handlers.is_empty();
                        }
                        true
                    });
                }
            }
        }
    }
    value
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn installs_each_lifecycle_hook_with_a_quoted_executable() {
        let binary = Path::new("/Applications/Concats's app/$bin/concats");
        let value = install_hooks(::toml::Value::Table(::toml::Table::new()), binary).unwrap();
        for event in EVENTS {
            let group = &value["hooks"][event][0];
            assert!(group.get("matcher").is_none());
            let command = group["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(
                shlex::split(command).unwrap(),
                [binary.to_str().unwrap(), "hook", AGENT]
            );
        }
        assert!(value.get("notify").is_none());
    }

    #[test]
    fn reinstall_and_uninstall_preserve_other_handlers_and_notify() {
        let original: ::toml::Value = ::toml::from_str(
            r#"
notify = ["computer-use", "turn-ended"]
[[hooks.Stop]]
matcher = "*"
[[hooks.Stop.hooks]]
type = "command"
command = "other-handler"
[[hooks.Stop.hooks]]
type = "command"
command = "'/old/concats' hook codex"
"#,
        )
        .unwrap();
        let installed = install_hooks(original.clone(), Path::new("/new/concats")).unwrap();
        assert_eq!(
            install_hooks(installed.clone(), Path::new("/new/concats")).unwrap(),
            installed
        );
        let removed = remove_hooks(installed);
        assert_eq!(removed["notify"], original["notify"]);
        let stop = removed["hooks"][STOP_EVENT].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["matcher"].as_str(), Some("*"));
        assert_eq!(stop[0]["hooks"].as_array().unwrap().len(), 1);
        assert_eq!(
            stop[0]["hooks"][0]["command"].as_str(),
            Some("other-handler")
        );
    }

    #[test]
    fn migrates_only_concats_notify_commands() {
        for source in [
            "notify = ['concats', 'hook', 'codex', '--payload']",
            "[hooks]\nnotify = ['concats', 'hook', 'codex']",
        ] {
            let value =
                install_hooks(::toml::from_str(source).unwrap(), Path::new("concats")).unwrap();
            assert!(value.get("notify").is_none());
            assert!(value["hooks"].get("notify").is_none());
            assert_eq!(value["hooks"][PROMPT_EVENT].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn rejects_missing_identity_and_unknown_events() {
        use crate::Agent;
        for payload in [
            r#"{"hook_event_name":"UserPromptSubmit","cwd":"/tmp","prompt":"hello"}"#,
            r#"{"hook_event_name":"Unknown","session_id":"test","cwd":"/tmp"}"#,
            r#"{"type":"agent-turn-complete","thread-id":"test","cwd":"/tmp"}"#,
        ] {
            assert!(CodexAgent.dispatch(None, payload).is_err());
        }
    }
}
