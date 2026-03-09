use std::path::Path;

use crate::{error::Result, git::Oid};

/// Metadata for a single session discovered from git refs.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Session ID (the ACP session identifier, typically a UUID).
    pub id: String,
    /// Human-readable title derived from the first prompt.
    pub title: String,
    /// Human-readable timestamp derived from the tip commit.
    pub timestamp: String,
    /// Unix timestamp of the tip commit (for sorting).
    pub commit_epoch: i64,
    /// Number of finalized turns in the session.
    pub turn_count: u32,
    /// OID of the tip commit on the session ref.
    pub tip_oid: Oid,
}

/// Metadata for a single turn within a session.
#[derive(Debug, Clone)]
pub struct TurnInfo {
    /// Zero-based turn number.
    pub turn_number: u32,
    /// The user prompt for this turn.
    pub prompt: String,
    /// Truncated response summary from the agent.
    pub response_summary: String,
    /// Commit OID for this turn.
    pub commit_oid: Oid,
}

/// Parsed fields from a checkpoint commit message.
struct ParsedCommitMessage {
    prompt: String,
    response_summary: String,
}

/// List all sessions by iterating `refs/agent/sessions/*`.
pub fn list_sessions(repo_path: &Path) -> Result<Vec<SessionInfo>> {
    let repo = git2::Repository::open(repo_path)?;
    let mut sessions = Vec::new();

    let refs = repo.references_glob("refs/agent/sessions/*")?;
    for reference in refs {
        let reference = reference?;
        let ref_name = match reference.name() {
            Some(n) => n.to_string(),
            None => continue,
        };

        let session_id = match ref_name.strip_prefix("refs/agent/sessions/") {
            Some(id) => id.to_string(),
            None => continue,
        };

        let tip = match reference.peel_to_commit() {
            Ok(c) => c,
            Err(_) => continue,
        };

        let tip_oid = Oid::from(tip.id());

        // Count turns and find the first prompt by walking the commit chain.
        let (turn_count, first_prompt) = count_turns_and_first_prompt(&repo, &tip);

        let commit_epoch = tip.time().seconds();
        let timestamp = format_epoch_timestamp(commit_epoch);
        let title = derive_title(&first_prompt);

        sessions.push(SessionInfo {
            id: session_id,
            title,
            timestamp,
            commit_epoch,
            turn_count,
            tip_oid,
        });
    }

    // Sort by commit timestamp in reverse chronological order.
    sessions.sort_by(|a, b| b.commit_epoch.cmp(&a.commit_epoch));

    Ok(sessions)
}

/// Load all turns for a given session by walking the commit chain.
pub fn load_session_turns(repo_path: &Path, session_id: &str) -> Result<Vec<TurnInfo>> {
    let repo = git2::Repository::open(repo_path)?;
    let ref_name = format!("refs/agent/sessions/{session_id}");

    let reference = repo.find_reference(&ref_name)?;
    let tip = reference.peel_to_commit()?;

    let mut commits = Vec::new();
    collect_session_commits(&repo, &tip, session_id, &mut commits);

    // Commits were collected tip-first; reverse to get chronological order.
    commits.reverse();

    let mut turns = Vec::new();
    for commit in &commits {
        let msg = commit.message().unwrap_or("");
        if let Some(parsed) = parse_commit_message(msg)
            && !parsed.response_summary.is_empty()
        {
            let turn_number = turns.len() as u32;
            turns.push(TurnInfo {
                turn_number,
                prompt: parsed.prompt,
                response_summary: parsed.response_summary,
                commit_oid: Oid::from(commit.id()),
            });
        }
    }

    Ok(turns)
}

/// Force-checkout the tree from a specific commit to restore working directory state.
pub fn restore_workdir_to_commit(repo_path: &Path, commit_oid: git2::Oid) -> Result<()> {
    let repo = git2::Repository::open(repo_path)?;
    let commit = repo.find_commit(commit_oid)?;
    let tree = commit.tree()?;

    repo.checkout_tree(
        tree.as_object(),
        Some(git2::build::CheckoutBuilder::new().force()),
    )?;

    Ok(())
}

// ── helpers ────────────────────────────────────────────────────────

/// Walk the commit chain from tip, collecting commits that belong to this session.
///
/// Session identity is determined by the ref (`refs/agent/sessions/{id}`), not
/// by the commit message. We stop walking when we hit a commit that is not a
/// checkpoint (i.e. does not start with `checkpoint:`).
fn collect_session_commits<'r>(
    _repo: &'r git2::Repository,
    commit: &git2::Commit<'r>,
    _session_id: &str,
    out: &mut Vec<git2::Commit<'r>>,
) {
    let msg = commit.message().unwrap_or("");
    if !msg.starts_with("checkpoint:") {
        return;
    }

    out.push(commit.clone());

    // Walk to parent (session commits form a linear chain).
    if commit.parent_count() > 0
        && let Ok(parent) = commit.parent(0)
    {
        collect_session_commits(_repo, &parent, _session_id, out);
    }
}

/// Count finalized turns and extract the first prompt from a session commit chain.
///
/// Returns `(turn_count, first_prompt)`. The first prompt is the prompt from
/// the earliest finalized commit (lowest turn number) belonging to this session.
fn count_turns_and_first_prompt(
    repo: &git2::Repository,
    tip: &git2::Commit<'_>,
) -> (u32, String) {
    let mut count = 0u32;
    let mut first_prompt = String::new();
    let mut current = tip.clone();

    // Walk tip→root, collecting the earliest finalized prompt.
    // Because we walk backwards, every finalized commit overwrites first_prompt
    // so at the end we have the oldest one.
    loop {
        let msg = current.message().unwrap_or("");
        if !msg.starts_with("checkpoint:") {
            break;
        }
        // A finalized checkpoint has a <response> tag.
        if msg.contains("<response>") {
            count += 1;
            if let Some(parsed) = parse_commit_message(msg) {
                first_prompt = parsed.prompt;
            }
        }
        if current.parent_count() == 0 {
            break;
        }
        match current.parent(0) {
            Ok(parent) => current = parent,
            Err(_) => break,
        }
    }

    let _ = repo;
    (count, first_prompt)
}

/// Derive a short human-readable title from the first prompt of a session.
///
/// Rules:
/// 1. Trim whitespace and collapse newlines/runs of whitespace to single spaces.
/// 2. Take up to 60 characters (or up to the first sentence boundary if shorter).
/// 3. Append "…" if truncated.
/// 4. Return "(empty prompt)" if the result is empty.
fn derive_title(prompt: &str) -> String {
    // Collapse whitespace.
    let cleaned: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");

    if cleaned.is_empty() {
        return "(empty prompt)".to_string();
    }

    // Look for a sentence boundary (. ! ?) within the first 60 chars.
    let limit = 60;
    if cleaned.len() <= limit {
        return cleaned;
    }

    let window = &cleaned[..limit];
    // Find the last sentence-ending punctuation followed by a space (or at end).
    if let Some(pos) = window.rfind(['.', '!', '?']) {
        // Only use the sentence boundary if it's not too short (at least 10 chars).
        if pos >= 10 {
            return cleaned[..=pos].to_string();
        }
    }

    // Hard truncate at last word boundary within limit.
    if let Some(pos) = window.rfind(' ') {
        format!("{}…", &cleaned[..pos])
    } else {
        format!("{window}…")
    }
}

/// Parse a checkpoint commit message into its constituent fields.
///
/// Format:
///
/// ```text
/// checkpoint: <subject>
///
/// <prompt>
/// ...
/// </prompt>
/// <response>
/// ...
/// </response>
/// ```
///
/// The `<response>` block is only present in finalized checkpoints.
/// Session identity comes from the ref path, not the message.
/// Turn numbers are derived from commit order, not stored in the message.
fn parse_commit_message(msg: &str) -> Option<ParsedCommitMessage> {
    if !msg.starts_with("checkpoint:") {
        return None;
    }

    let prompt = extract_xml_tag(msg, "prompt").unwrap_or_default();
    let response_summary = extract_xml_tag(msg, "response").unwrap_or_default();

    Some(ParsedCommitMessage {
        prompt,
        response_summary,
    })
}

/// Extract content between `<tag>` and `</tag>`, trimming leading/trailing whitespace.
fn extract_xml_tag(msg: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = msg.find(&open)? + open.len();
    let end = msg.find(&close)?;
    Some(msg[start..end].trim().to_string())
}

/// Format a Unix epoch timestamp into a human-readable `YYYY-MM-DD HH:MM:SS` string.
fn format_epoch_timestamp(epoch_secs: i64) -> String {
    let secs = epoch_secs.unsigned_abs();
    let secs_per_min = 60u64;
    let secs_per_hour = 3600u64;
    let secs_per_day = 86400u64;

    let days = secs / secs_per_day;
    let time_of_day = secs % secs_per_day;
    let hour = time_of_day / secs_per_hour;
    let minute = (time_of_day % secs_per_hour) / secs_per_min;
    let second = time_of_day % secs_per_min;

    let (year, month, day) = days_to_ymd(days);

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from Howard Hinnant's `civil_from_days`.
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m, d)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::fs;

    use super::*;

    fn init_repo_with_commit(dir: &Path) -> git2::Repository {
        let repo = git2::Repository::init(dir).unwrap();
        {
            let mut index = repo.index().unwrap();
            fs::write(dir.join("init.txt"), "init").unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();
            let sig = git2::Signature::now("test", "test@test").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        repo
    }

    fn create_session_commits(
        repo: &git2::Repository,
        dir: &Path,
        session_id: &str,
        num_turns: u32,
    ) {
        let sig = git2::Signature::now("test", "test@test").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let ref_name = format!("refs/agent/sessions/{session_id}");

        let mut parent = head;
        for turn in 0..num_turns {
            let mut index = repo.index().unwrap();
            fs::write(dir.join(format!("turn_{turn}.txt")), format!("turn {turn}")).unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();

            let msg = format!(
                "checkpoint: prompt {turn}\n\n\
                 <prompt>\nprompt {turn}\n</prompt>\n\
                 <response>\nresponse for turn {turn}\n</response>"
            );

            let oid = repo
                .commit(None, &sig, &sig, &msg, &tree, &[&parent])
                .unwrap();
            repo.reference(&ref_name, oid, true, "test").unwrap();
            parent = repo.find_commit(oid).unwrap();
        }
    }

    #[test]
    fn list_sessions_finds_refs() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());

        create_session_commits(&repo, dir.path(), "a1b2c3d4-e5f6-7890-abcd-ef1234567890", 2);
        create_session_commits(&repo, dir.path(), "f9e8d7c6-b5a4-3210-fedc-ba9876543210", 3);

        let sessions = list_sessions(dir.path()).unwrap();
        assert_eq!(sessions.len(), 2);
        // Both sessions exist with correct turn counts (order depends on commit time).
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"a1b2c3d4-e5f6-7890-abcd-ef1234567890"));
        assert!(ids.contains(&"f9e8d7c6-b5a4-3210-fedc-ba9876543210"));
        let session_a = sessions
            .iter()
            .find(|s| s.id == "a1b2c3d4-e5f6-7890-abcd-ef1234567890")
            .unwrap();
        let session_b = sessions
            .iter()
            .find(|s| s.id == "f9e8d7c6-b5a4-3210-fedc-ba9876543210")
            .unwrap();
        assert_eq!(session_a.turn_count, 2);
        assert_eq!(session_b.turn_count, 3);
    }

    #[test]
    fn load_turns_returns_chronological_order() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());

        create_session_commits(&repo, dir.path(), "a1b2c3d4-e5f6-7890-abcd-ef1234567890", 3);

        let turns = load_session_turns(dir.path(), "a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].turn_number, 0);
        assert_eq!(turns[1].turn_number, 1);
        assert_eq!(turns[2].turn_number, 2);
        assert!(turns[0].prompt.contains("prompt 0"));
        assert!(turns[2].response_summary.contains("response for turn 2"));
    }

    #[test]
    fn parse_commit_message_extracts_fields() {
        let msg = "checkpoint: fix the bug\n\n<prompt>\nfix the bug\n</prompt>\n<response>\nI fixed it\n</response>";
        let parsed = parse_commit_message(msg).unwrap();
        assert_eq!(parsed.prompt, "fix the bug");
        assert_eq!(parsed.response_summary, "I fixed it");
    }

    #[test]
    fn derive_title_basic() {
        assert_eq!(derive_title("fix the login bug"), "fix the login bug");
    }

    #[test]
    fn derive_title_empty() {
        assert_eq!(derive_title(""), "(empty prompt)");
        assert_eq!(derive_title("   "), "(empty prompt)");
    }

    #[test]
    fn derive_title_truncation() {
        let long = "a]".repeat(40); // 80 chars
        let title = derive_title(&long);
        assert!(title.len() <= 63); // 60 + "…" (3 bytes)
        assert!(title.ends_with('…'));
    }

    #[test]
    fn derive_title_sentence_boundary() {
        let prompt =
            "Fix the login bug. Then refactor the entire authentication module to use JWT tokens";
        let title = derive_title(prompt);
        assert_eq!(title, "Fix the login bug.");
    }

    #[test]
    fn derive_title_collapses_whitespace() {
        let prompt = "fix\n\nthe\n  login   bug";
        assert_eq!(derive_title(prompt), "fix the login bug");
    }

    #[test]
    fn format_epoch_timestamp_works() {
        // 2026-02-25 10:53:51 UTC = 1772016831 epoch seconds
        assert_eq!(format_epoch_timestamp(1772016831), "2026-02-25 10:53:51");
        // Unix epoch itself.
        assert_eq!(format_epoch_timestamp(0), "1970-01-01 00:00:00");
    }

    #[test]
    fn empty_repo_returns_no_sessions() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let sessions = list_sessions(dir.path()).unwrap();
        assert!(sessions.is_empty());
    }
}
