//! Comments against a loaded diff: what `concats comments` and the app's
//! socket share, so the two answer an agent the same way.
//!
//! Everything here anchors to content: a comment names a blob and a line range,
//! so it renders in every view that shows those lines. Line numbers come in as
//! 1-based new-side numbers, the same the manifest's links carry (never
//! computed by hand), and every anchor is checked against the loaded diff
//! before anything is stored.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use concats_diff::{
    Blob, FileChange,
    load::{INDEX_REV, Loaded, WORKTREE_REV},
};
use gix::ObjectId;

use crate::{
    Error, guide,
    interchange::{self, Entry, Meta},
    store::{self, Anchor, Comment},
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
pub fn resolve_anchor(spec: &str, loaded: &Loaded, root: &str) -> Result<(String, Anchor), Error> {
    let (path_part, lines) = spec.rsplit_once(':').ok_or(Error::AnchorSyntax)?;
    let (first, last) = lines.split_once('-').unwrap_or((lines, lines));
    let (Ok(start), Ok(end)) = (first.trim().parse::<u32>(), last.trim().parse::<u32>()) else {
        return Err(Error::AnchorLines(lines.to_string()));
    };
    if start == 0 || end == 0 {
        return Err(Error::AnchorZero);
    }
    let (start, end) = (start.min(end), start.max(end));

    let file = guide::find_file(loaded, path_part.trim_end_matches('/'))
        .ok_or_else(|| Error::NotInDiff(path_part.to_string()))?;
    let (new_side, _) = interchange::line_maps(file);
    let missing: Vec<String> = (start..=end)
        .filter(|n| !new_side.contains_key(n))
        .take(5)
        .map(|n| n.to_string())
        .collect();
    if !missing.is_empty() {
        return Err(Error::LinesNotInDiff {
            lines: missing.join(", "),
            path: file.path.clone(),
            ranges: reviewable_ranges(file, root),
        });
    }
    let (blob, line_start) = new_side[&start];
    let (_, line_end) = new_side[&end];
    Ok((
        file.path.clone(),
        Anchor {
            blob: loaded.blobs[blob as usize].oid,
            start: line_start,
            end: line_end,
        },
    ))
}

/// A file's hunks as ready-made links, so a caller fixing an anchor never does
/// line arithmetic.
pub fn reviewable_ranges(file: &FileChange, root: &str) -> String {
    let links: Vec<String> = file
        .hunks
        .iter()
        .map(|hunk| format!("{}\n", guide::hunk_link(file, hunk, root)))
        .collect();
    format!(
        "The file's reviewable ranges — comment within one of these:\n\n{}",
        links.join("\n")
    )
}

/// The comment's lines as a cursor pair in its file's document, for a comment
/// on a worktree file — minted where the app mints its own, so the comment
/// rides edits from the start. The document is the app's cached one when there
/// is one, else a fresh one on the bytes this load read; either way it is
/// cached back, so the app and the next CLI run read the same history. `None`
/// for a git blob, which never moves and needs none.
pub fn document_cursors(loaded: &Loaded, path: &str, at: Anchor) -> Option<store::Cursors> {
    let mut buffer = worktree_buffer(loaded, path)?;
    let cursors = buffer.cursors_on_disk(at.start, at.end)?;
    if let Some(saved) = buffer.saved_state() {
        store::save_buffer(&loaded.git_dir, buffer.origin.as_deref()?, &saved);
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

/// Where a comment sits for this export: its own lines when its blob is in the
/// range, else — for a comment on a worktree file whose cursors the file's
/// document reads — the lines those cursors cover in the file on disk. A
/// comment the range cannot place either way exports as recorded.
fn relocate(
    loaded: &Loaded,
    buffers: &mut HashMap<String, Option<Blob>>,
    in_range: &HashSet<ObjectId>,
    c: &Comment,
) -> Comment {
    let Some((from, to)) = &c.cursors else {
        return c.clone();
    };
    if in_range.contains(&c.anchor.blob) {
        return c.clone();
    }
    let buffer = buffers
        .entry(c.path.clone())
        .or_insert_with(|| worktree_buffer(loaded, &c.path));
    let Some(buffer) = buffer else {
        return c.clone();
    };
    if !buffer.adopt(c.id, from, to) {
        return c.clone();
    }
    match buffer.held_lines_on_disk(c.id) {
        Some((start, end)) => Comment {
            anchor: Anchor {
                blob: buffer.oid,
                start,
                end,
            },
            ..c.clone()
        },
        None => c.clone(),
    }
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
        .map(|c| relocate(loaded, &mut buffers, &in_range, c))
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
pub fn meta(loaded: &Loaded, root: &str) -> Meta {
    let endpoint = |oid: Option<ObjectId>, sentinel: &str| {
        oid.map_or_else(|| sentinel.to_string(), |o| o.to_string())
    };
    Meta {
        repo: Path::new(root)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
        base: Some(endpoint(loaded.merge_base, INDEX_REV)),
        head: Some(endpoint(loaded.head, WORKTREE_REV)),
    }
}
