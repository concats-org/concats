//! Review state: what was seen, and what was said — anchored to content.
//!
//! Everything here keys on `(blob oid, line)`. Git blobs are content-addressed
//! and immutable, so a mark or a comment attaches to the bytes, not to any
//! view's layout. The same line rendered in the guided review, the classic file
//! diff, a guide or a session's per-turn diff carries the same state,
//! across tabs and across restarts. The flip side, as with the highlight cache:
//! if the file changes, the blob oid changes, and the state stays with the old
//! content. For review state that is the right call; a changed line is
//! un-reviewed by definition.
//!
//! Persistence is SQLite at `.git/concats-app/store.db`: repo-local, never in
//! the worktree, invisible to git. One database carries all of a repo's review
//! state (seen, comments, and the guides table), and the
//! schema lives here and only here. SQLite because every table has at least two
//! concurrent writers — GUI instances saving on every action, agent CLIs adding
//! comments and submitting guides — and WAL plus a busy timeout replace the
//! load-merge-save dance the old JSON file needed. Mutations write through;
//! `refresh`/`external_change` are how the GUI picks up what other connections
//! committed.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use concats_diff::{Blob, Row, Side};
use gix::ObjectId;
use rusqlite::Connection;

use crate::Error;

/// One reviewed line: which blob, which 0-based line.
pub type LineKey = (ObjectId, u32);

/// A comment's run in its file's document, as two encoded cursors: the start
/// of its first line and the end of its last.
pub type Cursors = (Vec<u8>, Vec<u8>);

/// A comment's position: a contiguous, inclusive line range (`start..=end`,
/// 0-based) on one blob, keyed by the blob's content oid so it survives reloads
/// and re-renders. The persisted, oid-keyed twin of the in-memory
/// [`concats_diff::Side`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub blob: ObjectId,
    pub start: u32,
    pub end: u32,
}

impl Anchor {
    /// The same range with `start <= end`.
    fn normalized(self) -> Anchor {
        Anchor {
            start: self.start.min(self.end),
            end: self.start.max(self.end),
            ..self
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub id: u64,
    /// Repo-relative path, recorded for display — the position is the anchor.
    pub path: String,
    /// The lines this comment was written on: the new-side range when one
    /// exists (the comment renders below its last line), otherwise the
    /// old-side range.
    pub anchor: Anchor,
    pub body: String,
    /// Who left it: the GUI records the repo's `user.name`, the CLI whatever
    /// `--author` was given (agents pass their name). Absent on old records.
    pub author: Option<String>,
    pub created_at: u64,
    /// The thread root this replies to, never an intermediate reply.
    /// [`Store::root_of`] normalizes on the way in, so threads are exactly one
    /// level deep (GitHub's `in_reply_to_id` semantics); [`thread_key`] and the
    /// cascading delete both depend on that. A reply keeps the lines it was
    /// written on: its root's by default, or the lines a fix moved the
    /// conversation to. The thread renders under the newest of its comments the
    /// range can place — see [`inject_comments`].
    pub parent: Option<u64>,
    /// Where this comment came from, `"<source>:<id>"` — `"github:2181234567"`
    /// for one imported off a pull request. `None` for comments made here.
    /// Import matches on it, so re-running one converges instead of
    /// duplicating.
    pub external: Option<String>,
    /// The lines as a cursor pair in the file's document, for a comment on a
    /// worktree file: minted when the comment was made, in the app or by the
    /// CLI, and what carries it across edits. Encoded, because the store keeps
    /// bytes and only a document can read them. `None` for a comment on a git
    /// blob, which never moves.
    pub cursors: Option<Cursors>,
}

/// (all seen, any seen) for a set of line keys against a seen set — one
/// answer for the store and the app's published snapshot, so the two can
/// never disagree about what a tick box shows.
pub fn seen_state(keys: &[LineKey], seen: &HashSet<LineKey>) -> (bool, bool) {
    if keys.is_empty() {
        return (false, false);
    }
    let mut all = true;
    let mut any = false;
    for k in keys {
        if seen.contains(k) {
            any = true;
        } else {
            all = false;
        }
    }
    (all, any)
}

/// The thread a comment belongs to: its root's id, itself if it is the root.
/// Trivial because `parent` is always the root — see [`Comment::parent`].
pub fn thread_key(c: &Comment) -> u64 {
    c.parent.unwrap_or(c.id)
}

/// Now, in unix seconds. Callers stamp their own comments because an import
/// carries the timestamp the comment was really written at.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct Store {
    conn: Connection,
    /// SQLite's `data_version` as of our last look. It moves only when another
    /// connection commits, which is the question we ask: did the CLI or another
    /// instance write since we last checked?
    data_version: i64,
    pub seen: HashSet<LineKey>,
    pub comments: Vec<Comment>,
}

/// One schema, one location. The guides table lives here too, read and written
/// through the same per-repo database.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS seen(
  blob TEXT NOT NULL,
  line INTEGER NOT NULL,
  PRIMARY KEY(blob, line)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS comments(
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL,
  blob TEXT NOT NULL,
  start INTEGER NOT NULL,
  end INTEGER NOT NULL,
  body TEXT NOT NULL,
  author TEXT,
  created_at INTEGER NOT NULL,
  parent INTEGER,
  external TEXT
);
CREATE TABLE IF NOT EXISTS guides(
  id INTEGER PRIMARY KEY,
  base TEXT NOT NULL,
  head TEXT NOT NULL,
  author TEXT,
  created_at INTEGER NOT NULL,
  markdown TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS guides_range ON guides(base, head, created_at);
CREATE TABLE IF NOT EXISTS buffers(
  origin TEXT PRIMARY KEY,
  snapshot BLOB NOT NULL,
  disk BLOB NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS comment_cursors(
  comment INTEGER PRIMARY KEY,
  run_start BLOB NOT NULL,
  run_end BLOB NOT NULL
) WITHOUT ROWID;
";

/// The common git directory for `git_dir`: itself for a main worktree, the
/// shared `.git` for a linked one, which git records in a `commondir` file next
/// to the worktree's own git dir.
///
/// Resolved here rather than at each caller because every connection goes
/// through this one place, and getting it wrong is silent. A linked worktree
/// would quietly get a database of its own, and a comment left on a blob in one
/// checkout would be missing when the same blob turns up in another. Everything
/// in this schema is keyed by content — a blob, a line, a commit oid — and
/// linked worktrees share an object database, so they have to share the state
/// too.
fn common_dir(git_dir: &Path) -> std::path::PathBuf {
    let Ok(relative) = std::fs::read_to_string(git_dir.join("commondir")) else {
        return git_dir.to_path_buf();
    };
    let common = git_dir.join(relative.trim());
    common.canonicalize().unwrap_or(common)
}

/// Open (creating if needed) the repo's review database, WAL'd and schema'd.
/// Shared by the guide functions below — every connection to `store.db` goes through here.
pub(crate) fn open_db(git_dir: &Path) -> rusqlite::Result<Connection> {
    let dir = common_dir(git_dir).join("concats-app");
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("warning: cannot create {}: {error}", dir.display());
    }
    let conn = Connection::open(dir.join("store.db"))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

impl Store {
    /// Open the store for a repo. Never fails: when the database cannot be
    /// opened (read-only filesystem, exotic breakage) the store runs on an
    /// in-memory database — fully functional, nothing persists, one warning.
    pub fn open(git_dir: &Path) -> Store {
        let conn = open_db(git_dir).unwrap_or_else(|error| {
            eprintln!("warning: cannot open review store: {error}; state will not persist");
            let conn = Connection::open_in_memory().expect("in-memory sqlite always opens");
            conn.execute_batch(SCHEMA)
                .expect("schema on an empty in-memory database");
            conn
        });
        let mut store = Store {
            conn,
            data_version: -1,
            seen: HashSet::new(),
            comments: Vec::new(),
        };
        store.external_change();
        store.refresh();
        store
    }

    /// Did another connection (the CLI, another instance) commit since we
    /// last asked? Cheap — one pragma read, no table access.
    pub fn external_change(&mut self) -> bool {
        let version = self
            .conn
            .pragma_query_value(None, "data_version", |row| row.get(0))
            .unwrap_or(-1);
        let changed = version != self.data_version;
        self.data_version = version;
        changed
    }

    /// Re-read everything into memory. Returns whether anything visible
    /// changed. External deletes simply vanish and external adds appear:
    /// mutations write through, so memory never holds unsaved state a refresh
    /// could lose.
    pub fn refresh(&mut self) -> bool {
        let mut seen = HashSet::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT blob, line FROM seen") {
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
            });
            for (blob, line) in rows.into_iter().flatten().flatten() {
                if let Ok(oid) = ObjectId::from_hex(blob.as_bytes()) {
                    seen.insert((oid, line));
                }
            }
        }

        let mut comments = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, path, blob, start, end, body, author, created_at, parent, external
             FROM comments ORDER BY id",
        ) {
            let rows = stmt.query_map([], |row| {
                // NOTE: an oid that is not hex is corruption, not a query
                // error — drop the row rather than abandon the read.
                let blob_text = row.get::<_, String>(2)?;
                let Ok(blob) = ObjectId::from_hex(blob_text.as_bytes()) else {
                    return Ok(None);
                };
                let (start, end): (u32, u32) = (row.get(3)?, row.get(4)?);
                Ok(Some(Comment {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    anchor: Anchor { blob, start, end },
                    body: row.get(5)?,
                    author: row.get(6)?,
                    created_at: row.get(7)?,
                    parent: row.get(8)?,
                    external: row.get(9)?,
                    cursors: None,
                }))
            });
            comments.extend(rows.into_iter().flatten().flatten().flatten());
        }
        if let Ok(mut stmt) = self
            .conn
            .prepare("SELECT comment, run_start, run_end FROM comment_cursors")
        {
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            });
            for (id, from, to) in rows.into_iter().flatten().flatten() {
                if let Some(c) = comments.iter_mut().find(|c| c.id == id) {
                    c.cursors = Some((from, to));
                }
            }
        }

        let changed = seen != self.seen || comments != self.comments;
        self.seen = seen;
        self.comments = comments;
        changed
    }

    /// (all seen, any seen) for a set of line keys — drives the tick box
    /// (checked when all) and lets a view show partial state.
    pub fn state(&self, keys: &[LineKey]) -> (bool, bool) {
        seen_state(keys, &self.seen)
    }

    /// Tick-box semantics: if everything is seen, unsee it all; otherwise
    /// mark it all seen. Returns the new state. Writes through, one
    /// transaction per gesture.
    pub fn toggle(&mut self, keys: &[LineKey]) -> bool {
        let (all, _) = self.state(keys);
        let write = self.conn.transaction().and_then(|tx| {
            for (oid, line) in keys {
                if all {
                    tx.execute(
                        "DELETE FROM seen WHERE blob = ?1 AND line = ?2",
                        (oid.to_string(), line),
                    )?;
                } else {
                    tx.execute(
                        "INSERT OR IGNORE INTO seen(blob, line) VALUES (?1, ?2)",
                        (oid.to_string(), line),
                    )?;
                }
            }
            tx.commit()
        });
        if let Err(error) = write {
            eprintln!("warning: cannot save seen state: {error}");
        }
        if all {
            for k in keys {
                self.seen.remove(k);
            }
            false
        } else {
            for k in keys {
                self.seen.insert(*k);
            }
            true
        }
    }

    /// Store a comment and return its id. `comment.id` is ignored: the database
    /// allocates ids, so concurrent writers cannot collide. Every other field
    /// is taken as given, `created_at` included, because an import carries the
    /// time the comment was really written.
    pub fn add_comment(&mut self, comment: Comment) -> u64 {
        let comment = Comment {
            anchor: comment.anchor.normalized(),
            ..comment
        };
        let write = self.conn.execute(
            "INSERT INTO comments
             (path, blob, start, end, body, author, created_at, parent, external)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            (
                &comment.path,
                comment.anchor.blob.to_string(),
                comment.anchor.start,
                comment.anchor.end,
                &comment.body,
                &comment.author,
                comment.created_at,
                comment.parent,
                &comment.external,
            ),
        );
        if let Err(error) = &write {
            eprintln!("warning: cannot save comment: {error}");
        }
        let id = self.conn.last_insert_rowid() as u64;
        let minted = comment.cursors.clone();
        self.comments.push(Comment {
            id,
            cursors: None,
            ..comment
        });
        if let Some(pair) = minted {
            self.set_cursors(&[(id, pair)]);
        }
        id
    }

    /// Give comments their cursor pairs: what a new comment was made with, or
    /// what the app minted for one that arrived without any — written before
    /// cursors existed, held by the buffer from its exact lines. From here on
    /// the document carries the comment, in the CLI too.
    pub fn set_cursors(&mut self, cursors: &[(u64, Cursors)]) {
        for (id, (from, to)) in cursors {
            let write = self.conn.execute(
                "INSERT OR REPLACE INTO comment_cursors(comment, run_start, run_end)
                 VALUES (?1, ?2, ?3)",
                (id, from, to),
            );
            if let Err(error) = write {
                eprintln!("warning: cannot save the comment's cursors: {error}");
            }
            if let Some(c) = self.comments.iter_mut().find(|c| c.id == *id) {
                c.cursors = Some((from.clone(), to.clone()));
            }
        }
    }

    /// Reply to `parent` from where its thread is: the root's place, the
    /// reply's content, so the thread never splits across two lines. A reply
    /// written elsewhere — on the lines a fix moved the
    /// conversation to — is an [`Store::add_comment`] under [`Store::root_of`]
    /// with its own anchor. `None` when `parent` is not a stored comment.
    pub fn reply_comment(
        &mut self,
        parent: u64,
        body: String,
        author: Option<String>,
        created_at: u64,
        external: Option<String>,
    ) -> Option<u64> {
        let root = self.root_of(parent)?;
        let reply = Comment {
            id: 0,
            path: root.path.clone(),
            anchor: root.anchor,
            cursors: root.cursors.clone(),
            parent: Some(root.id),
            body,
            author,
            created_at,
            external,
        };
        Some(self.add_comment(reply))
    }

    /// The root of the thread `id` is in — `id` itself when it is a root. The
    /// one place a comment is threaded: every `parent` written comes from here,
    /// so a reply to a reply threads under the root, like GitHub.
    pub fn root_of(&self, id: u64) -> Option<&Comment> {
        let comment = self.comments.iter().find(|c| c.id == id)?;
        match comment.parent {
            Some(root) => self.comments.iter().find(|c| c.id == root),
            None => Some(comment),
        }
    }

    /// Carry everything anchored to `old` over to `new`.
    ///
    /// Every anchor in this store names content, and saving a file gives it a
    /// new content hash. Without this, a save would detach every comment on the
    /// file and clear all of its seen ticks. `lines` maps each old line to
    /// where it sits now. A line missing from the map was typed away; what was
    /// anchored there stays on the old oid, which is how a thread the range can
    /// no longer place renders as outdated. Comment by comment: a reply written
    /// on other lines than its root moves on its own.
    pub fn rehome(&mut self, old: ObjectId, new: ObjectId, lines: &HashMap<u32, u32>) -> bool {
        let moved: Vec<(u64, Anchor)> = self
            .comments
            .iter()
            .filter(|c| c.anchor.blob == old)
            .filter_map(|c| {
                Some((
                    c.id,
                    Anchor {
                        blob: new,
                        start: *lines.get(&c.anchor.start)?,
                        end: *lines.get(&c.anchor.end)?,
                    },
                ))
            })
            .collect();
        let seen: Vec<(u32, u32)> = self
            .seen
            .iter()
            .filter(|(oid, _)| *oid == old)
            .filter_map(|(_, line)| Some((*line, *lines.get(line)?)))
            .collect();
        if moved.is_empty() && seen.is_empty() {
            return false;
        }
        let write = self.conn.transaction().and_then(|tx| {
            for (from, to) in &seen {
                tx.execute(
                    "DELETE FROM seen WHERE blob = ?1 AND line = ?2",
                    (old.to_string(), from),
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO seen(blob, line) VALUES (?1, ?2)",
                    (new.to_string(), to),
                )?;
            }
            for (id, at) in &moved {
                tx.execute(
                    "UPDATE comments SET blob = ?2, start = ?3, end = ?4 WHERE id = ?1",
                    (id, new.to_string(), at.start, at.end),
                )?;
            }
            tx.commit()
        });
        if let Err(error) = write {
            eprintln!("warning: cannot carry review state to the saved file: {error}");
        }
        for (from, to) in seen {
            self.seen.remove(&(old, from));
            self.seen.insert((new, to));
        }
        for (id, at) in moved {
            if let Some(c) = self.comments.iter_mut().find(|c| c.id == id) {
                c.anchor = at;
            }
        }
        true
    }

    /// An upstream edit reached us: keep the anchor and the thread, take the
    /// new text.
    pub fn set_body(&mut self, id: u64, body: String) {
        if let Err(error) = self
            .conn
            .execute("UPDATE comments SET body = ?2 WHERE id = ?1", (id, &body))
        {
            eprintln!("warning: cannot update comment: {error}");
        }
        if let Some(c) = self.comments.iter_mut().find(|c| c.id == id) {
            c.body = body;
        }
    }

    /// Delete a comment and, when it is a thread root, the replies under it —
    /// a conversation is removed as a unit.
    pub fn delete_comment(&mut self, id: u64) {
        let write = self.conn.transaction().and_then(|tx| {
            tx.execute(
                "DELETE FROM comment_cursors
                  WHERE comment = ?1 OR comment IN (SELECT id FROM comments WHERE parent = ?1)",
                [id],
            )?;
            tx.execute("DELETE FROM comments WHERE id = ?1 OR parent = ?1", [id])?;
            tx.commit()
        });
        if let Err(error) = write {
            eprintln!("warning: cannot delete comment: {error}");
        }
        self.comments.retain(|c| c.id != id && c.parent != Some(id));
    }
}

/// The repo's `user.name`, for attributing GUI-posted comments. A minimal
/// config scan, local first and then the global locations, because this crate
/// carries no libgit2 and shells out to nothing.
pub fn git_user_name(git_dir: &Path) -> Option<String> {
    let mut candidates = vec![git_dir.join("config")];
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        candidates.push(Path::new(&xdg).join("git/config"));
    }
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(Path::new(&home).join(".gitconfig"));
        candidates.push(Path::new(&home).join(".config/git/config"));
    }
    candidates
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .find_map(|text| ini_user_name(&text))
}

fn ini_user_name(text: &str) -> Option<String> {
    let mut in_user = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[') {
            in_user = section
                .trim_end_matches(']')
                .trim()
                .eq_ignore_ascii_case("user");
        } else if in_user
            && let Some((k, v)) = line.split_once('=')
            && k.trim().eq_ignore_ascii_case("name")
        {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// The changed-line keys of a hunk, from its `Row::HunkBar` payload: the del
/// lines on the old blob plus the add lines on the new one. Context lines are
/// not part of "seen": they weren't changed.
pub fn hunk_keys(old: Option<Side>, new: Option<Side>, blobs: &[Blob]) -> Vec<LineKey> {
    let mut keys = Vec::new();
    for side in [old, new].into_iter().flatten() {
        let oid = blobs[side.blob as usize].oid;
        for l in side.start..=side.end {
            keys.push((oid, l));
        }
    }
    keys
}

/// Splice comment rows into a row stream, each thread directly below the LAST
/// line of its range (GitHub's convention) — wherever that line appears, in
/// any view. Idempotent: strips previously injected comment rows first.
///
/// A thread is one contiguous run, under the newest of its comments this range
/// can place. Every comment keeps the lines it was written on, so a reply
/// anchored on the lines a fix moved to brings the conversation with it, and a
/// range that shows only the root's lines still renders the thread there.
/// `refresh` hands comments back in id order and two threads on one line
/// interleave, so sorting by `(thread, id)` first is what lets the composer
/// walk a thread by index instead of searching for it.
///
/// A thread none of whose comments the range can place has nowhere to land —
/// the lines it was written against are not on screen. `show_all` names the
/// paths whose card should show it anyway, appended after the file's rows and
/// marked outdated, so a conversation is never merely invisible.
/// Returns the thread roots it placed. That is the ONE decision about whether a
/// conversation is outdated, and the card header's count reads it rather than
/// asking again — two answers to "can this thread be placed?" would drift, and a
/// header offering to reveal a thread that is already inline is exactly the
/// drift you would get.
pub fn inject_comments(
    rows: &mut Vec<Row>,
    blobs: &[Blob],
    comments: &[Comment],
    show_all: &HashSet<String>,
    about: Option<&str>,
) -> HashSet<u64> {
    rows.retain(|r| !matches!(r, Row::Comment { .. }));
    if comments.is_empty() {
        return HashSet::new();
    }
    let mut threaded: Vec<&Comment> = comments.iter().collect();
    threaded.sort_by_key(|c| (thread_key(c), c.id));

    // Bucketed by where the anchored content is, not by the number it had when
    // the comment was written. Type three lines above a thread and it has to
    // come down with the line it was left on, or it ends up pointing at
    // whatever sits at that number now. Ascending id, so a thread's newest
    // placeable comment has the last word on where it goes; a thread with no
    // place flushes as outdated rather than landing somewhere wrong.
    let where_blobs = blobs_by_path(rows, about);
    let mut thread_at: HashMap<u64, LineKey> = HashMap::new();
    for comment in &threaded {
        if let Some(at) = place(comment, blobs, where_blobs.get(comment.path.as_str())) {
            thread_at.insert(thread_key(comment), at);
        }
    }
    let mut by_anchor: HashMap<LineKey, Vec<&Comment>> = HashMap::new();
    for comment in &threaded {
        if let Some(at) = thread_at.get(&thread_key(comment)) {
            by_anchor.entry(*at).or_default().push(comment);
        }
    }
    let mut out: Vec<Row> = Vec::with_capacity(rows.len() + comments.len());
    // Which threads found a home, and which file's rows we are inside: a
    // card's outdated conversations are flushed when its run ends, which is
    // the next header or the end of the stream.
    let mut placed: HashSet<u64> = HashSet::new();
    // A File tab's stream is about one file and has no header to say so, which
    // is also why its outdated threads were never flushed before.
    let mut card: Option<String> = about.map(str::to_string);

    for row in rows.drain(..) {
        if let Row::FileHeader { path, .. } = &row {
            flush_outdated(&mut out, card.as_deref(), &threaded, &placed, show_all);
            card = Some(path.clone());
        }
        let anchor = match &row {
            Row::Code { blob, line, .. } => Some((blobs[*blob as usize].oid, *line)),
            _ => None,
        };
        out.push(row);
        let Some((oid, line)) = anchor else { continue };
        for c in by_anchor.get(&(oid, line)).into_iter().flatten() {
            placed.insert(thread_key(c));
            out.push(Row::Comment {
                id: c.id,
                parent: c.parent,
                body: c.body.clone(),
                meta: byline(c),
            });
        }
    }
    flush_outdated(&mut out, card.as_deref(), &threaded, &placed, show_all);
    *rows = out;
    placed
}

// --- submitted guides -----------------------------------------------------------
//
// A guide an agent `submit`s is a working aid for this review on this machine.
// It has the same lifecycle as seen-state and comments, so it lives next to
// them: in this database, repo-local, never pushed, keyed by the resolved
// `(merge base, head)` oids of the range it was linted against. Append-only:
// the CLI only inserts, the GUI only reads, and the newest guide wins. Coverage
// numbers and session links are not stored — the app recomputes coverage and
// mines sessions itself; nothing a submission claims is taken on trust.

pub struct Guide {
    /// The merge base. `submit` resolves it with the same loader the GUI uses,
    /// so matching is an oid comparison, not rev-parsing.
    pub base: ObjectId,
    pub head: ObjectId,
    pub created_at: u64,
    pub author: Option<String>,
    pub markdown: String,
}

/// Store one guide. The database serializes concurrent submitters.
pub fn save_guide(git_dir: &Path, rec: &Guide) -> Result<(), Error> {
    let store = |op| move |source| Error::Store { op, source };
    let conn = open_db(git_dir).map_err(store("cannot open review store"))?;
    conn.execute(
        "INSERT INTO guides(base, head, author, created_at, markdown)
         VALUES (?1,?2,?3,?4,?5)",
        (
            rec.base.to_string(),
            rec.head.to_string(),
            &rec.author,
            rec.created_at,
            &rec.markdown,
        ),
    )
    .map_err(store("cannot store guide"))?;
    Ok(())
}

/// Every stored guide, oldest first.
pub fn guides(git_dir: &Path) -> Vec<Guide> {
    let Ok(conn) = open_db(git_dir) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT base, head, author, created_at, markdown FROM guides ORDER BY created_at, id",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, u64>(3)?,
            row.get::<_, String>(4)?,
        ))
    });
    rows.into_iter()
        .flatten()
        .flatten()
        .filter_map(|(base, head, author, created_at, markdown)| {
            Some(Guide {
                base: ObjectId::from_hex(base.as_bytes()).ok()?,
                head: ObjectId::from_hex(head.as_bytes()).ok()?,
                created_at,
                author,
                markdown,
            })
        })
        .collect()
}

/// The (base, head) key a guide is stored under. A WORKTREE endpoint has no
/// commit, so it keys as the zero oid: "this repo's worktree". Worktree content
/// keeps changing as you work, so this match is a convention rather than exact:
/// the newest worktree guide applies, and a link that no longer resolves shows
/// up as a `Row::Warning` in the render, never silently.
pub fn guide_key(merge_base: Option<ObjectId>, head: Option<ObjectId>) -> (ObjectId, ObjectId) {
    let zero = ObjectId::null(gix::hash::Kind::Sha1);
    (merge_base.unwrap_or(zero), head.unwrap_or(zero))
}

/// The newest guide submitted for exactly this range — the one the app loads.
pub fn latest_guide(git_dir: &Path, merge_base: &ObjectId, head: &ObjectId) -> Option<Guide> {
    let conn = open_db(git_dir).ok()?;
    conn.query_row(
        "SELECT base, head, author, created_at, markdown FROM guides
         WHERE base = ?1 AND head = ?2
         ORDER BY created_at DESC, id DESC LIMIT 1",
        (merge_base.to_string(), head.to_string()),
        |row| {
            Ok((
                row.get::<_, Option<String>>(2)?,
                row.get::<_, u64>(3)?,
                row.get::<_, String>(4)?,
            ))
        },
    )
    .ok()
    .map(|(author, created_at, markdown)| Guide {
        base: *merge_base,
        head: *head,
        created_at,
        author,
        markdown,
    })
}

// --- the between-runs buffer cache -------------------------------------------
//
// Its own connection rather than the `Store`, the way the guide functions read: the
// loader thread needs it before any of the GUI's state exists.
//
// A cache, and only ever a cache. The file is the content; this holds the
// operation ids a comment's cursor names, which is the one thing a file cannot
// hold. A miss means a thread whose line was edited between runs reads as
// outdated until it is placed again; it never costs correctness, so every
// failure here is a warning and a fall-through.

/// Keep `origin`'s document, so its comments' cursors and unsaved typing outlive
/// the process.
pub fn save_buffer(git_dir: &Path, origin: &Path, saved: &concats_sync::Saved) {
    let Ok(conn) = open_db(git_dir) else {
        return;
    };
    let key = origin.to_string_lossy();
    let write = conn.execute(
        "INSERT INTO buffers (origin, snapshot, disk, updated_at) VALUES (?1,?2,?3,?4)
         ON CONFLICT(origin) DO UPDATE SET snapshot = ?2, disk = ?3, updated_at = ?4",
        (&key, &saved.snapshot, &saved.disk, now()),
    );
    if let Err(error) = write {
        eprintln!("warning: cannot cache {}: {error}", origin.display());
    }
}

pub fn load_buffer(git_dir: &Path, origin: &Path) -> Option<concats_sync::Saved> {
    let conn = open_db(git_dir).ok()?;
    let key = origin.to_string_lossy();
    let (snapshot, disk) = conn
        .query_row(
            "SELECT snapshot, disk FROM buffers WHERE origin = ?1",
            [&key],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .ok()?;
    Some(concats_sync::Saved { snapshot, disk })
}

/// Which blobs each path's rows are drawn from, in the order they appear.
///
/// A comment's own blob is often not on screen (the file was saved, or this is
/// another revision), but another revision of the same file usually is. That is
/// where its content has to be looked for.
fn blobs_by_path<'a>(rows: &'a [Row], about: Option<&'a str>) -> HashMap<&'a str, Vec<u32>> {
    let mut out: HashMap<&str, Vec<u32>> = HashMap::new();
    let mut card: Option<&str> = about;
    for row in rows {
        match row {
            Row::FileHeader { path, .. } => card = Some(path),
            Row::Code { blob, .. } => {
                if let Some(path) = card {
                    let seen = out.entry(path).or_default();
                    if !seen.contains(blob) {
                        seen.push(*blob);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The line key a comment renders under, or `None` when its lines are not on
/// screen.
///
/// Two answers, in order. A buffer holding the comment as a cursor pair is the
/// authority: the cursors ride every edit, and when they say the run is gone,
/// it is gone. Otherwise the comment names a blob and a line, and that is
/// exact — a git blob never changes, so the line is right for as long as the
/// blob is on screen. GitHub puts the thread under the last line of its range.
fn place(comment: &Comment, blobs: &[Blob], path_blobs: Option<&Vec<u32>>) -> Option<LineKey> {
    let held = path_blobs
        .into_iter()
        .flatten()
        .map(|i| &blobs[*i as usize])
        .find(|b| b.holds(comment.id));
    if let Some(blob) = held {
        return Some((blob.oid, blob.held_line(comment.id)?));
    }
    let own = blobs.iter().find(|b| b.oid == comment.anchor.blob)?;
    let line = own.anchor_line(comment.anchor.end)?;
    Some((comment.anchor.blob, line))
}

/// Where a comment's run sits in one blob: its own lines, when the blob is
/// the one the comment names. This is how a thread gets its cursor pair; from
/// then on the text carries it.
pub fn run_in(blob: &Blob, comment: &Comment) -> Option<(u32, u32)> {
    (comment.anchor.blob == blob.oid && (comment.anchor.end as usize) < blob.line_count())
        .then_some((comment.anchor.start, comment.anchor.end))
}

/// Append the threads recorded against `card` that this range could not place,
/// once its rows are done. Marked in the byline: a comment on lines that are no
/// longer on screen is a different thing from one on the code above it.
fn flush_outdated(
    out: &mut Vec<Row>,
    card: Option<&str>,
    threaded: &[&Comment],
    placed: &HashSet<u64>,
    show_all: &HashSet<String>,
) {
    let Some(path) = card.filter(|p| show_all.contains(*p)) else {
        return;
    };
    for c in threaded
        .iter()
        .filter(|c| c.path == path && !placed.contains(&thread_key(c)))
    {
        let byline = byline(c);
        out.push(Row::Comment {
            id: c.id,
            parent: c.parent,
            body: c.body.clone(),
            meta: if byline.is_empty() {
                "outdated".into()
            } else {
                format!("{byline} · outdated")
            },
        });
    }
}

/// How many threads this range cannot place for a path — what the card header
/// offers to reveal, and nothing when there is nothing hidden.
///
/// `placed` is what [`inject_comments`] reported, so the header and the stream
/// always agree about which conversations are outdated.
pub fn outdated_threads(comments: &[Comment], path: &str, placed: &HashSet<u64>) -> usize {
    comments
        .iter()
        .filter(|c| c.path == path && c.parent.is_none() && !placed.contains(&c.id))
        .count()
}

/// Who said this and, for an imported comment, where it came from. The range is
/// left out on purpose: the accent bar spanning the lines already says it. The
/// author is the one thing the strip cannot show otherwise.
fn byline(c: &Comment) -> String {
    let mut byline = c.author.clone().unwrap_or_default();
    if let Some(source) = c.external.as_deref().and_then(|e| e.split(':').next()) {
        if !byline.is_empty() {
            byline.push_str(" · ");
        }
        byline.push_str(source);
    }
    byline
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u8) -> ObjectId {
        ObjectId::from_hex(format!("{n:040x}").as_bytes()).unwrap()
    }

    fn anchor(blob: ObjectId, start: u32, end: u32) -> Anchor {
        Anchor { blob, start, end }
    }

    /// A comment ready for `add_comment`, which allocates the id.
    fn comment(path: &str, at: Anchor, body: &str) -> Comment {
        Comment {
            id: 0,
            path: path.into(),
            anchor: at,
            body: body.into(),
            author: None,
            created_at: 0,
            parent: None,
            external: None,
            cursors: None,
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = Store::open(tmp.path());
        s.toggle(&[(oid(1), 0), (oid(1), 1)]);
        let id = s.add_comment(Comment {
            author: Some("claude".into()),
            created_at: 1_700_000_000,
            external: Some("github:2181234567".into()),
            cursors: None,
            ..comment("a.txt", anchor(oid(2), 3, 5), "why though?")
        });

        let s2 = Store::open(tmp.path());
        assert_eq!(s2.seen, s.seen);
        assert_eq!(s2.comments.len(), 1);
        assert_eq!(s2.comments[0].id, id);
        assert_eq!(s2.comments[0].body, "why though?");
        assert_eq!(
            (s2.comments[0].anchor.start, s2.comments[0].anchor.end),
            (3, 5)
        );
        assert_eq!(s2.comments[0].author.as_deref(), Some("claude"));
        // The caller's timestamp survives — an import carries the time the
        // comment was really written, not the time it was ingested.
        assert_eq!(s2.comments[0].created_at, 1_700_000_000);
        assert_eq!(
            s2.comments[0].external.as_deref(),
            Some("github:2181234567")
        );
    }

    #[test]
    fn concurrent_writers_never_lose_or_collide() {
        let tmp = tempfile::tempdir().unwrap();
        let mut gui = Store::open(tmp.path());
        let mut cli = Store::open(tmp.path());

        // Ids come from the database: two writers cannot collide.
        let gui_id = gui.add_comment(comment("a.txt", anchor(oid(1), 0, 0), "mine"));
        let cli_id = cli.add_comment(comment("b.txt", anchor(oid(2), 1, 1), "theirs"));
        assert_ne!(gui_id, cli_id);

        // Each side sees the other's write as an external change to adopt.
        assert!(gui.external_change());
        assert!(gui.refresh());
        assert_eq!(gui.comments.len(), 2);

        // An external delete vanishes on refresh and never resurrects.
        cli.delete_comment(gui_id);
        gui.refresh();
        assert_eq!(gui.comments.len(), 1);
        assert_eq!(gui.comments[0].body, "theirs");
        let fresh = Store::open(tmp.path());
        assert_eq!(fresh.comments.len(), 1);
    }

    #[test]
    fn own_writes_are_not_an_external_change() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = Store::open(tmp.path());
        s.add_comment(comment("a.txt", anchor(oid(1), 0, 0), "mine"));
        assert!(!s.external_change());
        assert!(!s.refresh());
        assert_eq!(s.comments.len(), 1);
    }

    #[test]
    fn parses_user_name_from_git_config() {
        assert_eq!(
            ini_user_name("[core]\n\tbare = false\n[user]\n\tname = Ada L\n\temail = a@b.c\n"),
            Some("Ada L".to_string())
        );
        assert_eq!(ini_user_name("[user]\n\temail = a@b.c\n"), None);
        assert_eq!(
            ini_user_name("[USER]\nname = \"Quoted Name\"\n"),
            Some("Quoted Name".to_string())
        );
    }

    #[test]
    fn a_cached_buffer_round_trips_and_is_replaced_wholesale() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = std::path::Path::new("/repo/src.rs");
        let saved = concats_sync::Saved {
            snapshot: vec![1, 2, 3],
            disk: vec![4, 5],
        };
        save_buffer(tmp.path(), origin, &saved);
        let back = load_buffer(tmp.path(), origin).expect("cached");
        assert_eq!(back.snapshot, saved.snapshot);
        assert_eq!(back.disk, saved.disk);

        save_buffer(
            tmp.path(),
            origin,
            &concats_sync::Saved {
                snapshot: vec![9],
                disk: vec![4, 5],
            },
        );
        let back = load_buffer(tmp.path(), origin).expect("cached");
        assert_eq!(back.snapshot, vec![9], "the snapshot was replaced");
        assert!(load_buffer(tmp.path(), std::path::Path::new("/repo/other.rs")).is_none());
    }

    /// A comment's cursors are stored with it, copied onto a plain reply, and
    /// gone with the thread — they are the comment's, not the buffer's.
    #[test]
    fn a_comments_cursors_travel_with_the_comment() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = Store::open(tmp.path());
        let root = s.add_comment(Comment {
            cursors: Some((vec![1, 2], vec![3, 4])),
            ..comment("a.txt", anchor(oid(2), 3, 5), "here")
        });
        let reply = s
            .reply_comment(root, "and here".into(), None, 0, None)
            .unwrap();
        let bare = s.add_comment(comment("a.txt", anchor(oid(2), 9, 9), "no document"));

        let stored = Store::open(tmp.path());
        let cursors = |id: u64| {
            stored
                .comments
                .iter()
                .find(|c| c.id == id)
                .unwrap()
                .cursors
                .clone()
        };
        assert_eq!(cursors(root), Some((vec![1, 2], vec![3, 4])));
        assert_eq!(cursors(reply), Some((vec![1, 2], vec![3, 4])));
        assert_eq!(cursors(bare), None);

        // The app minted a pair for the bare one from its exact lines.
        s.set_cursors(&[(bare, (vec![5], vec![6]))]);
        let held = Store::open(tmp.path());
        let bare_now = held.comments.iter().find(|c| c.id == bare).unwrap();
        assert_eq!(bare_now.cursors, Some((vec![5], vec![6])));

        s.delete_comment(root);
        let rows: u32 = s
            .conn
            .query_row("SELECT count(*) FROM comment_cursors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            rows, 1,
            "the thread's cursors went with it; the bare one's stayed"
        );
    }

    /// Linked worktrees of one repo share an object database, so they have to
    /// share the review state keyed by it — otherwise a comment left on a blob
    /// in one checkout is missing when the same blob turns up in another.
    #[test]
    fn every_worktree_of_a_repo_resolves_to_one_database() {
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join(".git");
        let linked = shared.join("worktrees").join("feature");
        std::fs::create_dir_all(&linked).unwrap();
        // What `git worktree add` writes beside a linked worktree's git dir.
        std::fs::write(linked.join("commondir"), "../..\n").unwrap();

        let mut main = Store::open(&shared);
        let id = main.add_comment(comment("a.rs", anchor(oid(7), 1, 1), "from main"));

        let feature = Store::open(&linked);
        assert_eq!(
            feature.comments.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![id],
            "the linked worktree reads the same store"
        );
        // And a main worktree, which has no `commondir`, is left exactly as it
        // was.
        assert_eq!(common_dir(&shared), shared);
    }

    #[test]
    fn toggle_is_all_or_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = Store::open(tmp.path());
        let keys = [(oid(1), 0), (oid(1), 1)];
        // Partially seen -> toggling completes the set, not clears it.
        s.seen.insert(keys[0]);
        assert_eq!(s.state(&keys), (false, true));
        assert!(s.toggle(&keys));
        assert_eq!(s.state(&keys), (true, true));
        assert!(!s.toggle(&keys));
        assert_eq!(s.state(&keys), (false, false));
    }

    #[test]
    fn injects_comments_below_the_last_line_of_their_range() {
        let blobs = vec![Blob::new(oid(7), "txt".into(), "a\nb\nc\n".into())];
        let code = |line: u32| Row::Code {
            kind: concats_diff::LineKind::Add,
            old_no: None,
            new_no: Some(line + 1),
            blob: 0,
            line,
        };
        let mut rows = vec![code(0), code(1), code(2)];
        let comments = vec![Comment {
            id: 1,
            author: Some("claude".into()),
            ..comment("a.txt", anchor(oid(7), 0, 1), "range comment")
        }];
        inject_comments(&mut rows, &blobs, &comments, &HashSet::new(), None);
        assert_eq!(rows.len(), 4);
        assert!(matches!(&rows[2], Row::Comment { body, .. } if body == "range comment"));
        assert!(matches!(&rows[2], Row::Comment { meta, .. } if meta == "claude"));
        // Idempotent: re-injecting does not duplicate.
        inject_comments(&mut rows, &blobs, &comments, &HashSet::new(), None);
        assert_eq!(rows.len(), 4);
    }

    /// A file card: header, then one code row per line of `blob`.
    fn card(path: &str, blob: u32, lines: u32) -> Vec<Row> {
        let mut rows = vec![Row::FileHeader {
            path: path.into(),
            lang: "rust",
            adds: 0,
            dels: 0,
            from: None,
            similarity: None,
        }];
        rows.extend((0..lines).map(|line| Row::Code {
            kind: concats_diff::LineKind::Context,
            old_no: None,
            new_no: Some(line + 1),
            blob,
            line,
        }));
        rows
    }

    /// Typing on a commented line must not strand the conversation: you are
    /// answering it. Only the line going away detaches it. The held cursor
    /// pair makes that true.
    #[test]
    fn a_thread_stays_attached_while_its_line_is_being_typed_on() {
        let mut buffer = Blob::new(oid(7), "rs".into(), "fn a() {\n    let x = 1;\n}\n".into());
        let comments = vec![Comment {
            id: 1,
            ..comment("a.rs", anchor(oid(7), 1, 1), "why 1?")
        }];
        // What `hold_comments` does once, on the way in.
        buffer.hold(1, 1, 1);
        // Rename the variable on the commented line, and add a line above it.
        buffer.edit(17..18, "y");
        buffer.edit(0..0, "// note\n");

        let mut rows = card("a.rs", 0, 4);
        let placed = inject_comments(&mut rows, &[buffer], &comments, &HashSet::new(), None);
        assert_eq!(placed.len(), 1, "the thread is still placed");
        let at = rows
            .iter()
            .position(|r| matches!(r, Row::Comment { .. }))
            .expect("placed");
        assert!(
            matches!(&rows[at - 1], Row::Code { line: 2, .. }),
            "and it came down with the line it was left on"
        );
    }

    /// A comment on a blob the range does not show, held by no buffer, has no
    /// place: it is outdated for this range, never guessed onto a neighbour.
    #[test]
    fn a_thread_on_a_blob_that_is_not_on_screen_is_not_placed_at_all() {
        let now = Blob::new(oid(8), "rs".into(), "fn a() {\n}\n".into());
        let mut rows = card("a.rs", 0, 2);
        let comments = vec![Comment {
            id: 1,
            ..comment(
                "a.rs",
                anchor(oid(7), 1, 1),
                "about a line of another revision",
            )
        }];
        inject_comments(&mut rows, &[now], &comments, &HashSet::new(), None);
        assert!(!rows.iter().any(|r| matches!(r, Row::Comment { .. })));
    }

    #[test]
    fn a_comment_resolves_by_line_on_the_blob_it_names() {
        let blobs = vec![Blob::new(oid(7), "txt".into(), "a\nb\nc\n".into())];
        let mut rows = card("a.txt", 0, 3);
        let comments = vec![Comment {
            id: 1,
            ..comment("a.txt", anchor(oid(7), 1, 1), "on b")
        }];
        inject_comments(&mut rows, &blobs, &comments, &HashSet::new(), None);
        let at = rows
            .iter()
            .position(|r| matches!(r, Row::Comment { .. }))
            .expect("placed by line");
        assert!(matches!(&rows[at - 1], Row::Code { line: 1, .. }));
    }

    #[test]
    fn injects_threads_contiguously_whatever_their_ids() {
        let blobs = vec![Blob::new(oid(7), "txt".into(), "a\n".into())];
        let mut rows = vec![Row::Code {
            kind: concats_diff::LineKind::Add,
            old_no: None,
            new_no: Some(1),
            blob: 0,
            line: 0,
        }];
        // Two threads on the same line, interleaved by id — the order
        // `refresh`'s `ORDER BY id` hands them back in.
        let at = anchor(oid(7), 0, 0);
        let comments = vec![
            Comment {
                id: 1,
                ..comment("a.txt", at, "first finding")
            },
            Comment {
                id: 2,
                ..comment("a.txt", at, "second finding")
            },
            Comment {
                id: 3,
                parent: Some(1),
                ..comment("a.txt", at, "reply to the first")
            },
        ];
        inject_comments(&mut rows, &blobs, &comments, &HashSet::new(), None);
        let bodies: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Comment { body, .. } => Some(body.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            bodies,
            ["first finding", "reply to the first", "second finding"]
        );
        // The reply carries its root, so the renderer and the composer can
        // tell a thread's rows apart without consulting the store.
        assert!(matches!(&rows[2], Row::Comment { parent, .. } if *parent == Some(1)));
    }

    #[test]
    fn a_cards_outdated_threads_are_revealed_only_when_asked() {
        // The card shows blob 7; the thread was written against blob 8, which
        // this range does not show. A comment on a line that has since been
        // fixed, and that is when it matters most that it is not lost.
        let blobs = vec![Blob::new(oid(7), "txt".into(), "a\n".into())];
        let rows = || {
            vec![
                Row::FileHeader {
                    path: "a.txt".into(),
                    lang: "txt",
                    adds: 1,
                    dels: 0,
                    from: None,
                    similarity: None,
                },
                Row::Code {
                    kind: concats_diff::LineKind::Add,
                    old_no: None,
                    new_no: Some(1),
                    blob: 0,
                    line: 0,
                },
            ]
        };
        let comments = vec![
            Comment {
                id: 1,
                author: Some("octocat".into()),
                ..comment("a.txt", anchor(oid(8), 0, 0), "this leaks")
            },
            Comment {
                id: 2,
                parent: Some(1),
                author: Some("claude".into()),
                ..comment("a.txt", anchor(oid(8), 0, 0), "fixed")
            },
        ];

        let mut shut = rows();
        inject_comments(&mut shut, &blobs, &comments, &HashSet::new(), None);
        assert_eq!(shut.len(), 2, "an unplaceable thread stays hidden");

        let mut shown = rows();
        let placed = inject_comments(
            &mut shown,
            &blobs,
            &comments,
            &HashSet::from(["a.txt".to_string()]),
            None,
        );
        // The whole thread, after the file's rows, and marked as what it is.
        let seen: Vec<(&str, &str)> = shown
            .iter()
            .filter_map(|r| match r {
                Row::Comment { body, meta, .. } => Some((body.as_str(), meta.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("this leaks", "octocat · outdated"),
                ("fixed", "claude · outdated")
            ]
        );

        // And the header counts exactly what the splice could not place, so the
        // toggle is never offered for a thread that is already on screen.
        assert!(placed.is_empty(), "nothing could be placed here");
        assert_eq!(outdated_threads(&comments, "a.txt", &placed), 1);
        assert_eq!(outdated_threads(&comments, "a.txt", &HashSet::from([1])), 0);
    }

    #[test]
    fn a_reply_takes_its_roots_anchor_and_never_nests_deeper() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = Store::open(tmp.path());
        let root = s.add_comment(comment("a.txt", anchor(oid(2), 3, 5), "this leaks"));
        let reply = s
            .reply_comment(root, "fixed".into(), Some("claude".into()), 0, None)
            .unwrap();
        // A reply to a reply threads under the root, not under the reply.
        let deeper = s
            .reply_comment(reply, "thanks".into(), None, 0, None)
            .unwrap();

        let stored = Store::open(tmp.path());
        assert_eq!(stored.comments.len(), 3);
        for c in stored.comments.iter().filter(|c| c.id != root) {
            assert_eq!(c.parent, Some(root));
            assert_eq!(c.path, "a.txt");
            assert_eq!(c.anchor, anchor(oid(2), 3, 5));
            assert_eq!(thread_key(c), root);
        }
        assert_eq!(
            stored
                .comments
                .iter()
                .find(|c| c.id == deeper)
                .unwrap()
                .author,
            None
        );
        assert_eq!(s.reply_comment(9999, "nobody".into(), None, 0, None), None);
        // `root_of` is what a reply written elsewhere threads with.
        assert_eq!(s.root_of(deeper).map(|c| c.id), Some(root));
        assert_eq!(s.root_of(root).map(|c| c.id), Some(root));
        assert!(s.root_of(9999).is_none());
    }

    /// A fix changes the blob a thread was written on, so it strands the thread
    /// just when it succeeds. A reply anchored on the fixed lines brings the
    /// conversation there, and only there: in a range that still shows the
    /// original lines, the thread stays under the root.
    #[test]
    fn a_thread_renders_under_its_newest_comment_the_range_can_place() {
        let comments = vec![
            Comment {
                id: 1,
                ..comment("a.txt", anchor(oid(8), 0, 0), "this leaks")
            },
            Comment {
                id: 2,
                parent: Some(1),
                ..comment(
                    "a.txt",
                    anchor(oid(7), 2, 2),
                    "fixed: dropped on the error path",
                )
            },
        ];
        // The code line a thread's run sits under — walking back over the
        // thread's own rows to the row above its first comment.
        let under = |rows: &[Row], id: u64| -> Option<u32> {
            let mut at = rows
                .iter()
                .position(|r| matches!(r, Row::Comment { id: i, .. } if *i == id))?;
            while let Some(Row::Comment { .. }) = rows.get(at - 1) {
                at -= 1;
            }
            match rows.get(at - 1) {
                Some(Row::Code { line, .. }) => Some(*line),
                _ => None,
            }
        };

        // The range after the fix shows blob 7: the whole thread sits under
        // the reply's line, root first, contiguous.
        let fixed = vec![Blob::new(oid(7), "txt".into(), "a\nb\nc\n".into())];
        let mut rows = card("a.txt", 0, 3);
        let placed = inject_comments(&mut rows, &fixed, &comments, &HashSet::new(), None);
        assert_eq!(placed, HashSet::from([1]));
        assert_eq!(under(&rows, 1), Some(2));
        let at = rows
            .iter()
            .position(|r| matches!(r, Row::Comment { id: 1, .. }))
            .unwrap();
        assert!(matches!(&rows[at + 1], Row::Comment { id: 2, .. }));

        // The range before the fix shows blob 8: the thread stays on the
        // root's line, reply included.
        let before = vec![Blob::new(oid(8), "txt".into(), "x\n".into())];
        let mut rows = card("a.txt", 0, 1);
        let placed = inject_comments(&mut rows, &before, &comments, &HashSet::new(), None);
        assert_eq!(placed, HashSet::from([1]));
        assert_eq!(under(&rows, 1), Some(0));
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, Row::Comment { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn deleting_a_root_deletes_its_thread() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = Store::open(tmp.path());
        let root = s.add_comment(comment("a.txt", anchor(oid(2), 0, 0), "this leaks"));
        s.reply_comment(root, "fixed".into(), None, 0, None);
        let other = s.add_comment(comment("a.txt", anchor(oid(2), 0, 0), "unrelated"));

        s.delete_comment(root);
        // In memory and on disk, so the GUI shows no ghosts before the next
        // refresh.
        assert_eq!(s.comments.iter().map(|c| c.id).collect::<Vec<_>>(), [other]);
        assert_eq!(
            Store::open(tmp.path())
                .comments
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            [other]
        );
    }

    /// A comment belongs to content. Type lines above it and it has to come
    /// down with the line it was left on: the line number it was written
    /// against stops naming that line the moment anything moves.
    #[test]
    fn a_comment_follows_the_line_it_was_left_on_when_text_is_inserted_above() {
        let mut blob = Blob::new(oid(7), "md".into(), "a\nb\nMARK\nd\n".into());
        blob.origin = Some("/tmp/a.md".into());
        // Anchored on "MARK", line 2 as the file was read.
        let comments = vec![comment("a.md", anchor(oid(7), 2, 2), "here")];

        let rows = |blobs: &[Blob]| {
            let mut rows: Vec<Row> = (0..blobs[0].line_count())
                .map(|line| Row::Code {
                    kind: concats_diff::LineKind::Context,
                    old_no: None,
                    new_no: Some(line as u32 + 1),
                    blob: 0,
                    line: line as u32,
                })
                .collect();
            inject_comments(&mut rows, blobs, &comments, &HashSet::new(), None);
            // The line the comment row now sits under.
            let at = rows.iter().position(|r| matches!(r, Row::Comment { .. }))?;
            match rows.get(at - 1) {
                Some(Row::Code { line, .. }) => {
                    Some(blobs[0].line_text(*line as usize).to_string())
                }
                _ => None,
            }
        };
        assert_eq!(rows(std::slice::from_ref(&blob)).as_deref(), Some("MARK"));

        // Two lines go in above it; "MARK" is now line 4.
        blob.edit(0..0, "x\ny\n");
        assert_eq!(blob.line_text(4), "MARK");
        assert_eq!(
            rows(std::slice::from_ref(&blob)).as_deref(),
            Some("MARK"),
            "the thread came down with its line rather than staying on line 2"
        );

        // And when the line itself is typed away it has nowhere to land.
        let mut gone = blob.clone();
        let start = gone.line_starts[4] as usize;
        let end = gone.line_starts[5] as usize;
        gone.edit(start..end, "");
        assert_eq!(rows(std::slice::from_ref(&gone)), None);
    }

    /// Saving an edited file gives it a new content hash. Every anchor in this
    /// store names content, so without carrying them across, a save would
    /// silently detach every comment on the file and clear its seen ticks.
    #[test]
    fn a_save_carries_comments_and_seen_ticks_to_the_new_content() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = Store::open(tmp.path());
        let (old, new) = (oid(1), oid(2));

        let root = s.add_comment(comment("a.txt", anchor(old, 3, 3), "here"));
        s.reply_comment(root, "and here".into(), None, 0, None)
            .unwrap();
        s.toggle(&[(old, 3), (old, 9)]);

        // Two lines went in above line 3, so it now sits at 5; line 9 is gone.
        let lines: HashMap<u32, u32> = [(3, 5)].into_iter().collect();
        assert!(s.rehome(old, new, &lines));

        // The whole thread moved, reply included — it was written on the
        // root's lines.
        assert!(s.comments.iter().all(|c| c.anchor.blob == new));
        assert!(s.comments.iter().all(|c| c.anchor.start == 5));
        assert_eq!(s.comments.len(), 2);
        // The tick followed its line; the one whose line was typed away did
        // not.
        assert!(s.seen.contains(&(new, 5)));
        assert!(!s.seen.contains(&(old, 3)));
        assert!(
            s.seen.contains(&(old, 9)),
            "a tick whose line is gone is left behind rather than moved somewhere it never was"
        );
        // And it survives the process.
        let reopened = Store::open(tmp.path());
        assert!(reopened.seen.contains(&(new, 5)));
        assert!(reopened.comments.iter().all(|c| c.anchor.blob == new));

        // A reply written on lines the save typed away stays behind, on its
        // own: comments move one by one, not as a thread.
        let elsewhere = s.add_comment(Comment {
            parent: Some(root),
            ..comment("a.txt", anchor(old, 9, 9), "and this line")
        });
        assert!(
            !s.rehome(old, new, &lines),
            "nothing to carry: the reply's line was typed away"
        );
        let stayed = s.comments.iter().find(|c| c.id == elsewhere).unwrap();
        assert_eq!(stayed.anchor, anchor(old, 9, 9));
    }

    #[test]
    fn the_byline_names_the_author_and_an_imported_comments_source() {
        let local = Comment {
            author: Some("Ada L".into()),
            ..comment("a.txt", anchor(oid(1), 0, 0), "x")
        };
        assert_eq!(byline(&local), "Ada L");
        assert_eq!(
            byline(&Comment {
                external: Some("github:2181234567".into()),
                cursors: None,
                ..local.clone()
            }),
            "Ada L · github"
        );
        // Pre-author records have nothing to say.
        assert_eq!(
            byline(&comment("a.txt", anchor(oid(1), 0, 0), "x")),
            String::new()
        );
    }

    #[test]
    fn hunk_keys_covers_changed_lines_on_each_side_by_oid() {
        // A HunkBar side is (blob index, start, len); the keys it yields are
        // (blob oid, line) for start..start+len — the content-addressed anchors
        // seen-state marks. Context lines aren't part of it (the caller only
        // passes the changed runs), and an absent side contributes nothing.
        let blobs = vec![
            Blob::new(oid(1), "txt".into(), String::new()),
            Blob::new(oid(2), "txt".into(), String::new()),
        ];
        // Both sides: 2 del lines (5..=6) on blob 0, 1 add line (10) on blob 1.
        let side = |blob, start, end| Some(Side { blob, start, end });
        let keys = hunk_keys(side(0, 5, 6), side(1, 10, 10), &blobs);
        assert_eq!(keys, vec![(oid(1), 5), (oid(1), 6), (oid(2), 10)]);
        // A one-sided hunk (pure addition) keys only that side.
        assert_eq!(hunk_keys(None, side(0, 3, 3), &blobs), vec![(oid(1), 3)]);
    }
    fn guide(base: u8, head: u8, at: u64, md: &str) -> Guide {
        Guide {
            base: oid(base),
            head: oid(head),
            created_at: at,
            author: Some("claude".into()),
            markdown: md.into(),
        }
    }

    #[test]
    fn round_trips_and_newest_wins() {
        let tmp = tempfile::tempdir().unwrap();
        save_guide(tmp.path(), &guide(1, 2, 100, "first")).unwrap();
        save_guide(tmp.path(), &guide(1, 2, 200, "second")).unwrap();
        save_guide(tmp.path(), &guide(1, 3, 300, "other range")).unwrap();

        let g = latest_guide(tmp.path(), &oid(1), &oid(2)).unwrap();
        assert_eq!(g.markdown, "second");
        assert_eq!(g.author.as_deref(), Some("claude"));
        assert_eq!(guides(tmp.path()).len(), 3);
        // No guide for a range nothing was submitted against.
        assert!(latest_guide(tmp.path(), &oid(9), &oid(2)).is_none());
    }
}
