use time::OffsetDateTime;

use crate::error::{Error, Result};

const EMPTY_CHECKPOINT_SUBJECT: &str = "(empty checkpoint)";

/// Trailer key used to identify the session a checkpoint belongs to.
const SESSION_TRAILER: &str = "Session";
/// Trailer key for the agent that produced the checkpoint.
const AGENT_TRAILER: &str = "Agent";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    entries: Vec<TranscriptEntry>,
}

impl Transcript {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, TranscriptEntry> {
        self.entries.iter()
    }

    /// Append a validated transcript entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the new entry would violate checkpoint transcript
    /// ordering rules.
    pub fn append(&mut self, entry: TranscriptEntry) -> Result<()> {
        if self
            .entries
            .iter()
            .any(|existing| matches!(existing.kind, TranscriptEntryKind::Prompt { .. }))
            && matches!(entry.kind, TranscriptEntryKind::Prompt { .. })
        {
            return Err(Error::session(
                "checkpoint transcript cannot contain more than one prompt entry",
            ));
        }

        if matches!(entry.kind, TranscriptEntryKind::Prompt { .. }) && !self.entries.is_empty() {
            return Err(Error::session(
                "prompt entries must be the first entry in a checkpoint transcript",
            ));
        }

        self.entries.push(entry);
        Ok(())
    }

    #[must_use]
    pub fn label(&self, empty_label: Option<String>) -> Option<String> {
        let summary = self
            .entries
            .iter()
            .map(|entry| entry.kind.summarize())
            .find(|summary| !summary.is_empty());

        let summary = summary.or(empty_label)?;

        if summary.len() <= 60 {
            return Some(summary);
        }

        let window = &summary[..60];
        if let Some(pos) = window.rfind(['.', '!', '?'])
            && pos >= 10
        {
            return Some(summary[..=pos].to_string());
        }

        if let Some(pos) = window.rfind(' ') {
            Some(format!("{}...", &summary[..pos]))
        } else {
            Some(format!("{window}..."))
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let prompt_count = self
            .entries
            .iter()
            .filter(|entry| matches!(entry.kind, TranscriptEntryKind::Prompt { .. }))
            .count();

        if prompt_count > 1 {
            return Err(Error::session(
                "checkpoint transcript cannot contain more than one prompt entry",
            ));
        }

        if self
            .entries
            .iter()
            .skip(1)
            .any(|entry| matches!(entry.kind, TranscriptEntryKind::Prompt { .. }))
        {
            return Err(Error::session(
                "prompt entries must be the first entry in a checkpoint transcript",
            ));
        }

        Ok(())
    }
}

impl<'a> IntoIterator for &'a Transcript {
    type Item = &'a TranscriptEntry;
    type IntoIter = std::slice::Iter<'a, TranscriptEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub created_at: OffsetDateTime,
    pub kind: TranscriptEntryKind,
}

impl TranscriptEntry {
    #[must_use]
    pub fn prompt_now(text: impl Into<String>) -> Self {
        Self {
            created_at: OffsetDateTime::now_utc(),
            kind: TranscriptEntryKind::Prompt { text: text.into() },
        }
    }

    #[must_use]
    pub fn response_now(text: impl Into<String>) -> Self {
        Self {
            created_at: OffsetDateTime::now_utc(),
            kind: TranscriptEntryKind::Response { text: text.into() },
        }
    }

    #[must_use]
    pub fn tool_call_now(kind: impl Into<String>) -> Self {
        Self {
            created_at: OffsetDateTime::now_utc(),
            kind: TranscriptEntryKind::ToolCall { kind: kind.into() },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptEntryKind {
    Prompt { text: String },
    Response { text: String },
    ToolCall { kind: String },
}

impl TranscriptEntryKind {
    #[must_use]
    pub fn summarize(&self) -> String {
        let raw = match self {
            Self::Prompt { text } | Self::Response { text } => text,
            Self::ToolCall { kind, .. } => kind,
        };

        let cleaned = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if cleaned.is_empty() {
            match self {
                Self::ToolCall { .. } => "(tool call)".to_string(),
                _ => String::new(),
            }
        } else {
            cleaned
        }
    }
}

/// Encode a checkpoint transcript into the commit message format specified by
/// the session-storage RFC.
///
/// The format uses XML-ish tags for prompt, response, and tool entries, with
/// Git trailers for session metadata.
pub(crate) fn encode_commit_message(
    transcript: &Transcript,
    session_id: &str,
    agent_name: Option<&str>,
) -> Result<String> {
    use std::fmt::Write;

    transcript.validate()?;

    let subject = transcript
        .label(Some(EMPTY_CHECKPOINT_SUBJECT.to_string()))
        .unwrap_or_else(|| EMPTY_CHECKPOINT_SUBJECT.to_string());

    let mut message = subject;
    message.push_str("\n\n");

    for (i, entry) in transcript.entries.iter().enumerate() {
        if i > 0 {
            message.push('\n');
        }
        match &entry.kind {
            TranscriptEntryKind::Prompt { text } => {
                let _ = write!(message, "<prompt>{text}</prompt>");
            }
            TranscriptEntryKind::Response { text } => {
                let _ = write!(message, "<response>{text}</response>");
            }
            TranscriptEntryKind::ToolCall { kind } => {
                let _ = write!(message, "<tool kind=\"{kind}\" />");
            }
        }
    }

    // Trailers separated by a blank line from the body.
    if !transcript.is_empty() {
        message.push('\n');
    }
    let _ = write!(message, "\n{SESSION_TRAILER}: {session_id}");
    if let Some(agent) = agent_name {
        let _ = write!(message, "\n{AGENT_TRAILER}: {agent}");
    }

    Ok(message)
}

/// Decoded checkpoint metadata extracted from a commit message.
pub(crate) struct DecodedCheckpoint {
    pub transcript: Transcript,
    pub session_id: String,
}

/// Decode a checkpoint commit message in the session-storage RFC format.
///
/// Returns `None` if the message does not contain a valid `Session:` trailer.
pub(crate) fn decode_commit_message(message: &str) -> Option<DecodedCheckpoint> {
    let rest = message.split_once('\n').map_or("", |(_, rest)| rest);
    let session_id = extract_trailer(rest, SESSION_TRAILER)?;
    let body = strip_trailers(rest);
    let transcript = parse_tagged_body(body)?;

    Some(DecodedCheckpoint {
        transcript,
        session_id,
    })
}

/// Parse the tagged body section of a checkpoint commit message into a
/// transcript. Returns `None` if any tag violates ordering rules.
fn parse_tagged_body(body: &str) -> Option<Transcript> {
    let mut transcript = Transcript::new();
    let mut current_tag: Option<(&str, String)> = None;

    for line in body.lines() {
        process_body_line(line, &mut transcript, &mut current_tag).ok()?;
    }

    if let Some((tag, content)) = current_tag.take() {
        flush_tag(&mut transcript, tag, &content).ok()?;
    }

    transcript.validate().ok()?;
    Some(transcript)
}

/// Process a single line of the tagged body, updating the transcript and
/// tracking the currently-open tag.
fn process_body_line<'a>(
    line: &'a str,
    transcript: &mut Transcript,
    current_tag: &mut Option<(&'a str, String)>,
) -> Result<()> {
    if let Some(kind) = parse_tool_tag(line) {
        if let Some((tag, content)) = current_tag.take() {
            flush_tag(transcript, tag, &content)?;
        }
        return transcript.append(TranscriptEntry {
            created_at: OffsetDateTime::UNIX_EPOCH,
            kind: TranscriptEntryKind::ToolCall {
                kind: kind.to_string(),
            },
        });
    }

    if let Some((tag, inline_content)) = parse_open_tag(line) {
        if let Some((prev_tag, content)) = current_tag.take() {
            flush_tag(transcript, prev_tag, &content)?;
        }
        if let Some(text) = try_extract_inline_close(inline_content, tag) {
            flush_tag(transcript, tag, text)?;
        } else {
            *current_tag = Some((tag, inline_content.to_string()));
        }
        return Ok(());
    }

    if let Some(tag) = parse_close_tag(line) {
        if let Some((open_tag, content)) = current_tag.take()
            && open_tag == tag
        {
            flush_tag(transcript, open_tag, &content)?;
        }
        return Ok(());
    }

    if let Some((_, content)) = current_tag {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(line);
    }

    Ok(())
}

fn flush_tag(transcript: &mut Transcript, tag: &str, content: &str) -> Result<()> {
    let entry = match tag {
        "prompt" => TranscriptEntry {
            created_at: OffsetDateTime::UNIX_EPOCH,
            kind: TranscriptEntryKind::Prompt {
                text: content.to_string(),
            },
        },
        "response" => TranscriptEntry {
            created_at: OffsetDateTime::UNIX_EPOCH,
            kind: TranscriptEntryKind::Response {
                text: content.to_string(),
            },
        },
        _ => return Ok(()),
    };
    transcript.append(entry)
}

/// Parse `<tool kind="value" />` and return the kind value.
fn parse_tool_tag(line: &str) -> Option<&str> {
    let line = line.trim();
    let rest = line.strip_prefix("<tool ")?;
    let rest = rest.strip_suffix("/>")?;
    let rest = rest.trim();
    let rest = rest.strip_prefix("kind=\"")?;
    let kind = rest.strip_suffix('"')?;
    Some(kind)
}

/// Parse an opening tag like `<prompt>content` and return the tag name and trailing content.
fn parse_open_tag(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let rest = line.strip_prefix('<')?;
    // Must not start with '/' (that's a closing tag).
    if rest.starts_with('/') {
        return None;
    }
    let end = rest.find('>')?;
    let tag = &rest[..end];
    // Only accept known tags.
    if tag != "prompt" && tag != "response" {
        return None;
    }
    Some((tag, &rest[end + 1..]))
}

/// Try to find `</tag>` within `content` for inline close like `<tag>text</tag>`.
fn try_extract_inline_close<'a>(content: &'a str, tag: &str) -> Option<&'a str> {
    let close = format!("</{tag}>");
    let text = content.strip_suffix(&close)?;
    Some(text)
}

/// Parse `</tag>` and return the tag name.
fn parse_close_tag(line: &str) -> Option<&str> {
    let line = line.trim();
    let rest = line.strip_prefix("</")?;
    let tag = rest.strip_suffix('>')?;
    Some(tag)
}

/// Extract a trailer value from the message body. Trailers are lines at the
/// end of the message matching `Key: Value`.
fn extract_trailer(body: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    for line in body.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
        // Once we hit a non-trailer, non-empty line going backwards, stop if
        // it doesn't look like a trailer (Key: Value pattern).
        if !trimmed.contains(": ") {
            break;
        }
    }
    None
}

/// Strip trailer lines from the end of the body, returning just the tagged content.
fn strip_trailers(body: &str) -> &str {
    let mut end = body.len();
    for line in body.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() || looks_like_trailer(trimmed) {
            end = end.saturating_sub(line.len() + 1); // +1 for newline
        } else {
            break;
        }
    }
    &body[..end]
}

fn looks_like_trailer(line: &str) -> bool {
    // A trailer line matches "Key: Value" where Key has no spaces.
    if let Some((key, _)) = line.split_once(": ") {
        !key.is_empty() && !key.contains(' ')
    } else {
        false
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn label_uses_first_non_empty_summary() {
        let mut transcript = Transcript::new();
        transcript
            .append(TranscriptEntry::prompt_now("hello from the prompt"))
            .unwrap();

        assert_eq!(
            transcript.label(None),
            Some(String::from("hello from the prompt"))
        );
    }

    #[test]
    fn commit_message_round_trip_preserves_transcript() {
        let mut transcript = Transcript::new();
        transcript
            .append(TranscriptEntry::prompt_now("hello"))
            .unwrap();
        transcript
            .append(TranscriptEntry::response_now("done"))
            .unwrap();

        let encoded = encode_commit_message(&transcript, "sess-1", Some("concats")).unwrap();
        let decoded = decode_commit_message(&encoded).unwrap();

        assert_eq!(decoded.session_id, "sess-1");
        assert_eq!(decoded.transcript.len(), 2);
        assert!(matches!(
            decoded.transcript.iter().nth(0).unwrap().kind,
            TranscriptEntryKind::Prompt { ref text } if text == "hello"
        ));
        assert!(matches!(
            decoded.transcript.iter().nth(1).unwrap().kind,
            TranscriptEntryKind::Response { ref text } if text == "done"
        ));
    }

    #[test]
    fn commit_message_with_tool_calls() {
        let mut transcript = Transcript::new();
        transcript
            .append(TranscriptEntry::prompt_now("fix the bug"))
            .unwrap();
        transcript
            .append(TranscriptEntry::tool_call_now("read"))
            .unwrap();
        transcript
            .append(TranscriptEntry::tool_call_now("edit"))
            .unwrap();
        transcript
            .append(TranscriptEntry::response_now("done"))
            .unwrap();

        let encoded = encode_commit_message(&transcript, "sess-2", Some("Claude")).unwrap();
        let decoded = decode_commit_message(&encoded).unwrap();

        assert_eq!(decoded.session_id, "sess-2");
        assert_eq!(decoded.transcript.len(), 4);
        assert!(matches!(
            decoded.transcript.iter().nth(1).unwrap().kind,
            TranscriptEntryKind::ToolCall { ref kind } if kind == "read"
        ));
        assert!(matches!(
            decoded.transcript.iter().nth(2).unwrap().kind,
            TranscriptEntryKind::ToolCall { ref kind } if kind == "edit"
        ));
    }

    #[test]
    fn commit_message_without_agent_name() {
        let mut transcript = Transcript::new();
        transcript
            .append(TranscriptEntry::prompt_now("hello"))
            .unwrap();

        let encoded = encode_commit_message(&transcript, "sess-3", None).unwrap();
        assert!(!encoded.contains("Agent:"));

        let decoded = decode_commit_message(&encoded).unwrap();
        assert_eq!(decoded.session_id, "sess-3");
    }

    #[test]
    fn decode_rejects_missing_session_trailer() {
        let message = "some subject\n\n<prompt>hello</prompt>";
        assert!(decode_commit_message(message).is_none());
    }

    #[test]
    fn transcript_rejects_second_prompt() {
        let mut transcript = Transcript::new();
        transcript
            .append(TranscriptEntry::prompt_now("hello"))
            .unwrap();

        let error = transcript
            .append(TranscriptEntry::prompt_now("again"))
            .unwrap_err();

        assert!(error.to_string().contains("more than one prompt"));
    }
}
