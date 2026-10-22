//! concats sessions, read natively — the "why" column of a review.
//!
//! The capture layer (this repo's own CLI) records agent transcripts into the
//! same git repository as the code, under two ref namespaces
//! (rfcs/session_storage.md):
//!
//! ```text
//! refs/agent/sessions/<id>    turn commits: EMPTY tree, transcript in the
//!                             commit message. parents[0] = previous turn (or
//!                             the commit the session started on), parents[1] =
//!                             branch HEAD at write time, when different.
//! refs/agent/snapshots/<id>   snapshot commits: full worktree tree.
//!                             parents[0] = previous snapshot, parents[1] = the
//!                             turn it belongs to (the first snapshot has only
//!                             the turn parent).
//! ```
//!
//! Object access goes through gix (pure Rust). The turn grammar is parsed by
//! concats_message, the same parser the capture layer writes with, so the wire
//! format cannot drift between writer and reader.
//!
//! Linking a turn to a commit means "link intent to output": a commit and a
//! turn are related when the commit's diff and the turn's snapshot diff share a
//! (path, blob) pair — the same bytes landing at the same place. A turn whose
//! parents[1] is a range commit is linked too; it was recorded with that commit
//! checked out.
//!
//! One caveat: `refs/agent/*` are not fetched by default refspecs, so sessions
//! are only visible where they were recorded or explicitly fetched.

use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
};

use concats_diff::{
    LoadStats, Row,
    load::{self, Change, FileLowerer, Loaded},
};
use concats_message::{SESSION_REF_PREFIX, SNAPSHOT_REF_PREFIX, TurnEntryKind};
use gix::{ObjectId, Repository};

/// Guard against garbage refs; a session is a conversation, not a history.
const MAX_CHAIN: usize = 10_000;

pub fn worktree_fingerprint(repo: &Repository) -> Result<u64, concats_diff::Error> {
    let mut fingerprint = DefaultHasher::new();
    concats_diff::stage::worktree_fingerprint(repo)?.hash(&mut fingerprint);
    let platform = repo
        .references()
        .map_err(|error| concats_diff::Error::git("session refs", error))?;
    let mut refs = Vec::new();
    for prefix in [SESSION_REF_PREFIX, SNAPSHOT_REF_PREFIX] {
        let iter = platform
            .prefixed(prefix)
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
    }
    refs.sort();
    refs.hash(&mut fingerprint);
    Ok(fingerprint.finish())
}

/// A parsed turn plus what its snapshot says it actually changed.
pub struct Turn {
    pub session_id: String,
    pub oid: ObjectId,
    pub message: concats_message::Turn,
    /// parents[1]: branch HEAD when the turn was recorded, when different.
    pub branch_parent: Option<ObjectId>,
    /// The boundary diff of this turn: its final snapshot state against the
    /// previous turn's.
    pub touched: Vec<Change>,
}

/// One diff as a lookup: path -> blobs-after (`None` = deleted).
type PathBlobs = HashMap<String, HashSet<Option<ObjectId>>>;

/// One commit of the range: identity, message, and its own diff against the
/// first parent — both as lowering input (`changes`, with the before/after
/// oids the `FileLowerer` needs) and as the linking lookup (`paths`).
struct CommitDiff {
    oid: ObjectId,
    /// First line of the message.
    subject: String,
    /// The rest of the message, trimmed. Empty when subject-only.
    body: String,
    /// More than one = a merge; its diff is still against parents[0].
    parent_count: usize,
    changes: Vec<Change>,
    paths: PathBlobs,
}

/// The loaded range, prepared once: its commit set and each commit's diff.
/// Turns get linked against this, the Commits tab and the sessions' per-commit
/// diffs render from it, and its union is the cheap prefilter that keeps
/// session mining off the syscall floor.
pub struct RangeDiff {
    commit_set: HashSet<ObjectId>,
    /// Head-first, as walked from `head` — reverse for display order.
    by_commit: Vec<CommitDiff>,
    union: PathBlobs,
}

impl RangeDiff {
    fn overlaps(&self, touched: &[Change]) -> bool {
        touched.iter().any(|c| {
            self.union
                .get(c.path())
                .is_some_and(|s| s.contains(&c.new_oid()))
        })
    }
}

pub fn review_diff(repo: &Repository, loaded: &Loaded) -> RangeDiff {
    match (loaded.merge_base, loaded.head) {
        (Some(base), Some(head)) => range_diff(repo, &base, &head),
        // NOTE: only a WORKTREE load lacks a commit endpoint, and only it has
        // stage files: their new blobs are the range.
        _ => RangeDiff {
            commit_set: HashSet::new(),
            by_commit: Vec::new(),
            union: loaded
                .stage
                .iter()
                .map(|file| (file.path.clone(), HashSet::from([file.new])))
                .collect(),
        },
    }
}

pub fn matching_turns(range: &RangeDiff, turns: &[Turn]) -> HashMap<usize, (bool, bool)> {
    turns
        .iter()
        .enumerate()
        .filter_map(|(i, turn)| {
            let via_tree = range.overlaps(&turn.touched);
            let via_branch = turn
                .branch_parent
                .is_some_and(|oid| range.commit_set.contains(&oid));
            (via_tree || via_branch).then_some((i, (via_tree, via_branch)))
        })
        .collect()
}

fn range_diff(repo: &Repository, merge_base: &ObjectId, head: &ObjectId) -> RangeDiff {
    let commits = range_commits(repo, merge_base, head);
    let mut by_commit = Vec::new();
    let mut union: PathBlobs = HashMap::new();
    for (c, commit) in &commits {
        let c = *c;
        let Some(parent) = commit.parents.first().copied() else {
            continue;
        };
        let Ok(changes) = load::diff_commits(repo, parent, c) else {
            continue;
        };
        let mut m: PathBlobs = HashMap::new();
        for c in &changes {
            m.entry(c.path().to_string())
                .or_default()
                .insert(c.new_oid());
            union
                .entry(c.path().to_string())
                .or_default()
                .insert(c.new_oid());
        }
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
            paths: m,
        });
    }
    RangeDiff {
        commit_set: commits.into_iter().map(|(oid, _)| oid).collect(),
        by_commit,
        union,
    }
}

/// One commit, as the mining pass needs it. The direct counterpart of the
/// object's own fields — message, parents, tree.
struct Commit {
    message: String,
    parents: Vec<ObjectId>,
    tree: ObjectId,
}

fn read_commit(repo: &Repository, oid: &ObjectId) -> Result<Commit, ()> {
    let commit = repo.find_commit(*oid).map_err(|_| ())?;
    let decoded = commit.decode().map_err(|_| ())?;
    Ok(Commit {
        message: decoded.message.to_string(),
        parents: decoded.parents().collect(),
        tree: decoded.tree(),
    })
}

/// Every turn of every session in the repo, oldest first within a session.
///
/// The walk follows parents[0] from each session ref tip and stops at the first
/// commit whose message is not a turn of that session: the ordinary commit the
/// session was started on.
///
/// Snapshot attribution (`touched`) is only computed for candidate sessions:
/// ones with a turn recorded at a range commit, or whose overall session diff
/// (base tree vs newest snapshot tree, one tree diff) overlaps the range. Every
/// other session costs a chain walk and a single diff, not one diff per turn.
/// With a hundred recorded sessions that is the difference between a fast load
/// and seconds of I/O. The prefilter can miss a turn whose change touched the
/// range but was reverted by session end; for a linking aid that is acceptable.
// NOTE: two production callers, mine and `concats turns`; the rest of its fan-in is tests.
// coland-ignore: @coland/high_fan_in_complex_function
pub fn load_turns(repo: &Repository, range: &RangeDiff) -> Vec<Turn> {
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

        // Walk the turn chain first: the snapshot attribution below needs the
        // final turn oids (a turn that was amended changed its oid, and older
        // snapshots still point at the amended-away generations).
        let mut chain: Vec<(ObjectId, Commit, concats_message::Turn)> = Vec::new();
        let mut base = None; // the ordinary commit the session started on
        let mut oid = tip;
        for _ in 0..MAX_CHAIN {
            let Ok(c) = read_commit(repo, &oid) else {
                break;
            };
            let Ok(msg) = c.message.parse::<concats_message::Turn>() else {
                base = Some(oid);
                break;
            };
            if msg.session_id().as_str() != session_id {
                base = Some(oid);
                break;
            }
            let parent = c.parents.first().copied();
            chain.push((oid, c, msg));
            match parent {
                Some(p) => oid = p,
                None => break,
            }
        }
        chain.reverse(); // walked tip -> back; expose oldest-first

        let branch_link = chain.iter().any(|(_, c, _)| {
            c.parents
                .get(1)
                .is_some_and(|p| range.commit_set.contains(p))
        });
        // The newest snapshot of the session, whichever turn it belongs to.
        let snapshot = repo
            .find_reference(&format!("{SNAPSHOT_REF_PREFIX}{session_id}"))
            .ok()
            .and_then(|r| r.try_id().map(gix::Id::detach));
        let candidate = branch_link
            || match (base, snapshot.and_then(|tip| read_commit(repo, &tip).ok())) {
                (Some(base), Some(tip)) => match read_commit(repo, &base).map(|c| c.tree) {
                    Ok(base_tree) => load::diff_trees(repo, base_tree, tip.tree)
                        .is_ok_and(|changes| range.overlaps(&changes)),
                    Err(_) => false,
                },
                _ => false,
            };

        let chain_oids: Vec<ObjectId> = chain.iter().map(|(o, _, _)| *o).collect();
        let mut touched = match snapshot.filter(|_| candidate) {
            Some(tip) => snapshot_diffs(repo, tip, &chain_oids, base),
            None => HashMap::new(),
        };

        for (oid, c, msg) in chain {
            turns.push(Turn {
                session_id: session_id.clone(),
                oid,
                branch_parent: c.parents.get(1).copied(),
                touched: touched.remove(&oid).unwrap_or_default(),
                message: msg,
            });
        }
    }
    turns
}

/// final turn oid -> the boundary diff of that turn: its last snapshot tree
/// against the previous turn's (or, for the first turn, against the commit the
/// session started on; turn commits themselves have empty trees).
///
/// The subtlety is amended turns. A turn is amended as the agent streams, and
/// each amend gives it a new oid, so most snapshots' `parents[1]` point at turn
/// generations that are no longer in the session chain. Keying on the
/// snapshot's literal turn parent would attribute nearly everything to dangling
/// oids and leave the final turns empty (we saw that on live data). Instead
/// each snapshot is mapped to a chain position: a dangling turn generation has
/// the same `parents[0]` as its final generation, so "previous commit →
/// position" resolves every generation to one slot.
fn snapshot_diffs(
    repo: &Repository,
    tip: ObjectId,
    chain: &[ObjectId],
    base: Option<ObjectId>,
) -> HashMap<ObjectId, Vec<Change>> {
    let mut out = HashMap::new();
    let (Some(base), false) = (base, chain.is_empty()) else {
        return out;
    };

    let index: HashMap<ObjectId, usize> = chain.iter().enumerate().map(|(i, o)| (*o, i)).collect();
    // previous commit -> the position of the turn that follows it.
    let mut prev_of: HashMap<ObjectId, usize> = HashMap::new();
    prev_of.insert(base, 0);
    for i in 1..chain.len() {
        prev_of.insert(chain[i - 1], i);
    }

    // Walk the snapshot chain tip -> back, mapping each snapshot to a chain
    // position; then keep the last snapshot tree per position (a turn can have
    // many snapshots: tool_write, files_changed, one per amend, …).
    let mut newest_tree: HashMap<usize, ObjectId> = HashMap::new();
    let mut cur = Some(tip);
    for _ in 0..MAX_CHAIN {
        let Some(oid) = cur else { break };
        let Ok(snap) = read_commit(repo, &oid) else {
            break;
        };
        let (prev, turn) = match snap.parents.as_slice() {
            [prev, turn] => (Some(*prev), *turn),
            [turn] => (None, *turn),
            _ => break,
        };
        let pos = index.get(&turn).copied().or_else(|| {
            let p0 = read_commit(repo, &turn).ok()?.parents.first().copied()?;
            prev_of.get(&p0).copied()
        });
        if let Some(pos) = pos {
            // tip-first walk: the first tree we see per position is the newest.
            newest_tree.entry(pos).or_insert(snap.tree);
        }
        cur = prev;
    }

    // Boundary diffs, oldest first. Positions with no snapshot (a turn that
    // changed nothing) carry the baseline forward.
    let Ok(base_commit) = read_commit(repo, &base) else {
        return out;
    };
    let mut base_tree = base_commit.tree;
    for (pos, turn) in chain.iter().enumerate() {
        let Some(tree) = newest_tree.get(&pos).copied() else {
            continue;
        };
        // NOTE: an unreadable snapshot tree attributes nothing; the turn stays
        // unlinked, as it always did.
        out.insert(
            *turn,
            load::diff_trees(repo, base_tree, tree).unwrap_or_default(),
        );
        base_tree = tree;
    }
    out
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
/// Commit diffs are exact — worktree noise never entered a commit — so unlike
/// the old snapshot boundary diffs, nothing here needs path scoping.
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
/// recorded with the commit checked out (parents[1]), or sharing a (path, blob)
/// with its diff. Walked tail-first, and `by_commit` is head-first, so each
/// list is the commits in the order the turn produced them.
fn commits_by_turn(range: &RangeDiff, turns: &[Turn]) -> HashMap<usize, Vec<usize>> {
    let mut out: HashMap<usize, Vec<usize>> = HashMap::new();
    for (ci, cd) in range.by_commit.iter().enumerate().rev() {
        for (i, t) in turns.iter().enumerate() {
            let via_branch = t.branch_parent == Some(cd.oid);
            let via_tree = t.touched.iter().any(|c| {
                cd.paths
                    .get(c.path())
                    .is_some_and(|s| s.contains(&c.new_oid()))
            });
            if via_branch || via_tree {
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
    // NOTE: the mining pass re-reads the same snapshot trees constantly; the
    // object cache `open_repo` sets collapses that to memory reads.
    let Ok(repo) = load::open_repo(&loaded.root) else {
        return Mined::default();
    };
    let range = review_diff(&repo, loaded);

    let mut mined = Mined::default();
    // The Commits tab is independent of sessions — build it before the
    // no-turns bail-out. A WORKTREE review has no head commit of its own, so
    // its tab lists HEAD's commits since the base, then what is uncommitted.
    let worktree = loaded
        .head
        .is_none()
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

    let turns = load_turns(&repo, &range);
    if turns.is_empty() {
        return mined;
    }
    let via = matching_turns(&range, &turns);
    let turn_commits = commits_by_turn(&range, &turns);

    mined.sessions = sessions_rows(
        &repo,
        &range,
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
    let merge_base = loaded.merge_base?;
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
/// the same blob table. Commit diffs are exact, so the old snapshot-noise
/// scoping is gone. A commit several turns produced together renders under the
/// first and is cross-referenced after.
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
    via: &HashMap<usize, (bool, bool)>,
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
        via.keys().map(|&i| turns[i].session_id.as_str()).collect();
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
            if loaded.workdir.is_some() {
                " A linked turn's snapshot holds the same changed file contents as this \
                 working tree."
            } else {
                " Each linked turn is followed by the commit(s) it produced."
            },
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
                format!("session/{session}"),
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
            match via.get(&i) {
                Some((true, _)) => "matched by content",
                Some((false, _)) => "recorded at commit",
                None => "not linked to this diff",
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
/// — exactly what the Commits tab shows for it. Rendering the commit rather
/// than the turn's snapshot boundary kills the worktree noise (Cargo.lock
/// churn, scratch files) at the source: what never entered a commit was never
/// part of this diff. A commit already rendered under an earlier turn collapses
/// to a cross-reference line.
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
    let turn = format!("turn/{}", &t.oid.to_string()[..10]);
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
    use std::{path::Path, str::FromStr};

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

    /// A `test-session` snapshot commit of `tree`.
    fn snapshot(dir: &Path, tree: &str, parents: &[&str], reason: &str) -> String {
        let message = format!("snapshot\n\nSession: test-session\nReason: {reason}");
        let args: Vec<&str> = ["commit-tree", tree]
            .into_iter()
            .chain(parents.iter().flat_map(|parent| ["-p", parent]))
            .chain(["-m", &message])
            .collect();
        git(dir, &args)
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
        let head_tree = git(dir, &["rev-parse", "HEAD^{tree}"]);

        // The session that produced it: a turn (empty tree, message = the
        // transcript, parent = base) and a snapshot whose tree matches the
        // feature commit's content.
        let msg = turn_message("Add two", "please add two", "added two");
        let turn = git(dir, &["commit-tree", EMPTY_TREE, "-p", &base, "-m", &msg]);
        git(
            dir,
            &["update-ref", "refs/agent/sessions/test-session", &turn],
        );
        let snap = snapshot(dir, &head_tree, &[&turn], "turn_commit");
        git(
            dir,
            &["update-ref", "refs/agent/snapshots/test-session", &snap],
        );

        let repo = gix::open(dir).unwrap();
        let mb = oid(&base);
        let hd = oid(&head);
        let range = range_diff(&repo, &mb, &hd);
        let turns = load_turns(&repo, &range);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].message.subject(), "Add two");
        assert_eq!(turns[0].session_id, "test-session");
        assert_eq!(
            turns[0].touched,
            vec![Change::Modified {
                path: "a.txt".to_string(),
                old_oid: oid(&git(dir, &["rev-parse", "HEAD~1:a.txt"])),
                new_oid: oid(&git(dir, &["rev-parse", "HEAD:a.txt"])),
            }]
        );

        // The turn's snapshot and the feature commit changed the same bytes at
        // the same path, so the link fires via tree overlap.
        assert_eq!(range.by_commit[0].oid, hd);
        assert_eq!(
            commits_by_turn(&range, &turns),
            HashMap::from([(0, vec![0])])
        );
        assert_eq!(
            matching_turns(&range, &turns),
            HashMap::from([(0, (true, false))])
        );

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

    /// The live-data failure mode: a turn is amended as the agent streams, so
    /// most snapshots' turn parents point at amended-away generations whose
    /// oids are no longer in the session chain. Attribution must resolve those
    /// generations to the final turn, or the final turn shows no changes.
    #[test]
    fn attributes_changes_across_amended_turns() {
        let (tmp, base) = base_repo();
        let dir = tmp.path();

        // Turn v1 + a snapshot of its mid-turn state.
        let msg_v1 = turn_message("Add lines", "please add lines", "working");
        let v1 = git(
            dir,
            &["commit-tree", EMPTY_TREE, "-p", &base, "-m", &msg_v1],
        );
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(dir, &["add", "."]);
        let tree1 = git(dir, &["write-tree"]);
        let s1 = snapshot(dir, &tree1, &[&v1], "tool_write");

        // The amend: same parents, updated message => new turn oid. The ref
        // moves to v2; v1 dangles, referenced only by s1.
        let msg_v2 = turn_message("Add lines", "please add lines", "added two and three");
        let v2 = git(
            dir,
            &["commit-tree", EMPTY_TREE, "-p", &base, "-m", &msg_v2],
        );
        git(
            dir,
            &["update-ref", "refs/agent/sessions/test-session", &v2],
        );
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(dir, &["add", "."]);
        let tree2 = git(dir, &["write-tree"]);
        let s2 = snapshot(dir, &tree2, &[&s1, &v2], "turn_amend");
        git(
            dir,
            &["update-ref", "refs/agent/snapshots/test-session", &s2],
        );

        // The output: a commit with the turn's final content.
        git(dir, &["commit", "-qm", "feature"]);
        let head = git(dir, &["rev-parse", "HEAD"]);

        let repo = gix::open(dir).unwrap();
        let range = range_diff(&repo, &oid(&base), &oid(&head));
        let turns = load_turns(&repo, &range);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].oid, oid(&v2));
        // The full change (one -> one,two,three) lands on the final turn, even
        // though snapshot s1 points at the amended-away v1.
        assert_eq!(
            turns[0].touched,
            vec![Change::Modified {
                path: "a.txt".to_string(),
                old_oid: oid(&git(dir, &["rev-parse", "HEAD~1:a.txt"])),
                new_oid: oid(&git(dir, &["rev-parse", "HEAD:a.txt"])),
            }]
        );

        assert_eq!(
            commits_by_turn(&range, &turns),
            HashMap::from([(0, vec![0])])
        );
        assert!(matches!(
            matching_turns(&range, &turns).get(&0),
            Some((true, _))
        ));
    }

    /// A session is linked as a whole: once any turn of it matches the range,
    /// every turn renders, because the turns that produced nothing are what led
    /// to the turn that did.
    #[test]
    fn renders_every_turn_of_a_linked_session() {
        let (tmp, base) = base_repo();
        let dir = tmp.path();

        // Turn 1 only wrote a scratch file that never reaches the range.
        let msg1 = turn_message("Take notes", "think it through", "noted");
        let t1 = git(dir, &["commit-tree", EMPTY_TREE, "-p", &base, "-m", &msg1]);
        std::fs::write(dir.join("b.txt"), "scratch\n").unwrap();
        git(dir, &["add", "."]);
        let tree1 = git(dir, &["write-tree"]);
        let s1 = snapshot(dir, &tree1, &[&t1], "tool_write");

        // Turn 2 wrote the content the range's commit carries.
        let msg2 = turn_message("Add two", "now add two", "added two");
        let t2 = git(dir, &["commit-tree", EMPTY_TREE, "-p", &t1, "-m", &msg2]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(dir, &["add", "."]);
        let tree2 = git(dir, &["write-tree"]);
        let s2 = snapshot(dir, &tree2, &[&s1, &t2], "turn_amend");
        git(
            dir,
            &["update-ref", "refs/agent/sessions/test-session", &t2],
        );
        git(
            dir,
            &["update-ref", "refs/agent/snapshots/test-session", &s2],
        );

        // The output commit carries a.txt only — b.txt stays the agent's
        // scratch, so turn 1 has nothing in this range.
        git(dir, &["rm", "-q", "-f", "b.txt"]);
        git(dir, &["commit", "-qm", "feature"]);
        let head = git(dir, &["rev-parse", "HEAD"]);

        let repo = gix::open(dir).unwrap();
        let range = range_diff(&repo, &oid(&base), &oid(&head));
        let turns = load_turns(&repo, &range);
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
        assert!(prose.iter().any(|md| md.contains("matched by content")));
        assert!(
            prose
                .iter()
                .any(|md| md.contains("**1 session(s)** linked to it")
                    && md.contains("**2 turn(s)**, **1** of them linked")),
            "header must count the whole session: {prose:?}"
        );
        // Only turn 2 produced the commit, so only it carries diff rows.
        assert!(
            sess.iter()
                .any(|r| matches!(r, Row::FileHeader { path, .. } if path == "a.txt"))
        );
        assert!(
            !sess
                .iter()
                .any(|r| matches!(r, Row::FileHeader { path, .. } if path == "b.txt"))
        );
    }

    #[test]
    fn ignores_repos_without_sessions() {
        let (tmp, base) = base_repo();
        let dir = tmp.path();

        let repo = gix::open(dir).unwrap();
        let head = oid(&base);
        let range = range_diff(&repo, &head, &head);
        assert!(load_turns(&repo, &range).is_empty());
    }
}
