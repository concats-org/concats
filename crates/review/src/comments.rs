//! Comments against a loaded diff: what `concats comments` and the app's
//! socket share, so the two answer an agent the same way.
//!
//! Everything here anchors to content: a comment names a blob and a line range,
//! so it renders in every view that shows those lines. Line numbers come in as
//! 1-based new-side numbers, the same the manifest's links carry (never
//! computed by hand), and every anchor is checked against the loaded diff
//! before anything is stored.

use std::collections::{HashMap, HashSet};

use concats_diff::{
    Blob, FileChange,
    load::{INDEX_REV, Loaded, WORKTREE_REV},
};
use gix::ObjectId;

use crate::{
    Error, guide,
    interchange::{self, Entry, Meta, ResolveError, Side},
    store::{self, Anchor, Comment, Store},
};

/// `<path>:<start>[-<end>]` resolved against the loaded diff. Everything that
/// takes an anchor shares this grammar, so nothing can disagree about what
/// addresses a line.
///
/// Every line of the range must be in the diff; context lines count, the GUI
/// lets you comment on them too. Pure deletions have no new-side number, so you
/// cannot address them from here — `import` reaches them via `old L…` anchors.
/// A miss names the file's reviewable ranges, so a fix is a copy rather than
/// fresh line arithmetic.
pub fn resolve_anchor(spec: &str, loaded: &Loaded) -> Result<(String, Anchor), Error> {
    let (path_part, lines) = spec.rsplit_once(':').ok_or(Error::AnchorSyntax)?;
    let (first, last) = lines.split_once('-').unwrap_or((lines, lines));
    let (Ok(start), Ok(end)) = (first.trim().parse::<u32>(), last.trim().parse::<u32>()) else {
        return Err(Error::AnchorLines(lines.to_string()));
    };
    if start == 0 || end == 0 {
        return Err(Error::AnchorZero);
    }
    let range = start.min(end)..=start.max(end);
    interchange::resolve(loaded, path_part, Side::New, range).map_err(|error| match error {
        ResolveError::UnknownPath => Error::NotInDiff(path_part.to_string()),
        ResolveError::MissingLines { file, missing } => Error::LinesNotInDiff {
            lines: missing
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            path: file.path.clone(),
            ranges: reviewable_ranges(loaded, file),
        },
    })
}

/// A file's hunks as ready-made links, so a caller fixing an anchor never does
/// line arithmetic.
pub fn reviewable_ranges(loaded: &Loaded, file: &FileChange) -> String {
    let links: Vec<String> = file
        .hunks
        .iter()
        .map(|hunk| format!("{}\n", guide::hunk_link(file, hunk, &loaded.root)))
        .collect();
    format!(
        "The file's reviewable ranges — comment within one of these:\n\n{}",
        links.join("\n")
    )
}

/// Leave a comment on the lines `spec` names in the loaded diff.
pub fn add(
    store: &mut Store,
    loaded: &Loaded,
    spec: &str,
    body: &str,
    author: Option<String>,
) -> Result<Comment, Error> {
    let body = non_empty(body)?;
    let (path, at) = resolve_anchor(spec, loaded)?;
    let cursors = document_cursors(store, loaded, &path, at);
    store.add_comment(Comment {
        body,
        author,
        cursors,
        ..store::comment(path, at)
    })
}

/// Answer the thread `to` is in. A plain reply takes its root's place; with
/// `moved` it is anchored where that spec resolves in the loaded diff — where
/// the code lives now — and brings the thread along.
pub fn reply(
    store: &mut Store,
    to: u64,
    body: &str,
    author: Option<String>,
    moved: Option<(&Loaded, &str)>,
) -> Result<Comment, Error> {
    let body = non_empty(body)?;
    let Some((loaded, spec)) = moved else {
        return store.reply_comment(to, body, author, store::now(), None);
    };
    // The root, not the id given: a reply to a reply threads under the root.
    let thread = store.root_of(to).ok_or(Error::NoSuchComment(to))?.id;
    let (path, at) = resolve_anchor(spec, loaded)?;
    let cursors = document_cursors(store, loaded, &path, at);
    store.add_comment(Comment {
        body,
        author,
        parent: Some(thread),
        cursors,
        ..store::comment(path, at)
    })
}

/// What the CLI and the socket answer a new comment with. A root always says
/// where it went; a reply only when it was `moved` off its root's lines.
pub fn added(c: &Comment, moved: bool) -> String {
    let place = format!("{}:{}–{}", c.path, c.anchor.start + 1, c.anchor.end + 1);
    match (c.parent, moved) {
        (None, _) => format!("added comment #{} on {place}", c.id),
        (Some(thread), true) => {
            format!("added comment #{} replying to #{thread}, on {place}", c.id)
        }
        (Some(thread), false) => format!("added comment #{} replying to #{thread}", c.id),
    }
}

/// A comment body, trimmed. Whitespace is not a comment.
fn non_empty(body: &str) -> Result<String, Error> {
    let body = body.trim();
    if body.is_empty() {
        return Err(Error::EmptyBody);
    }
    Ok(body.to_string())
}

/// The comment's lines as a cursor pair in its file's document, for a comment
/// on a worktree file — minted where the app mints its own, so the comment
/// rides edits from the start. The document is the app's cached one when there
/// is one, else a fresh one on the bytes this load read; either way it is
/// cached back, so the app and the next CLI run read the same history. `None`
/// for a git blob, which never moves and needs none.
pub fn document_cursors(
    store: &Store,
    loaded: &Loaded,
    path: &str,
    at: Anchor,
) -> Option<store::Cursors> {
    let mut buffer = worktree_buffer(loaded, path)?;
    let cursors = buffer.cursors_on_disk(at.start, at.end)?;
    if let Some(saved) = buffer.saved_state() {
        store.save_buffer(buffer.origin.as_deref()?, &saved);
    }
    Some(cursors)
}

/// The worktree blob at `path`, with the app's cached document restored onto
/// it when there is one: what a comment's cursors are minted in and read from.
/// `None` for a commit range, which has no worktree side.
fn worktree_buffer(loaded: &Loaded, path: &str) -> Option<Blob> {
    let origin = loaded.workdir.as_ref()?.join(path);
    let blob = loaded
        .blobs
        .iter()
        .find(|b| b.origin.as_deref() == Some(origin.as_path()))?;
    let mut buffer = blob.clone();
    if let Some(saved) = store::load_buffer(&loaded.git_dir, &origin) {
        buffer.restore_state(&saved, blob.oid);
    }
    Some(buffer)
}

/// Where a comment sits for this export when its blob is not in the range: for
/// a comment on a worktree file whose cursors the file's document reads, the
/// lines those cursors cover in the file on disk. `None` when its blob is in
/// the range, or when nothing places it — either way it exports as recorded.
fn relocate(
    loaded: &Loaded,
    buffers: &mut HashMap<String, Option<Blob>>,
    in_range: &HashSet<ObjectId>,
    c: &Comment,
) -> Option<Anchor> {
    let (from, to) = c.cursors.as_ref()?;
    if in_range.contains(&c.anchor.blob) {
        return None;
    }
    let buffer = buffers
        .entry(c.path.clone())
        .or_insert_with(|| worktree_buffer(loaded, &c.path))
        .as_mut()?;
    if !buffer.adopt(c.id, from, to) {
        return None;
    }
    let (start, end) = buffer.held_lines_on_disk(c.id)?;
    Some(Anchor {
        blob: buffer.oid,
        start,
        end,
    })
}

/// `comments` as document entries, each where it sits now — a comment on a
/// worktree file that changed since it was made reads where its cursors sit in
/// the file, not where it was written — in the order a reviewer reads: diff
/// order, stale paths after, then by line, then by thread, each conversation
/// kept whole.
///
/// Also returns the threads none of whose comments anchor in the range — made
/// against another range, or another worktree's files, since the store is
/// shared by every worktree. Those export as recorded and will not re-import
/// against it.
pub fn entries(loaded: &Loaded, comments: &[Comment]) -> (Vec<Entry>, HashSet<u64>) {
    let rows = loaded
        .files
        .iter()
        .flat_map(|f| f.hunks.iter())
        .flat_map(|h| h.rows.iter());
    let (old, new) = interchange::blob_sides(rows, &loaded.blobs);
    let in_range: HashSet<ObjectId> = old.union(&new).copied().collect();
    let mut buffers = HashMap::new();
    let comments: Vec<Comment> = comments
        .iter()
        .map(|c| Comment {
            anchor: relocate(loaded, &mut buffers, &in_range, c).unwrap_or(c.anchor),
            ..c.clone()
        })
        .collect();
    let anchored: HashSet<u64> = comments
        .iter()
        .filter(|c| !interchange::outside_range(c, &old, &new))
        .map(store::thread_key)
        .collect();
    let elsewhere = comments
        .iter()
        .map(store::thread_key)
        .filter(|t| !anchored.contains(t))
        .collect();

    let mut entries = interchange::entries_from(&comments, &old, &new);
    let file_ix: HashMap<&str, usize> = loaded
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_str(), i))
        .collect();
    let thread = |e: &Entry| e.reply_to.or(e.id);
    entries.sort_by(|x, y| {
        let ix = |e: &Entry| file_ix.get(e.path.as_str()).copied().unwrap_or(usize::MAX);
        (ix(x), &x.path, x.start, thread(x), x.id).cmp(&(ix(y), &y.path, y.start, thread(y), y.id))
    });
    (entries, elsewhere)
}

/// The frontmatter a document carries: what repository, and which two oids the
/// anchors were resolved against.
pub fn meta(loaded: &Loaded) -> Meta {
    let endpoint = |oid: Option<ObjectId>, sentinel: &str| {
        oid.map_or_else(|| sentinel.to_string(), |o| o.to_string())
    };
    Meta {
        repo: loaded
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
        base: Some(endpoint(loaded.merge_base, INDEX_REV)),
        head: Some(endpoint(loaded.head, WORKTREE_REV)),
    }
}
