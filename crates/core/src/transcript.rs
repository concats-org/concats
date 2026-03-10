use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::error::{Error, Result};

const CHECKPOINT_SUBJECT_PREFIX: &str = "checkpoint: ";
const EMPTY_CHECKPOINT_SUBJECT: &str = "(empty checkpoint)";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    #[serde(default)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn tool_call_now(name: impl Into<String>, payload: Value) -> Self {
        Self {
            created_at: OffsetDateTime::now_utc(),
            kind: TranscriptEntryKind::ToolCall {
                name: name.into(),
                payload,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptEntryKind {
    Prompt { text: String },
    Response { text: String },
    ToolCall { name: String, payload: Value },
}

impl TranscriptEntryKind {
    #[must_use]
    pub fn summarize(&self) -> String {
        let raw = match self {
            Self::Prompt { text } | Self::Response { text } => text,
            Self::ToolCall { name, .. } => name,
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

pub(crate) fn encode_commit_message(transcript: &Transcript) -> Result<String> {
    let subject = transcript
        .label(Some(EMPTY_CHECKPOINT_SUBJECT.to_string()))
        .unwrap_or_else(|| EMPTY_CHECKPOINT_SUBJECT.to_string());
    let payload = serde_json::to_string_pretty(transcript)
        .map_err(|error| Error::session(format!("failed to serialize checkpoint: {error}")))?;
    Ok(format!("{CHECKPOINT_SUBJECT_PREFIX}{subject}\n\n{payload}"))
}

pub(crate) fn decode_commit_message(message: &str) -> Option<Transcript> {
    if !message.starts_with(CHECKPOINT_SUBJECT_PREFIX) {
        return None;
    }

    let body = message.split_once("\n\n").map_or("", |(_, body)| body);
    let transcript = serde_json::from_str::<Transcript>(body).ok()?;
    transcript.validate().ok()?;
    Some(transcript)
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

        let encoded = encode_commit_message(&transcript).unwrap();
        let decoded = decode_commit_message(&encoded).unwrap();

        assert_eq!(decoded, transcript);
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
