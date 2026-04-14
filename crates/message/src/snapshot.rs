use std::{fmt::Display, str::FromStr};

use crate::{Error, Result, SessionId};

const SNAPSHOT_SUBJECT: &str = "snapshot";
const SESSION_TRAILER_PREFIX: &str = "Session: ";
const REASON_TRAILER_PREFIX: &str = "Reason: ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum SnapshotReason {
    TurnCommit,
    TurnAmend,
    ToolWrite,
    FilesChanged,
}

impl SnapshotReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TurnCommit => "turn_commit",
            Self::TurnAmend => "turn_amend",
            Self::ToolWrite => "tool_write",
            Self::FilesChanged => "files_changed",
        }
    }
}

impl Display for SnapshotReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SnapshotReason {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "turn_commit" => Ok(Self::TurnCommit),
            "turn_amend" => Ok(Self::TurnAmend),
            "tool_write" => Ok(Self::ToolWrite),
            "files_changed" => Ok(Self::FilesChanged),
            _ => Err(Error::snapshot(format!(
                "unsupported snapshot reason: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct Snapshot {
    session_id: SessionId,
    reason: Option<SnapshotReason>,
}

impl Snapshot {
    #[must_use]
    pub fn new(session_id: SessionId, reason: SnapshotReason) -> Self {
        Self {
            session_id,
            reason: Some(reason),
        }
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    #[must_use]
    pub fn reason(&self) -> Option<SnapshotReason> {
        self.reason
    }
}

impl FromStr for Snapshot {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        let mut session_id = None;
        let mut reason = None;
        for line in input.lines() {
            if let Some(value) = line.strip_prefix(SESSION_TRAILER_PREFIX)
                && session_id.replace(value.parse()?).is_some()
            {
                return Err(Error::snapshot("snapshot has multiple Session trailers"));
            }
            if let Some(value) = line.strip_prefix(REASON_TRAILER_PREFIX)
                && reason.replace(value.parse()?).is_some()
            {
                return Err(Error::snapshot("snapshot has multiple Reason trailers"));
            }
        }
        let session_id =
            session_id.ok_or_else(|| Error::snapshot("snapshot Session trailer is required"))?;
        Ok(Self { session_id, reason })
    }
}

impl Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(SNAPSHOT_SUBJECT)?;
        f.write_str("\n\n")?;
        f.write_str(SESSION_TRAILER_PREFIX)?;
        f.write_str(self.session_id.as_ref())?;
        if let Some(reason) = self.reason {
            f.write_str("\n")?;
            f.write_str(REASON_TRAILER_PREFIX)?;
            f.write_str(reason.as_str())?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    fn session_id(value: &str) -> SessionId {
        value.parse().unwrap()
    }

    #[test]
    fn round_trip_with_reason() {
        let snapshot = Snapshot::new(session_id("session-a"), SnapshotReason::TurnCommit);
        let parsed: Snapshot = snapshot.to_string().parse().unwrap();

        assert_eq!(parsed.session_id().as_ref(), "session-a");
        assert_eq!(parsed.reason(), Some(SnapshotReason::TurnCommit));
    }

    #[test]
    fn parses_without_reason() {
        let parsed: Snapshot = "snapshot\n\nSession: session-a".parse().unwrap();

        assert_eq!(parsed.session_id().as_ref(), "session-a");
        assert_eq!(parsed.reason(), None);
    }

    #[test]
    fn rejects_missing_session_trailer() {
        let error = "snapshot\n\nReason: turn_commit"
            .parse::<Snapshot>()
            .unwrap_err();

        assert!(error.to_string().contains("Session trailer is required"));
    }

    #[test]
    fn rejects_duplicate_session_trailer() {
        let error = "snapshot\n\nSession: a\nSession: b"
            .parse::<Snapshot>()
            .unwrap_err();

        assert!(error.to_string().contains("multiple Session trailers"));
    }

    #[test]
    fn rejects_duplicate_reason_trailer() {
        let error = "snapshot\n\nSession: a\nReason: turn_commit\nReason: turn_amend"
            .parse::<Snapshot>()
            .unwrap_err();

        assert!(error.to_string().contains("multiple Reason trailers"));
    }

    #[test]
    fn reason_round_trips_through_strings() {
        for reason in [
            SnapshotReason::TurnCommit,
            SnapshotReason::TurnAmend,
            SnapshotReason::ToolWrite,
            SnapshotReason::FilesChanged,
        ] {
            let parsed: SnapshotReason = reason.as_str().parse().unwrap();
            assert_eq!(parsed, reason);
        }
    }

    #[test]
    fn reason_rejects_unknown_value() {
        let error = "unknown".parse::<SnapshotReason>().unwrap_err();

        assert!(error.to_string().contains("unsupported snapshot reason"));
    }

    #[test]
    fn display_formats_with_reason() {
        let snapshot = Snapshot::new(session_id("session-a"), SnapshotReason::FilesChanged);

        assert_eq!(
            snapshot.to_string(),
            "snapshot\n\nSession: session-a\nReason: files_changed"
        );
    }
}
