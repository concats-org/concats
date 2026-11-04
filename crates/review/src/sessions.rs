//! concats sessions, read natively — the "why" column of a review.
//!
//! The capture layer (this repo's own CLI) records agent transcripts into the
//! same git repository as the code (rfcs/session_storage.md):
//!
//! ```text
//! refs/agent/sessions/<id>    turn commits: EMPTY tree, transcript in the
//!                             commit message. parents[0] = previous turn (or
//!                             the commit the session started on), parents[1] =
//!                             branch HEAD at write time, when different.
//! ```
//!
//! Object access goes through gix (pure Rust). The turn grammar is parsed by
//! concats_message, the same parser the capture layer writes with, so the wire
//! format cannot drift between writer and reader.
//!
//! A turn is linked to a range commit when its parents[1] is that commit: it
//! was recorded with the commit checked out. No worktree snapshots are kept,
//! so what a turn changed can't be matched to a commit's content.
//!
//! One caveat: `refs/agent/*` are not fetched by default refspecs, so sessions
//! are only visible where they were recorded or explicitly fetched.

use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
};

use concats_diff::{
    LoadStats, Row,
    load::{self, Change, FileLowerer, Loaded, Rev},
};
use concats_message::{SESSION_REF_PREFIX, TurnEntryKind};
use gix::{ObjectId, Repository};

/// Guard against garbage refs; a session is a conversation, not a history.
const MAX_CHAIN: usize = 10_000;

/// How a transcript's prose blobs are named: a session's header, and each
/// turn's prompts and responses under it.
const SESSION_PROSE: &str = "session/";
const TURN_PROSE: &str = "turn/";

/// Whether the prose blob `name` is transcript, Markdown as the prompter and
/// the agent wrote it. A commit message is not: it can be anyone's, and few
/// are written as Markdown.
pub fn is_transcript(name: &str) -> bool {
    name.starts_with(SESSION_PROSE) || name.starts_with(TURN_PROSE)
}

pub fn worktree_fingerprint(repo: &Repository) -> Result<u64, concats_diff::Error> {
    let mut fingerprint = DefaultHasher::new();
    concats_diff::stage::worktree_fingerprint(repo)?.hash(&mut fingerprint);
    let platform = repo
        .references()
        .map_err(|error| concats_diff::Error::git("session refs", error))?;
    let mut refs = Vec::new();
    let iter = platform
        .prefixed(SESSION_REF_PREFIX)
        .map_err(|error| concats_diff::Error::git("session refs", error))?;
    for reference in iter {
        let reference = reference.map_err(|source| concats_diff::Error::Git {
            op: "session ref",
            source,
        })?;
        refs.push((
            reference.name().as_bstr().to_string(),
            reference.try_id().map(gix::Id::detach),
        ));
    }
    refs.sort();
    refs.hash(&mut fingerprint);
    Ok(fingerprint.finish())
}

/// A parsed turn.
pub struct Turn {
    pub session_id: String,
    pub oid: ObjectId,
    pub message: concats_message::Turn,
    /// parents[1]: branch HEAD when the turn was recorded, when different.
    pub branch_parent: Option<ObjectId>,
}

/// One commit of the range: identity, message, and its own diff against the
/// first parent, with the before/after oids the `FileLowerer` needs.
struct CommitDiff {
    oid: ObjectId,
    /// First line of the message.
    subject: String,
    /// The rest of the message, trimmed. Empty when subject-only.
    body: String,
    /// More than one = a merge; its diff is still against parents[0].
    parent_count: usize,
    changes: Vec<Change>,
}

/// The loaded range, prepared once: its commit set and each commit's diff.
/// Turns get linked against this, and the Commits tab and the sessions'
/// per-commit diffs render from it.
pub struct RangeDiff {
    commit_set: HashSet<ObjectId>,
    /// Head-first, as walked from `head` — reverse for display order.
    by_commit: Vec<CommitDiff>,
}

pub fn review_diff(repo: &Repository, loaded: &Loaded) -> RangeDiff {
    match (loaded.base, loaded.head) {
        (Rev::Commit(base), Rev::Commit(head)) => range_diff(repo, &base, &head),
        // NOTE: a WORKTREE load has no head commit, so no commit of its own to
        // link a turn to.
        _ => RangeDiff {
            commit_set: HashSet::new(),
            by_commit: Vec::new(),
        },
    }
}

/// The turns recorded with a range commit checked out.
pub fn matching_turns(range: &RangeDiff, turns: &[Turn]) -> HashSet<usize> {
    turns
        .iter()
        .enumerate()
        .filter(|(_, turn)| {
            turn.branch_parent
                .is_some_and(|oid| range.commit_set.contains(&oid))
        })
        .map(|(i, _)| i)
        .collect()
}

fn range_diff(repo: &Repository, merge_base: &ObjectId, head: &ObjectId) -> RangeDiff {
    let commits = range_commits(repo, merge_base, head);
    let mut by_commit = Vec::new();
    for (c, commit) in &commits {
        let c = *c;
        let Some(parent) = commit.parents.first().copied() else {
            continue;
        };
        let Ok(changes) = load::diff_commits(repo, parent, c) else {
            continue;
        };
        // Gix hands the message over as one string: the subject is its first
        // line, the body the rest, both trimmed.
        let message = commit.message.trim();
        let (subject, body) = message.split_once('\n').unwrap_or((message, ""));
        by_commit.push(CommitDiff {
            oid: c,
            subject: subject.trim().to_string(),
            body: body.trim().to_string(),
            parent_count: commit.parents.len(),
            changes,
        });
    }
    RangeDiff {
        commit_set: commits.into_iter().map(|(oid, _)| oid).collect(),
        by_commit,
    }
}

/// One commit, as the mining pass needs it: its message and parents.
struct Commit {
    message: String,
    parents: Vec<ObjectId>,
}

fn read_commit(repo: &Repository, oid: &ObjectId) -> Result<Commit, ()> {
    let commit = repo.find_commit(*oid).map_err(|_| ())?;
    let decoded = commit.decode().map_err(|_| ())?;
    Ok(Commit {
        message: decoded.message.to_string(),
        parents: decoded.parents().collect(),
    })
}

/// Every turn of every session in the repo, oldest first within a session.
///
/// The walk follows parents[0] from each session ref tip and stops at the first
/// commit whose message is not a turn of that session: the ordinary commit the
/// session was started on.
// NOTE: two production callers, mine and `concats turns`; the rest of its fan-in is tests.
// coland-ignore: @coland/high_fan_in_complex_function
pub fn load_turns(repo: &Repository) -> Vec<Turn> {
    let refs: Vec<(String, ObjectId)> = repo
        .references()
        .ok()
        .and_then(|platform| {
            let iter = platform.prefixed(SESSION_REF_PREFIX).ok()?;
            Some(
                iter.filter_map(|r| {
                    let r = r.ok()?;
                    let tip = r.try_id()?.detach();
                    Some((r.name().as_bstr().to_string(), tip))
                })
                .collect(),
            )
        })
        .unwrap_or_default();
    let mut turns = Vec::new();
    for (name, tip) in refs {
        let session_id = name[SESSION_REF_PREFIX.len()..].to_string();
        let mut chain = Vec::new();
        let mut oid = tip;
        for _ in 0..MAX_CHAIN {
            let Ok(c) = read_commit(repo, &oid) else {
                break;
            };
            let Ok(message) = c.message.parse::<concats_message::Turn>() else {
                break;
            };
            if message.session_id().as_str() != session_id {
                break;
            }
            let parent = c.parents.first().copied();
            chain.push(Turn {
                session_id: session_id.clone(),
                oid,
                message,
                branch_parent: c.parents.get(1).copied(),
            });
            match parent {
                Some(p) => oid = p,
                None => break,
            }
        }
        // Walked tip -> back; exposed oldest-first.
        turns.extend(chain.into_iter().rev());
    }
    turns
}

/// Header rows for one commit: the subject, then the body (when there is one)
/// quoted line by line. Commit messages are not markdown, and a stray `#` or
/// list marker must not restructure the document. Inside a blockquote it can't
/// — the same guard turn prompts get.
///
/// Both are prose lines, so the message reads, selects and comments like the
/// code under it. Neither carries a heading level: the same commit renders
/// under the Commits tab and under the turn that wrote it, and content that
/// differed between the two would carry two conversations instead of one.
fn commit_header_rows(rows: &mut Vec<Row>, cd: &CommitDiff, low: &mut FileLowerer) {
    let oid = cd.oid.to_string();
    let sha = &oid[..10];
    rows.extend(low.prose(
        format!("commit/{sha}/subject"),
        format!("**{}**  `{sha}`\n", cd.subject),
    ));
    if cd.parent_count > 1 {
        rows.push(Row::Prose {
            md: format!(
                "_merge commit ({} parents) — changes shown against the first parent_",
                cd.parent_count
            ),
        });
    }
    if !cd.body.is_empty() {
        let mut md = String::new();
        for line in cd.body.lines() {
            md.push_str("> ");
            md.push_str(&escape_angle_outside_code(line));
            md.push('\n');
        }
        rows.extend(low.prose(format!("commit/{sha}/body"), md));
    }
}

/// `<…>` is an autolink/HTML tag to the markdown renderer, so a literal
/// `Co-Authored-By: X <mail>` trailer would render as a link widget. Escape
/// it — but not inside `inline code`, where backslashes stay visible and
/// angle brackets are already literal.
fn escape_angle_outside_code(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 4);
    let mut in_code = false;
    for ch in line.chars() {
        match ch {
            '`' => {
                in_code = !in_code;
                out.push(ch);
            }
            '\\' | '<' if !in_code => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Lower one commit's changes as file cards, into the shared blob table.
fn commit_change_rows(
    rows: &mut Vec<Row>,
    changes: &[Change],
    id_prefix: &str,
    low: &mut FileLowerer,
    blob_paths: &mut HashMap<u32, String>,
) {
    let mut shown = 0usize;
    let mut binary = 0usize;
    for (j, c) in changes.iter().enumerate() {
        match low.file(format!("{id_prefix}-c{j}"), c) {
            Ok(Some(fc)) => {
                for h in &fc.hunks {
                    for r in &h.rows {
                        if let Row::Code { blob, .. } = r {
                            blob_paths.entry(*blob).or_insert_with(|| fc.path.clone());
                        }
                    }
                }
                rows.push(fc.header());
                rows.extend(fc.default_rows());
                shown += 1;
            }
            Ok(None) => binary += 1,
            Err(_) => {}
        }
    }
    if shown == 0 && binary == 0 {
        rows.push(Row::Prose {
            md: "_no reviewable changes in this commit_".into(),
        });
    }
    if binary > 0 {
        rows.push(Row::Prose {
            md: format!("_… and {binary} binary file(s), not shown_"),
        });
    }
}

/// turn -> the indices into `range.by_commit` of the commits it is linked to:
/// the one it was recorded with checked out (parents[1]).
fn commits_by_turn(range: &RangeDiff, turns: &[Turn]) -> HashMap<usize, Vec<usize>> {
    let mut out: HashMap<usize, Vec<usize>> = HashMap::new();
    for (ci, cd) in range.by_commit.iter().enumerate().rev() {
        for (i, t) in turns.iter().enumerate() {
            if t.branch_parent == Some(cd.oid) {
                out.entry(i).or_default().push(ci);
            }
        }
    }
    out
}

/// Commits reachable from `head`, stopping at `merge_base`. This is the
/// pragmatic review-range set — a side branch merged below the merge base can
/// slip extra commits in, which is acceptable for attributing transcripts.
fn range_commits(
    repo: &Repository,
    merge_base: &ObjectId,
    head: &ObjectId,
) -> Vec<(ObjectId, Commit)> {
    let mut out = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack = vec![*head];
    while let Some(oid) = stack.pop() {
        if oid == *merge_base || !seen.insert(oid) {
            continue;
        }
        let Ok(c) = read_commit(repo, &oid) else {
            continue;
        };
        stack.extend(c.parents.iter().copied());
        out.push((oid, c));
        if out.len() >= MAX_CHAIN {
            break;
        }
    }
    out
}

/// What the review document gets out of all this.
#[derive(Default)]
pub struct Mined {
    /// The Sessions tab: per linked session, the transcript of every one of
    /// its turns interleaved with the per-commit diffs those turns produced.
    pub sessions: Vec<Row>,
    /// The Commits tab: the range organized by commit, oldest first, and on a
    /// WORKTREE review the uncommitted changes last. Empty (no tab) when the
    /// range has no commit.
    pub commits: Vec<Row>,
    /// blob index -> path for blobs the session/commit lowering added (the
    /// review's own files are mapped by the caller).
    pub blob_paths: HashMap<u32, String>,
}

/// Mine the repo's recorded sessions for the loaded range. Cheap when there are
/// none (one refs directory listing), so the GUI just always calls it. Takes
/// `loaded` mutably because the sessions tab lowers each turn's own diff into
/// the same blob table the review uses.
// NOTE: the pass's entry: the app's load is its one production caller, the rest tests.
// coland-ignore: @coland/high_call_degree
pub fn mine(loaded: &mut Loaded) -> Mined {
    let Ok(repo) = load::open_repo(&loaded.root) else {
        return Mined::default();
    };
    let range = review_diff(&repo, loaded);

    let mut mined = Mined::default();
    // The Commits tab is independent of sessions — build it before the
    // no-turns bail-out. A WORKTREE review has no head commit of its own, so
    // its tab lists HEAD's commits since the base, then what is uncommitted.
    let worktree = (loaded.head == Rev::Worktree)
        .then(|| worktree_commits(&repo, loaded))
        .flatten();
    let commits = worktree.as_ref().map_or(&range, |(commits, _)| commits);
    if !commits.by_commit.is_empty() {
        mined.commits = commits_stream(&repo, commits, loaded, &mut mined.blob_paths);
        if let Some((_, head)) = &worktree {
            let uncommitted = uncommitted_rows(&repo, *head, loaded, &mut mined.blob_paths);
            mined.commits.extend(uncommitted);
        }
    }

    let turns = load_turns(&repo);
    if turns.is_empty() {
        return mined;
    }
    // A WORKTREE review links turns to the same commits its Commits tab
    // lists: HEAD's since the base.
    let via = matching_turns(commits, &turns);
    let turn_commits = commits_by_turn(commits, &turns);

    mined.sessions = sessions_rows(
        &repo,
        commits,
        &turns,
        &via,
        &turn_commits,
        loaded,
        &mut mined.blob_paths,
    );
    mined
}

/// The Commits tab: the loaded range organized by commit, oldest first — the
/// story read forward. Each commit is its message header followed by its own
/// diff against the first parent, lowered through the shared blob table like
/// every other view (same lazy highlighting, same content-addressed review
/// state). Hunk ids get their own `c` namespace, so they cannot collide with
/// the manifest's (`h`) or the sessions' (`t`).
fn commits_stream(
    repo: &Repository,
    range: &RangeDiff,
    loaded: &mut Loaded,
    blob_paths: &mut HashMap<u32, String>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    rows.push(Row::Title {
        text: "# Commits".into(),
    });
    rows.push(Row::Prose {
        md: match range.by_commit.len() {
            1 => "**1 commit** in this range.".to_string(),
            n => format!("**{n} commits** in this range, oldest first."),
        },
    });

    let mut scratch = LoadStats::default();
    let mut low = FileLowerer::new((repo, None), &mut loaded.blobs, "c", &mut scratch);

    for cd in range.by_commit.iter().rev() {
        commit_header_rows(&mut rows, cd, &mut low);
        let sha = cd.oid.to_string();
        commit_change_rows(&mut rows, &cd.changes, &sha[..10], &mut low, blob_paths);
    }
    rows
}

/// A `<commit>...WORKTREE` review's commits: HEAD's history back to where it
/// meets the base, and HEAD, which the uncommitted changes sit on. `None` for
/// `INDEX...WORKTREE`.
fn worktree_commits(repo: &Repository, loaded: &Loaded) -> Option<(RangeDiff, ObjectId)> {
    let Rev::Commit(merge_base) = loaded.base else {
        return None;
    };
    let head = repo.head_id().ok()?.detach();
    Some((range_diff(repo, &merge_base, &head), head))
}

/// The Commits tab's last entry on a WORKTREE review: the working tree
/// against `head`, as one more commit-shaped header and its file cards. No
/// rows when nothing is uncommitted. Hunk ids get a `u` namespace of their own.
fn uncommitted_rows(
    repo: &Repository,
    head: ObjectId,
    loaded: &mut Loaded,
    blob_paths: &mut HashMap<u32, String>,
) -> Vec<Row> {
    let (changes, overlay) = match load::uncommitted(repo, loaded, head) {
        Ok(uncommitted) => uncommitted,
        Err(error) => {
            // NOTE: the commits above still stand; a failed worktree read only
            // costs the last entry.
            eprintln!("concats: uncommitted changes not shown: {error}");
            return Vec::new();
        }
    };
    if changes.is_empty() {
        return Vec::new();
    }
    let sha = head.to_string();
    let mut scratch = LoadStats::default();
    let mut low = FileLowerer::new((repo, Some(&overlay)), &mut loaded.blobs, "u", &mut scratch);
    let mut rows = low.prose(
        format!("uncommitted/{}", &sha[..10]),
        format!("**Uncommitted**  on top of `{}`\n", &sha[..10]),
    );
    commit_change_rows(&mut rows, &changes, "uncommitted", &mut low, blob_paths);
    rows
}

/// The Sessions document: the same row vocabulary the review uses, so the
/// renderer does not change. A session is linked when any one of its turns is,
/// and it then renders whole: every turn's transcript (prompt as blockquote,
/// response as markdown, tool calls summarized), each followed by the commits
/// that turn produced — the commit's message and its own diff against its
/// first parent, lowered through the same `FileLowerer` the review uses, into
/// the same blob table. A commit several turns produced together renders under
/// the first and is cross-referenced after.
///
/// Whole, because the session is the unit of "why". A turn that produced
/// nothing of this diff is still what led to the turn that did — collapsing
/// those to a count threw the reasoning away and left the transcript starting
/// mid-thought.
// NOTE: the pass's state, passed explicitly; AGENTS.md rules out a one-call-site carrier.
// coland-ignore: @coland/excessive_positional_parameters
fn sessions_rows(
    repo: &Repository,
    range: &RangeDiff,
    turns: &[Turn],
    via: &HashSet<usize>,
    turn_commits: &HashMap<usize, Vec<usize>>,
    loaded: &mut Loaded,
    blob_paths: &mut HashMap<u32, String>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    if via.is_empty() {
        return rows;
    }

    // Session lowering must not skew the review's perf numbers.
    let mut scratch = LoadStats::default();
    // Resume interning into the review's blob table: identical content (the
    // usual case, a turn's final state is what got committed) costs nothing.
    let mut low = FileLowerer::new((repo, None), &mut loaded.blobs, "t", &mut scratch);

    let linked_sessions: HashSet<&str> =
        via.iter().map(|&i| turns[i].session_id.as_str()).collect();
    let all_sessions: HashSet<&str> = turns.iter().map(|t| t.session_id.as_str()).collect();

    rows.push(Row::Title {
        text: "# Sessions".into(),
    });
    let rendered = turns
        .iter()
        .filter(|t| linked_sessions.contains(t.session_id.as_str()))
        .count();
    rows.push(Row::Prose {
        md: format!(
            "The transcripts behind this diff: **{} session(s)** linked to it, each \
             whole — **{} turn(s)**, **{}** of them linked.{}{}",
            linked_sessions.len(),
            rendered,
            via.len(),
            " Each linked turn is followed by the commit(s) it was recorded on.",
            match all_sessions.len() - linked_sessions.len() {
                0 => String::new(),
                n => format!(" {n} other recorded session(s) have no turns linked here."),
            }
        ),
    });

    let mut session = "";
    let mut rendered_commits: HashSet<ObjectId> = HashSet::new();
    for (i, t) in turns.iter().enumerate() {
        if !linked_sessions.contains(t.session_id.as_str()) {
            continue;
        }
        if t.session_id != session {
            session = &t.session_id;
            rows.extend(low.prose(
                format!("{SESSION_PROSE}{session}"),
                format!("## Session `{session}`\n"),
            ));
            if let Some(agent) = t.message.agent_name() {
                rows.push(Row::Prose {
                    md: format!("_recorded by **{agent}**_"),
                });
            }
        }
        let commits = turn_commits.get(&i).map(Vec::as_slice).unwrap_or(&[]);
        turn_rows(
            &mut rows,
            t,
            if via.contains(&i) {
                "recorded at commit"
            } else {
                "not linked to this diff"
            },
            commits,
            range,
            &mut rendered_commits,
            &mut low,
            blob_paths,
        );
    }
    rows
}

/// One turn: header, transcript entries in order, then the commit(s) the turn
/// produced, each as its message header plus its diff against the first parent
/// — exactly what the Commits tab shows for it. A commit already rendered under
/// an earlier turn collapses to a cross-reference line.
#[allow(clippy::too_many_arguments)]
// NOTE: the pass's state, passed explicitly; AGENTS.md rules out a one-call-site carrier.
// coland-ignore: @coland/excessive_positional_parameters
fn turn_rows(
    rows: &mut Vec<Row>,
    t: &Turn,
    link: &str,
    commits: &[usize],
    range: &RangeDiff,
    rendered_commits: &mut HashSet<ObjectId>,
    low: &mut FileLowerer,
    blob_paths: &mut HashMap<u32, String>,
) {
    // What a comment on this turn's content records. The turn's own oid names
    // it: a turn belongs to one session, and its commit is what the content is
    // recognized by.
    let turn = format!("{TURN_PROSE}{}", &t.oid.to_string()[..10]);
    rows.push(Row::Title {
        text: format!("### {}", t.message.subject()),
    });
    rows.push(Row::Prose {
        md: format!("_turn `{}` · {link}_", &t.oid.to_string()[..10]),
    });

    for (i, e) in t.message.entries().iter().enumerate() {
        match &e.kind {
            TurnEntryKind::Prompt { text } => {
                let mut md = String::from("**Prompt**\n\n");
                for line in text.trim().lines() {
                    md.push_str("> ");
                    md.push_str(line);
                    md.push('\n');
                }
                rows.extend(low.prose(format!("{turn}/prompt/{i}"), md));
            }
            TurnEntryKind::Response { text } => {
                rows.extend(low.prose(
                    format!("{turn}/response/{i}"),
                    format!("**Response**\n\n{}\n", text.trim()),
                ));
            }
        }
    }

    // The commit(s) this turn produced, oldest first.
    for &ci in commits {
        let cd = &range.by_commit[ci];
        if !rendered_commits.insert(cd.oid) {
            rows.push(Row::Prose {
                md: format!(
                    "_also produced commit `{}` — shown under an earlier turn_",
                    &cd.oid.to_string()[..10]
                ),
            });
            continue;
        }
        commit_header_rows(rows, cd, low);
        let sha = cd.oid.to_string();
        commit_change_rows(
            rows,
            &cd.changes,
            &format!("{}-{}", &t.oid.to_string()[..10], &sha[..10]),
            low,
            blob_paths,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use concats_fixture::git;
    use concats_message::{SessionId, TurnEntry};

    use super::*;

    /// What a row stream reads as: the lines of its prose blobs, in order.
    /// The content assertions below are about what a reviewer sees, and a
    /// transcript entry is one blob behind many line rows.
    fn prose(rows: &[Row], blobs: &[concats_diff::Blob]) -> String {
        rows.iter()
            .filter_map(|r| match r {
                Row::Code { blob, line, .. } => {
                    let b = &blobs[*blob as usize];
                    b.prose.is_some().then(|| b.line_text(*line as usize))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// git's well-known empty tree oid — turn commits carry no content.
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    /// Build the turn message with the real grammar's writer, so the test can
    /// never diverge from what the capture layer actually commits.
    fn turn_message(subject: &str, prompt: &str, response: &str) -> String {
        concats_message::Turn::new(SessionId::from_str("test-session").unwrap())
            .with_subject(subject)
            .unwrap()
            .with_entry(TurnEntry::prompt_now(prompt))
            .with_entry(TurnEntry::response_now(response))
            .to_string()
    }

    /// A repo with `a.txt` committed on `main`, and that commit.
    fn base_repo() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "base"]);
        let base = git(dir, &["rev-parse", "HEAD"]);
        (tmp, base)
    }

    fn oid(hex: &str) -> ObjectId {
        ObjectId::from_hex(hex.as_bytes()).unwrap()
    }

    #[test]
    fn reads_and_links_a_synthesized_session() {
        // An ordinary history: base, then one commit changing a.txt.
        let (tmp, base) = base_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "feature"]);
        let head = git(dir, &["rev-parse", "HEAD"]);

        // A turn recorded with that commit checked out: empty tree, message =
        // the transcript, parents = the session's base and the branch HEAD.
        let msg = turn_message("Add two", "please add two", "added two");
        let turn = git(
            dir,
            &[
                "commit-tree",
                EMPTY_TREE,
                "-p",
                &base,
                "-p",
                &head,
                "-m",
                &msg,
            ],
        );
        git(
            dir,
            &["update-ref", "refs/agent/sessions/test-session", &turn],
        );

        let repo = gix::open(dir).unwrap();
        let mb = oid(&base);
        let hd = oid(&head);
        let range = range_diff(&repo, &mb, &hd);
        let turns = load_turns(&repo);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].message.subject(), "Add two");
        assert_eq!(turns[0].session_id, "test-session");

        // Recorded at the feature commit, so linked to it.
        assert_eq!(range.by_commit[0].oid, hd);
        assert_eq!(
            commits_by_turn(&range, &turns),
            HashMap::from([(0, vec![0])])
        );
        assert_eq!(matching_turns(&range, &turns), HashSet::from([0]));

        // The full mining pass: a Sessions document holding the transcript
        // followed by the produced diff rows.
        let mut loaded = load::load(dir, &base, &head).unwrap();
        // Every hunk leads with its HunkBar — the seen tick box's anchor —
        // in every view, because it is part of the hunk's own rows.
        for f in &loaded.files {
            for h in &f.hunks {
                assert!(matches!(h.rows.first(), Some(Row::HunkBar { .. })));
            }
        }
        let mined = mine(&mut loaded);
        let sess = &mined.sessions;
        let read = prose(sess, &loaded.blobs);
        assert!(read.contains("test-session"));
        assert!(read.contains("please add two"));
        // The turn's diff is the commit it produced: message header (subject +
        // short sha), then that commit's own diff.
        assert!(read.contains("feature") && read.contains(&head[..10]));
        assert!(
            sess.iter()
                .any(|r| matches!(r, Row::FileHeader { path, .. } if path == "a.txt"))
        );
        assert!(sess.iter().any(|r| matches!(r, Row::Code { .. })));
        // One commit still gets its Commits tab, though it repeats the File Diff.
        assert!(prose(&mined.commits, &loaded.blobs).contains("feature"));
        // The transcript is Markdown to the app; the commit's message is not.
        let transcript: Vec<&str> = loaded
            .blobs
            .iter()
            .filter(|b| b.prose.as_deref().is_some_and(is_transcript))
            .map(|b| b.text.as_str())
            .collect();
        assert!(transcript.iter().any(|t| t.contains("test-session")));
        assert!(transcript.iter().any(|t| t.contains("please add two")));
        assert!(!transcript.iter().any(|t| t.contains("feature")));
    }

    #[test]
    fn commits_stream_organizes_the_range_by_commit_oldest_first() {
        let (tmp, base) = base_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "add two"]);
        std::fs::write(dir.join("b.txt"), "hi\n").unwrap();
        git(dir, &["add", "."]);
        git(
            dir,
            &[
                "commit",
                "-qm",
                "add b\n\nwhy b exists\nsee `Option<T>`\n\nCo-Authored-By: X <x@y.z>",
            ],
        );
        let head = git(dir, &["rev-parse", "HEAD"]);

        let mut loaded = load::load(dir, &base, &head).unwrap();
        let mined = mine(&mut loaded);

        assert!(matches!(
            mined.commits.first(),
            Some(Row::Title { text }) if text == "# Commits"
        ));
        let read = prose(&mined.commits, &loaded.blobs);
        // Oldest first: "add two" before "add b".
        let two = read.find("add two").unwrap();
        let b = read.find("add b").unwrap();
        assert!(two < b, "commits must render oldest first");
        // The body renders blockquoted line by line, with autolink-able
        // angle brackets escaped — except inside inline code.
        assert!(read.contains("> why b exists\n> see `Option<T>`"));
        assert!(read.contains("> Co-Authored-By: X \\<x@y.z>"));
        // Each commit carries its own file diff.
        let headers: Vec<&String> = mined
            .commits
            .iter()
            .filter_map(|r| match r {
                Row::FileHeader { path, .. } => Some(path),
                _ => None,
            })
            .collect();
        assert_eq!(headers, [&"a.txt".to_string(), &"b.txt".to_string()]);
        assert!(mined.commits.iter().any(|r| matches!(r, Row::Code { .. })));
    }

    #[test]
    fn a_worktree_review_lists_heads_commits_then_what_is_uncommitted() {
        let (tmp, base) = base_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "add two"]);
        std::fs::write(dir.join("b.txt"), "hi\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "add b"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let mut loaded = load::load(dir, &base, "WORKTREE").unwrap();
        let mined = mine(&mut loaded);
        let read = prose(&mined.commits, &loaded.blobs);
        let two = read.find("add two").unwrap();
        let b = read.find("add b").unwrap();
        let uncommitted = read.find("Uncommitted").unwrap();
        assert!(
            two < b && b < uncommitted,
            "commits oldest first, then the worktree"
        );
        let headers: Vec<&str> = mined
            .commits
            .iter()
            .filter_map(|r| match r {
                Row::FileHeader { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headers, ["a.txt", "b.txt", "a.txt"]);

        // No commit since the base: no tab, however much is uncommitted.
        for base in ["HEAD", "INDEX"] {
            let mut loaded = load::load(dir, base, "WORKTREE").unwrap();
            assert!(mine(&mut loaded).commits.is_empty(), "{base}...WORKTREE");
        }
    }

    #[test]
    fn a_worktree_review_links_turns_recorded_on_heads_commits() {
        let (tmp, base) = base_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(dir, &["commit", "-qam", "feature"]);
        let head = git(dir, &["rev-parse", "HEAD"]);
        let msg = turn_message("Add two", "please add two", "added two");
        let turn = git(
            dir,
            &[
                "commit-tree",
                EMPTY_TREE,
                "-p",
                &base,
                "-p",
                &head,
                "-m",
                &msg,
            ],
        );
        git(
            dir,
            &["update-ref", "refs/agent/sessions/test-session", &turn],
        );
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let mut loaded = load::load(dir, &base, "WORKTREE").unwrap();
        let sessions = mine(&mut loaded).sessions;
        assert!(prose(&sessions, &loaded.blobs).contains("please add two"));
    }

    #[test]
    fn renders_every_turn_of_a_linked_session() {
        let (tmp, base) = base_repo();
        let dir = tmp.path();

        // The range's one commit.
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "feature"]);
        let head = git(dir, &["rev-parse", "HEAD"]);

        // Turn 1 was recorded on the base, turn 2 with the commit checked out.
        let msg1 = turn_message("Take notes", "think it through", "noted");
        let t1 = git(dir, &["commit-tree", EMPTY_TREE, "-p", &base, "-m", &msg1]);
        let msg2 = turn_message("Add two", "now add two", "added two");
        let t2 = git(
            dir,
            &[
                "commit-tree",
                EMPTY_TREE,
                "-p",
                &t1,
                "-p",
                &head,
                "-m",
                &msg2,
            ],
        );
        git(
            dir,
            &["update-ref", "refs/agent/sessions/test-session", &t2],
        );

        let repo = gix::open(dir).unwrap();
        let range = range_diff(&repo, &oid(&base), &oid(&head));
        let turns = load_turns(&repo);
        assert_eq!(turns.len(), 2);
        assert_eq!(matching_turns(&range, &turns).len(), 1);

        let mut loaded = load::load(dir, &base, &head).unwrap();
        let sess = mine(&mut loaded).sessions;
        let titles: Vec<&String> = sess
            .iter()
            .filter_map(|r| match r {
                Row::Title { text } => Some(text),
                _ => None,
            })
            .collect();
        assert!(titles.iter().any(|t| t.contains("Take notes")));
        assert!(titles.iter().any(|t| t.contains("Add two")));

        let prose: Vec<&String> = sess
            .iter()
            .filter_map(|r| match r {
                Row::Prose { md } => Some(md),
                _ => None,
            })
            .collect();
        assert!(
            prose
                .iter()
                .any(|md| md.contains("not linked to this diff"))
        );
        assert!(prose.iter().any(|md| md.contains("recorded at commit")));
        assert!(
            prose
                .iter()
                .any(|md| md.contains("**1 session(s)** linked to it")
                    && md.contains("**2 turn(s)**, **1** of them linked")),
            "header must count the whole session: {prose:?}"
        );
        // Turn 2 carries the commit it was recorded on.
        assert!(
            sess.iter()
                .any(|r| matches!(r, Row::FileHeader { path, .. } if path == "a.txt"))
        );
    }

    #[test]
    fn ignores_repos_without_sessions() {
        let (tmp, _) = base_repo();
        let dir = tmp.path();

        let repo = gix::open(dir).unwrap();
        assert!(load_turns(&repo).is_empty());
    }
}
