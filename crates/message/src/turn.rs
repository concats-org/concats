use std::{fmt::Display, str::FromStr};

use pest::{Parser as _, iterators::Pair};
use pest_derive::Parser;

use crate::{Error, Result, SessionId};

const DEFAULT_TURN_SUBJECT: &str = "turn";
const SESSION_TRAILER_PREFIX: &str = "Session: ";
const AGENT_TRAILER_PREFIX: &str = "Agent: ";

#[derive(Parser)]
#[grammar = "turn.pest"]
struct TurnParser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    subject: String,
    session_id: SessionId,
    agent_name: Option<String>,
    entries: Vec<TurnEntry>,
}

impl Turn {
    #[must_use]
    pub fn new(session_id: SessionId) -> Self {
        Self {
            subject: DEFAULT_TURN_SUBJECT.to_string(),
            session_id,
            agent_name: None,
            entries: Vec::new(),
        }
    }

    /// Return a new turn message with one more entry.
    #[must_use]
    pub fn with_entry(mut self, entry: TurnEntry) -> Self {
        self.entries.push(entry);
        self
    }

    /// Return a new turn message with an explicit subject.
    ///
    /// # Errors
    ///
    /// Returns an error if the subject contains newline characters.
    pub fn with_subject(mut self, subject: impl Into<String>) -> Result<Self> {
        let subject = subject.into();
        validate_trailer_value("subject", &subject)?;
        self.subject = subject;
        Ok(self)
    }

    /// Return a new turn message with an explicit agent name.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent name contains newline characters.
    pub fn with_agent_name(mut self, agent_name: impl Into<String>) -> Result<Self> {
        let agent_name = agent_name.into();
        validate_trailer_value("agent name", &agent_name)?;
        self.agent_name = Some(agent_name);
        Ok(self)
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    #[must_use]
    pub fn agent_name(&self) -> Option<&str> {
        self.agent_name.as_deref()
    }

    #[must_use]
    pub fn entries(&self) -> &[TurnEntry] {
        &self.entries
    }

    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    #[must_use]
    pub fn suggest_subject(&self) -> Option<String> {
        let summary = self
            .entries
            .iter()
            .map(TurnEntry::summary)
            .find(|summary| !summary.is_empty())?;

        if summary.chars().count() <= 60 {
            return Some(summary);
        }

        let window = match summary.char_indices().nth(60) {
            Some((index, _)) => &summary[..index],
            None => &summary,
        };
        if let Some(pos) = window.rfind(['.', '!', '?'])
            && window[..pos].chars().count() >= 10
        {
            return Some(window[..=pos].to_string());
        }
        if let Some(pos) = window.rfind(' ') {
            Some(format!("{}...", &window[..pos]))
        } else {
            Some(format!("{window}..."))
        }
    }
}

impl FromStr for Turn {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        let mut pairs = TurnParser::parse(Rule::commit_message, input)
            .map_err(|error| Error::turn(format!("invalid turn commit message: {error}")))?;

        let commit_message = pairs
            .next()
            .ok_or_else(|| Error::turn("invalid turn commit message"))?;

        let mut subject = None;
        let mut session_id = None;
        let mut agent_name = None;
        let mut entries = Vec::new();

        for pair in commit_message.into_inner() {
            match pair.as_rule() {
                Rule::subject => {
                    subject = Some(pair.as_str().to_string());
                }
                Rule::EOI => {}
                Rule::transcript => {
                    for entry in pair.into_inner() {
                        entries.push(parse_entry(entry)?);
                    }
                }
                Rule::session => {
                    let value = pair.into_inner().next().map_or("", |v| v.as_str());
                    session_id = Some(value.parse()?);
                }
                Rule::agent => {
                    let value = pair.into_inner().next().map_or("", |v| v.as_str());
                    agent_name = Some(value.to_string());
                }
                _ => unreachable!("unexpected turn message rule: {:?}", pair.as_rule()),
            }
        }

        let subject = subject.ok_or_else(|| Error::turn("turn subject is required"))?;
        let session_id =
            session_id.ok_or_else(|| Error::turn("turn Session trailer is required"))?;
        validate_trailer_value("subject", &subject)?;
        if let Some(agent_name) = &agent_name {
            validate_trailer_value("agent name", agent_name)?;
        }
        Ok(Self {
            subject,
            session_id,
            agent_name,
            entries,
        })
    }
}

impl Display for Turn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.subject)?;
        f.write_str("\n\n")?;

        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                f.write_str("\n")?;
            }

            match &entry.kind {
                TurnEntryKind::Prompt { text } => {
                    f.write_str("<prompt>")?;
                    f.write_str(&escape_text(text))?;
                    f.write_str("</prompt>")?;
                }
                TurnEntryKind::Response { text } => {
                    f.write_str("<response>")?;
                    f.write_str(&escape_text(text))?;
                    f.write_str("</response>")?;
                }
                TurnEntryKind::ToolCall { kind } => {
                    f.write_str("<tool kind=\"")?;
                    f.write_str(&escape_text(kind.as_str()).replace('"', "&quot;"))?;
                    f.write_str("\" />")?;
                }
            }
        }

        if !self.entries.is_empty() {
            f.write_str("\n\n")?;
        }

        f.write_str(SESSION_TRAILER_PREFIX)?;
        f.write_str(self.session_id.as_ref())?;

        if let Some(agent_name) = &self.agent_name {
            f.write_str("\n")?;
            f.write_str(AGENT_TRAILER_PREFIX)?;
            f.write_str(agent_name)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEntry {
    pub kind: TurnEntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TurnToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    Other,
}

impl TurnToolKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Edit => "edit",
            Self::Delete => "delete",
            Self::Move => "move",
            Self::Search => "search",
            Self::Execute => "execute",
            Self::Think => "think",
            Self::Fetch => "fetch",
            Self::SwitchMode => "switch_mode",
            Self::Other => "other",
        }
    }
}

impl Display for TurnToolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TurnToolKind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "read" => Ok(Self::Read),
            "edit" => Ok(Self::Edit),
            "delete" => Ok(Self::Delete),
            "move" => Ok(Self::Move),
            "search" => Ok(Self::Search),
            "execute" => Ok(Self::Execute),
            "think" => Ok(Self::Think),
            "fetch" => Ok(Self::Fetch),
            "switch_mode" => Ok(Self::SwitchMode),
            "other" => Ok(Self::Other),
            _ => Err(Error::turn(format!("unsupported turn tool kind: {value}"))),
        }
    }
}

impl TurnEntry {
    #[must_use]
    pub fn prompt_now(text: impl Into<String>) -> Self {
        Self {
            kind: TurnEntryKind::Prompt { text: text.into() },
        }
    }

    #[must_use]
    pub fn response_now(text: impl Into<String>) -> Self {
        Self {
            kind: TurnEntryKind::Response { text: text.into() },
        }
    }

    #[must_use]
    pub fn tool_call_now(kind: TurnToolKind) -> Self {
        Self {
            kind: TurnEntryKind::ToolCall { kind },
        }
    }

    fn summary(&self) -> String {
        self.kind.summary()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEntryKind {
    Prompt { text: String },
    Response { text: String },
    ToolCall { kind: TurnToolKind },
}

impl TurnEntryKind {
    fn summary(&self) -> String {
        match self {
            Self::Prompt { text } | Self::Response { text } => {
                text.split_whitespace().collect::<Vec<_>>().join(" ")
            }
            Self::ToolCall { kind } => kind.to_string(),
        }
    }
}

fn parse_entry(pair: Pair<'_, Rule>) -> Result<TurnEntry> {
    match pair.as_rule() {
        Rule::prompt => {
            let text = pair.into_inner().next().map_or("", |text| text.as_str());
            Ok(TurnEntry::prompt_now(unescape_entities(text)?))
        }
        Rule::response => {
            let text = pair.into_inner().next().map_or("", |text| text.as_str());
            Ok(TurnEntry::response_now(unescape_entities(text)?))
        }
        Rule::tool => {
            let kind = pair
                .into_inner()
                .next()
                .map_or("", |kind| kind.as_str())
                .parse()?;
            Ok(TurnEntry::tool_call_now(kind))
        }
        _ => unreachable!("unexpected turn entry rule: {:?}", pair.as_rule()),
    }
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape_entities(value: &str) -> Result<String> {
    let mut decoded = String::with_capacity(value.len());
    let mut remaining = value;

    while let Some((prefix, rest)) = remaining.split_once('&') {
        decoded.push_str(prefix);
        if let Some(rest) = rest.strip_prefix("amp;") {
            decoded.push('&');
            remaining = rest;
        } else if let Some(rest) = rest.strip_prefix("lt;") {
            decoded.push('<');
            remaining = rest;
        } else if let Some(rest) = rest.strip_prefix("gt;") {
            decoded.push('>');
            remaining = rest;
        } else if let Some(rest) = rest.strip_prefix("quot;") {
            decoded.push('"');
            remaining = rest;
        } else {
            return Err(Error::turn("turn body contains invalid escape"));
        }
    }

    decoded.push_str(remaining);
    Ok(decoded)
}

fn validate_trailer_value(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::turn(format!("{label} must not be empty")));
    }
    if value.contains(['\n', '\r']) {
        return Err(Error::turn(format!(
            "{label} cannot contain newline characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    fn session_id(value: &str) -> SessionId {
        value.parse().unwrap()
    }

    #[test]
    fn session_id_parses_bare_value() {
        let session_id: SessionId = "session-a".parse().unwrap();
        assert_eq!(session_id.as_ref(), "session-a");
    }

    #[test]
    fn session_id_rejects_empty_value() {
        let error = "".parse::<SessionId>().unwrap_err();
        assert!(error.to_string().contains("session id must not be empty"));
    }

    #[test]
    fn new_message_defaults_subject_agent_and_entries() {
        let message = Turn::new(session_id("session-a"));

        assert_eq!(message.subject(), "turn");
        assert_eq!(message.session_id().as_ref(), "session-a");
        assert_eq!(message.agent_name(), None);
        assert!(message.entries().is_empty());
    }

    #[test]
    fn message_operations_are_copy_on_write() {
        let base = Turn::new(session_id("session-a"));
        let with_subject = base.clone().with_subject("custom").unwrap();
        let with_agent = with_subject.clone().with_agent_name("Claude").unwrap();
        let with_entry = with_agent
            .clone()
            .with_entry(TurnEntry::prompt_now("hello"));

        assert_eq!(base.subject(), "turn");
        assert_eq!(base.agent_name(), None);
        assert!(base.entries().is_empty());

        assert_eq!(with_subject.subject(), "custom");
        assert_eq!(with_subject.agent_name(), None);
        assert!(with_subject.entries().is_empty());

        assert_eq!(with_agent.subject(), "custom");
        assert_eq!(with_agent.agent_name(), Some("Claude"));
        assert!(with_agent.entries().is_empty());

        assert_eq!(with_entry.subject(), "custom");
        assert_eq!(with_entry.agent_name(), Some("Claude"));
        assert_eq!(with_entry.entries(), &[TurnEntry::prompt_now("hello")]);
    }

    #[test]
    fn display_omits_agent_when_unset() {
        let message = Turn::new(session_id("session-a")).with_entry(TurnEntry::prompt_now("hello"));

        assert_eq!(
            message.to_string(),
            "turn\n\n<prompt>hello</prompt>\n\nSession: session-a"
        );
    }

    #[test]
    fn display_formats_tool_call_entries() {
        let message = Turn::new(session_id("session-a"))
            .with_agent_name("Claude")
            .unwrap()
            .with_entry(TurnEntry::prompt_now("hello"))
            .with_entry(TurnEntry::tool_call_now(TurnToolKind::Read));

        assert_eq!(
            message.to_string(),
            "turn\n\n<prompt>hello</prompt>\n<tool kind=\"read\" />\n\nSession: session-a\nAgent: Claude"
        );
    }

    #[test]
    fn message_round_trip_preserves_entries() {
        let message = Turn::new(session_id("session-a"))
            .with_subject("custom subject")
            .unwrap()
            .with_agent_name("Claude")
            .unwrap()
            .with_entry(TurnEntry::prompt_now("hello"))
            .with_entry(TurnEntry::response_now("done"))
            .with_entry(TurnEntry::tool_call_now(TurnToolKind::Read));

        let parsed: Turn = message.to_string().parse().unwrap();

        assert_eq!(parsed.subject(), "custom subject");
        assert_eq!(parsed.session_id().as_ref(), "session-a");
        assert_eq!(parsed.agent_name(), Some("Claude"));
        assert_eq!(parsed.entries(), message.entries());
    }

    #[test]
    fn suggest_subject_uses_first_non_empty_prompt() {
        let message = Turn::new(session_id("session-a"))
            .with_entry(TurnEntry::prompt_now("  hello \n world  "))
            .with_entry(TurnEntry::response_now("done"));

        assert_eq!(message.suggest_subject().as_deref(), Some("hello world"));
    }

    #[test]
    fn suggest_subject_falls_back_to_response() {
        let message = Turn::new(session_id("session-a"))
            .with_entry(TurnEntry::prompt_now("   "))
            .with_entry(TurnEntry::response_now("done now"));

        assert_eq!(message.suggest_subject().as_deref(), Some("done now"));
    }

    #[test]
    fn suggest_subject_falls_back_to_tool_kind() {
        let message = Turn::new(session_id("session-a"))
            .with_entry(TurnEntry::tool_call_now(TurnToolKind::Execute));

        assert_eq!(message.suggest_subject().as_deref(), Some("execute"));
    }

    #[test]
    fn suggest_subject_returns_none_for_empty_turn() {
        let message = Turn::new(session_id("session-a"));

        assert_eq!(message.suggest_subject(), None);
    }

    #[test]
    fn suggest_subject_truncates_to_readable_summary() {
        let message = Turn::new(session_id("session-a")).with_entry(TurnEntry::prompt_now(
            "this is a deliberately long summary sentence that should truncate cleanly before the sixty character window closes",
        ));

        assert_eq!(
            message.suggest_subject().as_deref(),
            Some("this is a deliberately long summary sentence that should...")
        );
    }

    #[test]
    fn parsed_message_preserves_subject() {
        let parsed: Turn =
            "custom subject\n\n<prompt>hello</prompt>\n\nSession: session-a\nAgent: Claude"
                .parse()
                .unwrap();

        assert_eq!(parsed.subject(), "custom subject");
        assert_eq!(parsed.agent_name(), Some("Claude"));
        assert_eq!(parsed.entries(), &[TurnEntry::prompt_now("hello")]);
    }

    #[test]
    fn parsed_message_allows_missing_agent() {
        let parsed: Turn = "custom subject\n\nSession: session-a".parse().unwrap();

        assert_eq!(parsed.subject(), "custom subject");
        assert_eq!(parsed.session_id().as_ref(), "session-a");
        assert_eq!(parsed.agent_name(), None);
        assert!(parsed.entries().is_empty());
    }

    #[test]
    fn tool_kind_round_trips_through_strings() {
        let kind: TurnToolKind = "read".parse().unwrap();

        assert_eq!(kind, TurnToolKind::Read);
        assert_eq!(kind.to_string(), "read");
    }

    #[test]
    fn tool_kind_rejects_unknown_value() {
        let error = "unknown".parse::<TurnToolKind>().unwrap_err();

        assert!(error.to_string().contains("unsupported turn tool kind"));
    }

    #[test]
    fn parsed_message_rejects_unknown_tool_kind() {
        let error = "turn\n\n<tool kind=\"unknown\" />\n\nSession: session-a"
            .parse::<Turn>()
            .unwrap_err();

        assert!(error.to_string().contains("unsupported turn tool kind"));
    }

    #[test]
    fn with_subject_rejects_newlines() {
        let error = Turn::new(session_id("session-a"))
            .with_subject("bad\nsubject")
            .unwrap_err();

        assert!(error.to_string().contains("subject cannot contain newline"));
    }

    #[test]
    fn with_agent_name_rejects_newlines() {
        let error = Turn::new(session_id("session-a"))
            .with_agent_name("bad\nagent")
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("agent name cannot contain newline")
        );
    }
}
