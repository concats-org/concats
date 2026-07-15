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
    path::Path,
};

use concats_diff::{
    LoadStats, Row,
    load::{self, Change, FileLowerer, Loaded},
};
use concats_message::{SESSION_REF_PREFIX, SNAPSHOT_REF_PREFIX, TurnEntryKind};
use gix::{ObjectId, Repository};

/// Guard against garbage refs; a session is a conversation, not a history.
const MAX_CHAIN: usize = 10_000;

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
pub struct CommitDiff {
    pub oid: ObjectId,
    /// First line of the message.
    pub subject: String,
    /// The rest of the message, trimmed. Empty when subject-only.
    pub body: String,
    /// More than one = a merge; its diff is still against parents[0].
    pub parent_count: usize,
    pub changes: Vec<Change>,
    paths: PathBlobs,
}

/// The loaded range, prepared once: its commit set and each commit's diff.
/// Turns get linked against this, the Commits tab and the sessions' per-commit
/// diffs render from it, and its union is the cheap prefilter that keeps
/// session mining off the syscall floor.
pub struct RangeDiff {
    commit_set: HashSet<ObjectId>,
    /// Head-first, as walked from `head` — reverse for display order.
    pub by_commit: Vec<CommitDiff>,
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

pub fn range_diff(repo: &Repository, merge_base: &ObjectId, head: &ObjectId) -> RangeDiff {
    let commits = range_commits(repo, merge_base, head);
    let mut by_commit = Vec::new();
    let mut union: PathBlobs = HashMap::new();
    for &c in &commits {
        let Ok(commit) = read_commit(repo, &c) else {
            continue;
        };
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
        let (subject, body) = split_message(&commit.message);
        by_commit.push(CommitDiff {
            oid: c,
            subject,
            body,
            parent_count: commit.parents.len(),
            changes,
            paths: m,
        });
    }
    RangeDiff {
        commit_set: commits.into_iter().collect(),
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
        let candidate = branch_link
            || match (base, session_tip_tree(repo, &session_id)) {
                (Some(base), Some(tip_tree)) => match read_commit(repo, &base).map(|c| c.tree) {
                    Ok(base_tree) => load::diff_trees(repo, base_tree, tip_tree)
                        .is_ok_and(|changes| range.overlaps(&changes)),
                    Err(_) => false,
                },
                _ => false,
            };

        let chain_oids: Vec<ObjectId> = chain.iter().map(|(o, _, _)| *o).collect();
        let mut touched = if candidate {
            snapshot_diffs(repo, &session_id, &chain_oids, base)
        } else {
            HashMap::new()
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

/// The tree of the newest snapshot of a session, whichever turn it belongs to.
fn session_tip_tree(repo: &Repository, session_id: &str) -> Option<ObjectId> {
    let tip = resolve_ref(repo, &format!("{SNAPSHOT_REF_PREFIX}{session_id}"))?;
    read_commit(repo, &tip).ok().map(|c| c.tree)
}

/// A ref's target oid, if the ref exists.
fn resolve_ref(repo: &Repository, name: &str) -> Option<ObjectId> {
    repo.find_reference(name)
        .ok()?
        .try_id()
        .map(gix::Id::detach)
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
    session_id: &str,
    chain: &[ObjectId],
    base: Option<ObjectId>,
) -> HashMap<ObjectId, Vec<Change>> {
    let mut out = HashMap::new();
    let Some(tip) = resolve_ref(repo, &format!("{SNAPSHOT_REF_PREFIX}{session_id}")) else {
        return out;
    };
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

/// (subject, body): the first line, and the rest trimmed. Gix hands
/// the message over as one string, so the split lives here.
fn split_message(msg: &str) -> (String, String) {
    match msg.trim().split_once('\n') {
        Some((s, rest)) => (s.trim().to_string(), rest.trim().to_string()),
        None => (msg.trim().to_string(), String::new()),
    }
}

/// Header rows for one commit: the subject as a heading, the body (when there
/// is one) quoted line by line. Commit messages are not markdown, and a stray
/// `#` or list marker must not restructure the document. Inside a blockquote it
/// can't — the same guard turn prompts get.
fn commit_header_rows(rows: &mut Vec<Row>, cd: &CommitDiff, level: &str) {
    rows.push(Row::Title {
        text: format!("{level} {}  `{}`", cd.subject, &cd.oid.to_string()[..10]),
    });
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
        rows.push(Row::Prose { md });
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
    cd: &CommitDiff,
    id_prefix: &str,
    low: &mut FileLowerer,
    blob_paths: &mut HashMap<u32, String>,
) {
    let mut shown = 0usize;
    let mut binary = 0usize;
    for (j, c) in cd.changes.iter().enumerate() {
        match low.file(
            format!("{id_prefix}-c{j}"),
            c.path().to_string(),
            None,
            None,
            c.old_oid(),
            c.new_oid(),
        ) {
            Ok(Some(fc)) => {
                for h in &fc.hunks {
                    for r in &h.rows {
                        if let Row::Code { blob, .. } = r {
                            blob_paths.entry(*blob).or_insert_with(|| fc.path.clone());
                        }
                    }
                }
                rows.push(Row::FileHeader {
                    path: fc.path.clone(),
                    lang: fc.lang,
                    adds: fc.adds,
                    dels: fc.dels,
                    from: fc.from.clone(),
                    similarity: fc.similarity,
                });
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

/// A turn ↔ commit link inside a review range.
pub struct Link {
    pub commit: ObjectId,
    /// Index into the `turns` slice passed to `link_turns`.
    pub turn: usize,
    /// The commit's diff and the turn's snapshot diff share a (path, blob).
    pub via_tree: bool,
    /// The turn was recorded with this commit checked out (parents[1]).
    pub via_branch: bool,
}

/// Link turns to the commits of the range. Pure: everything expensive was
/// computed once in `range_diff`.
pub fn link_turns(range: &RangeDiff, turns: &[Turn]) -> Vec<Link> {
    let mut links = Vec::new();
    if turns.is_empty() {
        return links;
    }
    for cd in &range.by_commit {
        for (i, t) in turns.iter().enumerate() {
            let via_branch = t.branch_parent == Some(cd.oid);
            let via_tree = t.touched.iter().any(|c| {
                cd.paths
                    .get(c.path())
                    .is_some_and(|s| s.contains(&c.new_oid()))
            });
            if via_branch || via_tree {
                links.push(Link {
                    commit: cd.oid,
                    turn: i,
                    via_tree,
                    via_branch,
                });
            }
        }
    }
    links
}

/// Commits reachable from `head`, stopping at `merge_base`. This is the
/// pragmatic review-range set — a side branch merged below the merge base can
/// slip extra commits in, which is acceptable for attributing transcripts.
fn range_commits(repo: &Repository, merge_base: &ObjectId, head: &ObjectId) -> Vec<ObjectId> {
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
        out.push(oid);
        stack.extend(c.parents.iter().copied());
        if out.len() >= MAX_CHAIN {
            break;
        }
    }
    out
}

/// What the review document gets out of all this.
#[derive(Default)]
pub struct Mined {
    /// The Sessions tab: per linked session, the transcript of its linked
    /// turns interleaved with the per-commit diffs those turns produced.
    pub sessions: Vec<Row>,
    /// The Commits tab: the range organized by commit, oldest first. Empty
    /// (no tab) when the range has fewer than two commits — one commit would
    /// just duplicate the File Diff.
    pub commits: Vec<Row>,
    /// blob index -> path for blobs the session/commit lowering added (the
    /// review's own files are mapped by the caller).
    pub blob_paths: HashMap<u32, String>,
}

/// Mine the repo's recorded sessions for the loaded range. Cheap when there are
/// none (one refs directory listing), so the GUI just always calls it. Takes
/// `loaded` mutably because the sessions tab lowers each turn's own diff into
/// the same blob table the review uses.
pub fn mine(repo_path: &Path, loaded: &mut Loaded) -> Mined {
    // A WORKTREE load has no commit range — no commits to organize, no
    // sessions to link.
    let (Some(merge_base), Some(head)) = (loaded.merge_base, loaded.head) else {
        return Mined::default();
    };
    let Some(root) = load::discover(repo_path) else {
        return Mined::default();
    };
    let Ok(mut repo) = gix::open(&root) else {
        return Mined::default();
    };
    // The mining pass re-reads the same snapshot trees constantly; a small
    // object cache collapses that to memory reads.
    repo.object_cache_size_if_unset(16 * 1024 * 1024);
    let range = range_diff(&repo, &merge_base, &head);

    let mut mined = Mined::default();
    // The Commits tab is independent of sessions — build it before the
    // no-turns bail-out.
    if range.by_commit.len() > 1 {
        mined.commits = commits_stream(&repo, &range, loaded, &mut mined.blob_paths);
    }

    let turns = load_turns(&repo, &range);
    if turns.is_empty() {
        return mined;
    }
    let links = link_turns(&range, &turns);

    // Dedupe: several commits can link the same turn. Preserve turn order.
    let mut via: HashMap<usize, (bool, bool)> = HashMap::new();
    for l in &links {
        let e = via.entry(l.turn).or_insert((false, false));
        e.0 |= l.via_tree;
        e.1 |= l.via_branch;
    }
    // turn -> the by_commit indices linking it. by_commit is head-first, so
    // descending index order = the commits as the turn produced them.
    let commit_ix: HashMap<ObjectId, usize> = range
        .by_commit
        .iter()
        .enumerate()
        .map(|(i, cd)| (cd.oid, i))
        .collect();
    let mut turn_commits: HashMap<usize, Vec<usize>> = HashMap::new();
    for l in &links {
        if let Some(&ci) = commit_ix.get(&l.commit) {
            turn_commits.entry(l.turn).or_default().push(ci);
        }
    }
    for v in turn_commits.values_mut() {
        v.sort_unstable_by(|a, b| b.cmp(a));
    }

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
        md: format!(
            "**{} commits** in this range, oldest first.",
            range.by_commit.len()
        ),
    });

    let mut blob_ix: HashMap<ObjectId, u32> = loaded
        .blobs
        .iter()
        .enumerate()
        .map(|(i, b)| (b.oid, i as u32))
        .collect();
    let mut next_hunk_id = 0usize;
    let mut scratch = LoadStats::default();
    let mut low = FileLowerer {
        repo,
        blobs: &mut loaded.blobs,
        blob_ix: &mut blob_ix,
        next_hunk_id: &mut next_hunk_id,
        hunk_prefix: "c",
        overlay: None,
        st: &mut scratch,
    };

    for cd in range.by_commit.iter().rev() {
        commit_header_rows(&mut rows, cd, "##");
        let sha = cd.oid.to_string();
        commit_change_rows(&mut rows, cd, &sha[..10], &mut low, blob_paths);
    }
    rows
}

/// The Sessions document: the same row vocabulary the review uses, so the
/// renderer does not change. For each session with linked turns: the transcript
/// of each linked turn (prompt as blockquote, response as markdown, tool calls
/// summarized), followed by the commits that turn produced — each commit's
/// message and its own diff against its first parent, lowered through the same
/// `FileLowerer` the review uses, into the same blob table. Commit diffs are
/// exact, so the old snapshot-noise scoping is gone. A commit several turns
/// produced together renders under the first and is cross-referenced after.
/// Unlinked turns collapse to a count, like skipped context lines.
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

    // Resume interning into the review's blob table: identical content (the
    // usual case, a turn's final state is what got committed) costs nothing.
    let mut blob_ix: HashMap<ObjectId, u32> = loaded
        .blobs
        .iter()
        .enumerate()
        .map(|(i, b)| (b.oid, i as u32))
        .collect();
    let mut next_hunk_id = 0usize;
    // Session lowering must not skew the review's perf numbers.
    let mut scratch = LoadStats::default();
    let mut low = FileLowerer {
        repo,
        blobs: &mut loaded.blobs,
        blob_ix: &mut blob_ix,
        next_hunk_id: &mut next_hunk_id,
        hunk_prefix: "t",
        overlay: None,
        st: &mut scratch,
    };

    let linked_sessions: HashSet<&str> =
        via.keys().map(|&i| turns[i].session_id.as_str()).collect();
    let all_sessions: HashSet<&str> = turns.iter().map(|t| t.session_id.as_str()).collect();

    rows.push(Row::Title {
        text: "# Sessions".into(),
    });
    rows.push(Row::Prose {
        md: format!(
            "The transcripts behind this diff: **{} turn(s)** across **{} \
             session(s)** linked to this range, each followed by the \
             commit(s) it produced.{}",
            via.len(),
            linked_sessions.len(),
            match all_sessions.len() - linked_sessions.len() {
                0 => String::new(),
                n => format!(" {n} other recorded session(s) have no turns linked here."),
            }
        ),
    });

    let mut session = "";
    let mut skipped = 0usize;
    let mut rendered_commits: HashSet<ObjectId> = HashSet::new();
    for (i, t) in turns.iter().enumerate() {
        if !linked_sessions.contains(t.session_id.as_str()) {
            continue;
        }
        if t.session_id != session {
            flush_skipped(&mut rows, &mut skipped);
            session = &t.session_id;
            rows.push(Row::Title {
                text: format!("## Session `{session}`"),
            });
            if let Some(agent) = t.message.agent_name() {
                rows.push(Row::Prose {
                    md: format!("_recorded by **{agent}**_"),
                });
            }
        }
        if !via.contains_key(&i) {
            skipped += 1;
            continue;
        }
        flush_skipped(&mut rows, &mut skipped);
        let commits = turn_commits.get(&i).map(Vec::as_slice).unwrap_or(&[]);
        turn_rows(
            &mut rows,
            t,
            via[&i].0,
            commits,
            range,
            &mut rendered_commits,
            &mut low,
            blob_paths,
        );
    }
    flush_skipped(&mut rows, &mut skipped);
    rows
}

fn flush_skipped(rows: &mut Vec<Row>, skipped: &mut usize) {
    if *skipped > 0 {
        rows.push(Row::Prose {
            md: format!("_⋯ {skipped} turn(s) not linked to this range_"),
        });
        *skipped = 0;
    }
}

/// One turn: header, transcript entries in order, then the commit(s) the turn
/// produced, each as its message header plus its diff against the first parent
/// — exactly what the Commits tab shows for it. Rendering the commit rather
/// than the turn's snapshot boundary kills the worktree noise (Cargo.lock
/// churn, scratch files) at the source: what never entered a commit was never
/// part of this diff. A commit already rendered under an earlier turn collapses
/// to a cross-reference line.
#[allow(clippy::too_many_arguments)]
fn turn_rows(
    rows: &mut Vec<Row>,
    t: &Turn,
    via_tree: bool,
    commits: &[usize],
    range: &RangeDiff,
    rendered_commits: &mut HashSet<ObjectId>,
    low: &mut FileLowerer,
    blob_paths: &mut HashMap<u32, String>,
) {
    rows.push(Row::Title {
        text: format!("### {}", t.message.subject()),
    });
    rows.push(Row::Prose {
        md: format!(
            "_turn `{}` · {}_",
            &t.oid.to_string()[..10],
            if via_tree {
                "matched by content"
            } else {
                "recorded at commit"
            }
        ),
    });

    let mut tools: HashMap<&str, usize> = HashMap::new();
    for e in t.message.entries() {
        match &e.kind {
            TurnEntryKind::Prompt { text } => {
                let mut md = String::from("**Prompt**\n\n");
                for line in text.trim().lines() {
                    md.push_str("> ");
                    md.push_str(line);
                    md.push('\n');
                }
                rows.push(Row::Prose { md });
            }
            TurnEntryKind::Response { text } => {
                rows.push(Row::Prose {
                    md: format!("**Response**\n\n{}", text.trim()),
                });
            }
            TurnEntryKind::ToolCall { kind } => {
                *tools.entry(kind.as_str()).or_insert(0) += 1;
            }
        }
    }
    if !tools.is_empty() {
        let mut parts: Vec<String> = tools.iter().map(|(k, n)| format!("{n}× {k}")).collect();
        parts.sort();
        rows.push(Row::Prose {
            md: format!("_tools: {}_", parts.join(" · ")),
        });
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
        commit_header_rows(rows, cd, "####");
        let sha = cd.oid.to_string();
        commit_change_rows(
            rows,
            cd,
            &format!("{}-{}", &t.oid.to_string()[..10], &sha[..10]),
            low,
            blob_paths,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, process::Command, str::FromStr};

    use concats_message::{SessionId, TurnEntry};

    use super::*;

    /// git's well-known empty tree oid — turn commits carry no content.
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            // Hermetic: ignore user/system config — a global commit.gpgsign
            // (e.g. via the 1Password agent) would otherwise hang the fixture.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

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

    #[test]
    fn reads_and_links_a_synthesized_session() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // An ordinary history: base, then one commit changing a.txt.
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "base"]);
        let base = git(dir, &["rev-parse", "HEAD"]);
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
        let snap = git(
            dir,
            &[
                "commit-tree",
                &head_tree,
                "-p",
                &turn,
                "-m",
                "snapshot\n\nSession: test-session\nReason: turn_commit",
            ],
        );
        git(
            dir,
            &["update-ref", "refs/agent/snapshots/test-session", &snap],
        );

        let repo = gix::open(dir).unwrap();
        let mb = ObjectId::from_hex(base.as_bytes()).unwrap();
        let hd = ObjectId::from_hex(head.as_bytes()).unwrap();
        let range = range_diff(&repo, &mb, &hd);
        let turns = load_turns(&repo, &range);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].message.subject(), "Add two");
        assert_eq!(turns[0].session_id, "test-session");
        assert_eq!(
            turns[0].touched,
            vec![Change::Modified {
                path: "a.txt".to_string(),
                old_oid: ObjectId::from_hex(git(dir, &["rev-parse", "HEAD~1:a.txt"]).as_bytes())
                    .unwrap(),
                new_oid: ObjectId::from_hex(git(dir, &["rev-parse", "HEAD:a.txt"]).as_bytes())
                    .unwrap(),
            }]
        );

        // The turn's snapshot and the feature commit changed the same bytes at
        // the same path, so the link fires via tree overlap.
        let links = link_turns(&range, &turns);
        assert_eq!(links.len(), 1);
        assert!(links[0].via_tree);
        assert!(!links[0].via_branch);
        assert_eq!(links[0].commit, hd);

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
        let mined = mine(dir, &mut loaded);
        let sess = &mined.sessions;
        assert!(
            sess.iter()
                .any(|r| matches!(r, Row::Title { text } if text.contains("test-session")))
        );
        assert!(
            sess.iter()
                .any(|r| matches!(r, Row::Prose { md } if md.contains("please add two")))
        );
        // The turn's diff is the commit it produced: message header (subject +
        // short sha), then that commit's own diff.
        assert!(sess.iter().any(
            |r| matches!(r, Row::Title { text } if text.contains("feature") && text.contains(&head[..10]))
        ));
        assert!(
            sess.iter()
                .any(|r| matches!(r, Row::FileHeader { path, .. } if path == "a.txt"))
        );
        assert!(sess.iter().any(|r| matches!(r, Row::Code { .. })));
        // A one-commit range does not earn a Commits tab.
        assert!(mined.commits.is_empty());
    }

    #[test]
    fn commits_stream_organizes_the_range_by_commit_oldest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "base"]);
        let base = git(dir, &["rev-parse", "HEAD"]);
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
        let mined = mine(dir, &mut loaded);

        let titles: Vec<&String> = mined
            .commits
            .iter()
            .filter_map(|r| match r {
                Row::Title { text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(titles[0], "# Commits");
        // Oldest first: "add two" before "add b".
        let two = titles.iter().position(|t| t.contains("add two")).unwrap();
        let b = titles.iter().position(|t| t.contains("add b")).unwrap();
        assert!(two < b, "commits must render oldest first");
        // The body renders blockquoted line by line, with autolink-able
        // angle brackets escaped — except inside inline code.
        assert!(mined.commits.iter().any(|r| matches!(
            r,
            Row::Prose { md } if md.contains("> why b exists\n> see `Option<T>`")
                && md.contains("> Co-Authored-By: X \\<x@y.z>")
        )));
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

    /// The live-data failure mode: a turn is amended as the agent streams, so
    /// most snapshots' turn parents point at amended-away generations whose
    /// oids are no longer in the session chain. Attribution must resolve those
    /// generations to the final turn, or the final turn shows no changes.
    #[test]
    fn attributes_changes_across_amended_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "base"]);
        let base = git(dir, &["rev-parse", "HEAD"]);

        // Turn v1 + a snapshot of its mid-turn state.
        let msg_v1 = turn_message("Add lines", "please add lines", "working");
        let v1 = git(
            dir,
            &["commit-tree", EMPTY_TREE, "-p", &base, "-m", &msg_v1],
        );
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(dir, &["add", "."]);
        let tree1 = git(dir, &["write-tree"]);
        let s1 = git(
            dir,
            &[
                "commit-tree",
                &tree1,
                "-p",
                &v1,
                "-m",
                "snapshot\n\nSession: test-session\nReason: tool_write",
            ],
        );

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
        let s2 = git(
            dir,
            &[
                "commit-tree",
                &tree2,
                "-p",
                &s1,
                "-p",
                &v2,
                "-m",
                "snapshot\n\nSession: test-session\nReason: turn_amend",
            ],
        );
        git(
            dir,
            &["update-ref", "refs/agent/snapshots/test-session", &s2],
        );

        // The output: a commit with the turn's final content.
        git(dir, &["commit", "-qm", "feature"]);
        let head = git(dir, &["rev-parse", "HEAD"]);

        let repo = gix::open(dir).unwrap();
        let range = range_diff(
            &repo,
            &ObjectId::from_hex(base.as_bytes()).unwrap(),
            &ObjectId::from_hex(head.as_bytes()).unwrap(),
        );
        let turns = load_turns(&repo, &range);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].oid, ObjectId::from_hex(v2.as_bytes()).unwrap());
        // The full change (one -> one,two,three) lands on the final turn, even
        // though snapshot s1 points at the amended-away v1.
        assert_eq!(
            turns[0].touched,
            vec![Change::Modified {
                path: "a.txt".to_string(),
                old_oid: ObjectId::from_hex(git(dir, &["rev-parse", "HEAD~1:a.txt"]).as_bytes())
                    .unwrap(),
                new_oid: ObjectId::from_hex(git(dir, &["rev-parse", "HEAD:a.txt"]).as_bytes())
                    .unwrap(),
            }]
        );

        let links = link_turns(&range, &turns);
        assert_eq!(links.len(), 1);
        assert!(links[0].via_tree);
    }

    #[test]
    fn ignores_repos_without_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "base"]);

        let repo = gix::open(dir).unwrap();
        let head = ObjectId::from_hex(git(dir, &["rev-parse", "HEAD"]).as_bytes()).unwrap();
        let range = range_diff(&repo, &head, &head);
        assert!(load_turns(&repo, &range).is_empty());
    }
}
