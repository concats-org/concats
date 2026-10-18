//! Comments against a loaded diff: what `concats comments` and the app's
//! socket share, so the two answer an agent the same way.
//!
//! Everything here anchors to content: a comment names a blob and a line range,
//! so it renders in every view that shows those lines. Line numbers come in as
//! 1-based new-side numbers, the same the manifest's links carry (never
//! computed by hand), or as old-side ones for deleted lines, and every anchor
//! is checked against the loaded diff before anything is stored.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use concats_diff::{
    Blob, FileChange,
    load::{self, INDEX_REV, Loaded, WORKTREE_REV},
};
use gix::ObjectId;

use crate::{
    Error, guide,
    interchange::{self, Entry, Meta, ResolveError, Side},
    store::{self, Anchor, Comment, Store},
};

/// `<path>:<start>[-<end>]` resolved against the loaded diff, or
/// `<path>@<rev>:<start>[-<end>]` for deleted lines, which have no new-side
/// number. Everything that takes an anchor shares this grammar, so nothing can
/// disagree about what addresses a line.
///
/// `<rev>` is the old side the lines were numbered on, as [`old_rev`] prints
/// it: a 4 to 40 digit prefix of its commit, or `INDEX`. It must be this
/// diff's, or the lines would be read in another version of the file.
///
/// Every line of the range must be in the diff; context lines count, the GUI
/// lets you comment on them too. A miss names the file's reviewable ranges, so
/// a fix is a copy rather than fresh line arithmetic.
pub fn resolve_anchor(spec: &str, loaded: &Loaded) -> Result<(String, Anchor), Error> {
    let (at, lines) = spec.rsplit_once(':').ok_or(Error::AnchorSyntax)?;
    let (first, last) = lines.split_once('-').unwrap_or((lines, lines));
    let (Ok(start), Ok(end)) = (first.trim().parse::<u32>(), last.trim().parse::<u32>()) else {
        return Err(Error::AnchorLines(lines.to_string()));
    };
    if start == 0 || end == 0 {
        return Err(Error::AnchorZero);
    }
    // NOTE: a path can hold an `@` of its own; only a suffix shaped like a rev
    // names the old side.
    let old = at.rsplit_once('@').filter(|(_, rev)| {
        *rev == INDEX_REV
            || (4..=40).contains(&rev.len()) && rev.bytes().all(|b| b.is_ascii_hexdigit())
    });
    let (path_part, side) = match old {
        None => (at, Side::New),
        Some((path, rev)) => {
            let ours = match loaded.merge_base {
                Some(oid) => oid.to_string()[..rev.len()].eq_ignore_ascii_case(rev),
                None => rev == INDEX_REV,
            };
            if !ours {
                return Err(Error::AnchorOldSide {
                    named: rev.to_string(),
                    old: old_rev(loaded.merge_base),
                });
            }
            (path, Side::Old)
        }
    };
    let range = start.min(end)..=start.max(end);
    interchange::resolve(loaded, path_part, side, range).map_err(|error| match error {
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

/// `c`'s lines in the grammar [`resolve_anchor`] reads, so a printed anchor
/// can be passed back. `old` is its diff's [`old_rev`] when `c`'s blob is that
/// diff's old side; `None` prints new-side lines.
pub fn anchor_spec(c: &Comment, old: Option<&str>) -> String {
    let side = old.map_or_else(String::new, |rev| format!("@{rev}"));
    let (start, end) = (c.anchor.start + 1, c.anchor.end + 1);
    format!("{}{side}:{start}-{end}", c.path)
}

/// How an anchor names a diff's old side, its [`Loaded::merge_base`]: the
/// commit's first seven hex digits, or `INDEX`. A fixed length rather than
/// git's unique abbreviation: [`resolve_anchor`] only compares it with the
/// loaded diff's own old side, so uniqueness in the object database never
/// matters.
pub fn old_rev(old: Option<ObjectId>) -> String {
    old.map_or_else(
        || INDEX_REV.to_string(),
        |oid| oid.to_hex_with_len(7).to_string(),
    )
}

/// Which side of `loaded` each of its blobs is on.
pub fn sides(loaded: &Loaded) -> HashMap<ObjectId, Side> {
    let rows = loaded
        .files
        .iter()
        .flat_map(|f| f.hunks.iter())
        .flat_map(|h| h.rows.iter());
    interchange::blob_sides(rows, &loaded.blobs)
}

/// The ids of `comments` on the old side of `base...head`, and how an anchor
/// names that side — what [`sides`] tells from a loaded diff, read off the old
/// side's tree or index and a head lookup per path instead: a comment is there
/// when its blob is one of the old side's and not the head's at its path.
pub fn on_old_side(
    repo: &Path,
    base: &str,
    head: &str,
    comments: &[Comment],
) -> Result<(String, HashSet<u64>), concats_diff::Error> {
    let root =
        load::discover(repo).ok_or_else(|| concats_diff::Error::NoRepository(repo.into()))?;
    let repo = load::open_repo(&root)?;
    let head = (head.trim() != WORKTREE_REV)
        .then(|| load::resolve(&repo, head))
        .transpose()?;
    // NOTE: the side `load` numbers deleted lines on: the index, the base
    // itself against the worktree, else the merge base.
    let old = match (base.trim() == INDEX_REV, head) {
        (true, _) => None,
        (false, None) => Some(load::resolve(&repo, base)?),
        (false, Some(head)) => Some(
            repo.merge_base(load::resolve(&repo, base)?, head)
                .map_err(|e| concats_diff::Error::git("merge_base", e))?
                .detach(),
        ),
    };

    // NOTE: every blob of the old side, not the one at a comment's path: a file
    // renamed in the range has its old blob under its old path.
    let old_blobs: HashSet<ObjectId> = match old {
        Some(commit) => load::flatten_tree(&repo, load::commit_tree(&repo, commit)?)?
            .into_values()
            .collect(),
        None => repo
            .index_or_empty()
            .map_err(|e| concats_diff::Error::git("index", e))?
            .entries()
            .iter()
            .map(|e| e.id)
            .collect(),
    };
    let was_old: Vec<&Comment> = comments
        .iter()
        .filter(|c| old_blobs.contains(&c.anchor.blob))
        .collect();
    // Only a path some comment's blob was old on is read at the head.
    let mut changed = HashMap::new();
    for c in &was_old {
        if !changed.contains_key(c.path.as_str()) {
            let now = match load::read_at_head(&repo, head, &c.path) {
                Ok((oid, _)) => Some(oid),
                // NOTE: not at the head — the range deleted the file.
                Err(concats_diff::Error::NotInTree { .. }) => None,
                Err(concats_diff::Error::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    None
                }
                Err(error) => return Err(error),
            };
            changed.insert(c.path.as_str(), now != Some(c.anchor.blob));
        }
    }
    let ids = was_old
        .iter()
        .filter(|c| changed[c.path.as_str()])
        .map(|c| c.id)
        .collect();
    Ok((old_rev(old), ids))
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
/// where it went; a reply only when it was moved off its root's lines.
/// `anchored` is the diff its lines were resolved in: always there for a root,
/// for a reply only when it moved.
pub fn added(c: &Comment, anchored: Option<&Loaded>) -> String {
    let old = anchored
        .filter(|loaded| sides(loaded).get(&c.anchor.blob) == Some(&Side::Old))
        .map(|loaded| old_rev(loaded.merge_base));
    let place = anchor_spec(c, old.as_deref());
    match (c.parent, anchored) {
        (None, _) => format!("added comment #{} on {place}", c.id),
        (Some(thread), Some(_)) => {
            format!("added comment #{} replying to #{thread}, on {place}", c.id)
        }
        (Some(thread), None) => format!("added comment #{} replying to #{thread}", c.id),
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
    sides: &HashMap<ObjectId, Side>,
    c: &Comment,
) -> Option<Anchor> {
    let cursors = c.cursors.as_ref()?;
    if sides.contains_key(&c.anchor.blob) {
        return None;
    }
    let buffer = buffers
        .entry(c.path.clone())
        .or_insert_with(|| worktree_buffer(loaded, &c.path))
        .as_mut()?;
    if !buffer.adopt(c.id, cursors) {
        return None;
    }
    let lines = buffer.held_lines_on_disk(c.id)?;
    Some(Anchor {
        blob: buffer.oid,
        start: *lines.start(),
        end: *lines.end(),
    })
}

/// Which threads an export covers.
#[derive(Clone, Copy)]
pub enum Scope<'a> {
    /// The threads that anchor in the loaded range.
    Range,
    /// Every thread in the store.
    All,
    /// The threads a window places, whatever range they anchor in.
    Shown(&'a HashSet<u64>),
}

/// The threads of `comments` that `scope` covers, as document entries, each
/// where it sits now — a comment on a worktree file that changed since it was
/// made reads where its cursors sit in the file, not where it was written — in
/// the order a reviewer reads: diff order, stale paths after, then by line,
/// then by thread, each conversation kept whole.
///
/// Also returns the threads none of whose comments anchor in the range — made
/// against another range, or another worktree's files, since the store is
/// shared by every worktree. Those export as recorded and will not re-import
/// against it; [`Scope::Range`] leaves them out.
pub fn entries(loaded: &Loaded, comments: &[Comment], scope: Scope) -> (Vec<Entry>, HashSet<u64>) {
    let sides = sides(loaded);
    let mut buffers = HashMap::new();
    let mut comments: Vec<Comment> = comments
        .iter()
        .filter(|c| match scope {
            Scope::Shown(threads) => threads.contains(&store::thread_key(c)),
            Scope::Range | Scope::All => true,
        })
        .map(|c| Comment {
            anchor: relocate(loaded, &mut buffers, &sides, c).unwrap_or(c.anchor),
            ..c.clone()
        })
        .collect();
    let anchored: HashSet<u64> = comments
        .iter()
        .filter(|c| !interchange::outside_range(c, &sides))
        .map(store::thread_key)
        .collect();
    let elsewhere: HashSet<u64> = comments
        .iter()
        .map(store::thread_key)
        .filter(|t| !anchored.contains(t))
        .collect();
    if let Scope::Range = scope {
        comments.retain(|c| !elsewhere.contains(&store::thread_key(c)));
    }

    let mut entries = interchange::entries_from(&comments, &sides);
    let file_ix: HashMap<&str, usize> = loaded
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_str(), i))
        .collect();
    let thread = |e: &Entry| e.reply_to.unwrap_or(e.id);
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

/// The root entries that anchor, each with its path and anchor in the diff.
pub type Roots<'a> = Vec<(&'a Entry, (String, Anchor))>;

/// Resolve every root entry against the loaded diff, collecting all failures
/// (like lint) instead of stopping at the first. Replies are left for
/// [`write_entries`], which anchors them where it can.
pub fn resolve_roots<'a>(
    entries: &'a [Entry],
    loaded: &'a Loaded,
) -> (Roots<'a>, Vec<(&'a Entry, ResolveError<'a>)>) {
    let mut resolved = Vec::new();
    let mut failed = Vec::new();
    for entry in entries.iter().filter(|e| e.reply_to.is_none()) {
        match interchange::resolve(loaded, &entry.path, entry.side, entry.start..=entry.end) {
            Ok(anchor) => resolved.push((entry, anchor)),
            Err(error) => failed.push((entry, error)),
        }
    }
    (resolved, failed)
}

/// Write the batch, roots first so a reply threads onto the root this run just
/// stored. Re-importing converges: an entry matching a stored comment's `ref`,
/// or its anchor and body, is skipped as a duplicate.
pub fn write_entries(
    loaded: &Loaded,
    resolved: &[(&Entry, (String, Anchor))],
    entries: &[Entry],
    fallback_author: Option<&str>,
    dry: bool,
) -> Tally {
    let mut store = Store::open(&loaded.git_dir);
    let mut tally = Tally::default();
    // Document id → store id, so a reply threads onto the root this batch just
    // wrote. Roots first, which is what the partition is for.
    let mut ids: HashMap<u64, u64> = HashMap::new();

    // Roots must anchor; a reply anchors where it can and otherwise takes its
    // root's place, so an outdated line number in a reply cannot fail a batch.
    let replies: Vec<(&Entry, Option<(String, Anchor)>)> = entries
        .iter()
        .filter(|e| e.reply_to.is_some())
        .map(|e| {
            (
                e,
                interchange::resolve(loaded, &e.path, e.side, e.start..=e.end).ok(),
            )
        })
        .collect();
    for (entry, anchor) in resolved
        .iter()
        .map(|(e, r)| (*e, Some(r)))
        .chain(replies.iter().map(|(e, r)| (*e, r.as_ref())))
    {
        // A reply's thread first: it decides where the reply goes and, for a
        // document that carries no provenance, whether it is already there.
        let parent = thread_of(entry, &store, &ids);
        if entry.reply_to.is_some() && parent.is_none() {
            tally.orphaned += 1;
            continue;
        }

        // Read the match out before writing: the store is borrowed to find it.
        let matched =
            stored_match(&store, entry, anchor, parent).map(|c| (c.id, c.body == entry.body));
        let id = match matched {
            // Same text is a duplicate; different text was edited upstream
            // since the last import, so keep the anchor and the thread and
            // take the new words.
            Some((id, true)) => {
                tally.skipped += 1;
                id
            }
            Some((id, false)) => {
                tally.updated += 1;
                if !dry {
                    store.set_body(id, entry.body.clone());
                }
                id
            }
            // A dry run still records what each entry would become, so a reply
            // finds the root it would have threaded onto and the counts match a
            // real run. The placeholder is never used as a parent: nothing writes.
            None if dry => {
                tally.added += 1;
                0
            }
            None => {
                let author = entry
                    .author
                    .clone()
                    .or_else(|| fallback_author.map(str::to_string));
                let Some(stored) = store_entry(&mut store, loaded, entry, anchor, parent, author)
                else {
                    continue;
                };
                tally.added += 1;
                stored
            }
        };
        // Where the document id landed, so a reply later in the batch threads
        // onto the root this run just wrote.
        ids.insert(entry.id, id);
    }
    tally
}

/// What a batch did, so the pass that does it can stay one pass.
#[derive(Default)]
pub struct Tally {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
    /// Replies whose thread is not in this range — nothing to attach them to.
    pub orphaned: usize,
}

/// Write one entry: at its own anchor when it has one — a root, or a reply
/// written on other lines than its root — else into `parent`'s thread at the
/// root's place. `None` when the write failed, said on stderr.
// NOTE: write_entries' write step: the entry plus the place, thread and author it resolved.
// coland-ignore: @coland/excessive_positional_parameters
fn store_entry(
    store: &mut Store,
    loaded: &Loaded,
    entry: &Entry,
    anchor: Option<&(String, Anchor)>,
    parent: Option<u64>,
    author: Option<String>,
) -> Option<u64> {
    let created_at = entry.created_at.unwrap_or_else(store::now);
    let body = entry.body.clone();
    let external = entry.external.clone();
    let stored = if let Some((path, at)) = anchor {
        let cursors = document_cursors(store, loaded, path, *at);
        store.add_comment(Comment {
            body,
            author,
            created_at,
            parent,
            external,
            cursors,
            ..store::comment(path.clone(), *at)
        })
    } else {
        let root = parent.expect("a reply without a thread was skipped above");
        store.reply_comment(root, body, author, created_at, external)
    };
    // NOTE: a write the store refused is not imported; the batch goes on, and
    // the tally counts only what landed.
    stored
        .inspect_err(|error| eprintln!("warning: {error}"))
        .ok()
        .map(|c| c.id)
}

/// The thread a reply joins, as its root's id: a comment this batch just wrote,
/// or one an earlier import stored under the same upstream id, normalized to
/// the root so a reply to a reply threads like GitHub's. `None` for a root
/// entry, and for a reply whose thread is not stored — that reply is orphaned.
// NOTE: names the threading rule write_entries' loop applies to every reply.
// coland-ignore: @coland/single_caller_private_function
fn thread_of(entry: &Entry, store: &Store, written: &HashMap<u64, u64>) -> Option<u64> {
    let doc_id = entry.reply_to?;
    let id = written.get(&doc_id).copied().or_else(|| {
        let want = interchange::provenance(doc_id);
        let stored = store
            .comments
            .iter()
            .find(|c| c.external.as_deref() == Some(&want))?;
        Some(stored.id)
    })?;
    store.root_of(id).map(|c| c.id)
}

/// The stored comment an entry would land on: the same comment upstream when
/// both carry provenance — identity, not resemblance — and otherwise the same
/// text in the same place: its anchor when it resolved to one, else its thread.
// NOTE: names the duplicate rule import's convergence rests on.
// coland-ignore: @coland/single_caller_private_function
fn stored_match<'a>(
    store: &'a Store,
    entry: &Entry,
    anchor: Option<&(String, Anchor)>,
    parent: Option<u64>,
) -> Option<&'a Comment> {
    store.comments.iter().find(|c| {
        if let (Some(have), Some(want)) = (&c.external, &entry.external) {
            return have == want;
        }
        match anchor {
            Some((_, at)) => c.anchor == *at && c.parent == parent && c.body == entry.body,
            None => c.parent == parent && c.body == entry.body,
        }
    })
}

#[cfg(test)]
mod tests {
    use concats_diff::load;
    use concats_fixture::init_repo_with_commit;

    use super::*;

    /// What `add` answered, as the anchor it printed.
    fn printed(c: &Comment, loaded: &Loaded) -> String {
        let said = added(c, Some(loaded));
        said.rsplit_once(" on ").unwrap().1.to_string()
    }

    /// An anchor copied from what `add` answers is one `add` takes back, on
    /// either side of the diff.
    #[test]
    fn a_printed_anchor_resolves_to_the_lines_it_names() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo_with_commit(repo);
        std::fs::write(repo.join("init.txt"), "a\nb\n").unwrap();
        let loaded = load::load(repo, "HEAD", WORKTREE_REV).unwrap();
        let mut store = Store::open(&loaded.git_dir);
        let rev = old_rev(loaded.merge_base);
        assert_eq!(rev, loaded.merge_base.unwrap().to_string()[..7]);

        let deleted = format!("init.txt@{rev}:1");
        for (spec, side, shown) in [
            ("init.txt:1-2", Side::New, "init.txt:1-2".to_string()),
            (deleted.as_str(), Side::Old, format!("{deleted}-1")),
        ] {
            let c = add(&mut store, &loaded, spec, "lines", None).unwrap();
            assert_eq!(sides(&loaded)[&c.anchor.blob], side);
            assert_eq!(printed(&c, &loaded), shown);
            assert_eq!(resolve_anchor(&shown, &loaded).unwrap(), (c.path, c.anchor));
        }
    }

    /// Against the unstaged changes the old side is the index, and that is
    /// what an anchor on a deleted line names.
    #[test]
    fn an_anchor_on_a_line_deleted_from_the_index_names_the_index() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo_with_commit(repo);
        std::fs::write(repo.join("init.txt"), "staged\n").unwrap();
        concats_fixture::git(repo, &["add", "init.txt"]);
        std::fs::write(repo.join("init.txt"), "unstaged\n").unwrap();
        let loaded = load::load(repo, INDEX_REV, WORKTREE_REV).unwrap();
        let mut store = Store::open(&loaded.git_dir);

        let c = add(&mut store, &loaded, "init.txt@INDEX:1", "gone", None).unwrap();
        assert_eq!(c.anchor.blob, concats_sync::hash_object(b"staged\n"));
        assert_eq!(printed(&c, &loaded), "init.txt@INDEX:1-1");
        assert_eq!(
            resolve_anchor("init.txt@INDEX:1-1", &loaded).unwrap(),
            (c.path, c.anchor)
        );
    }

    /// The CLI list tells the old side from the trees, without a diff; it
    /// must agree with the loaded diff it stands in for — a modified file, a
    /// deleted one and a renamed one, against a commit head, the worktree and
    /// the index.
    #[test]
    fn the_old_side_from_the_trees_agrees_with_the_loaded_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo_with_commit(repo);
        let git = |args: &[&str]| concats_fixture::git(repo, args);
        std::fs::write(repo.join("a.txt"), "a\nb\n").unwrap();
        std::fs::write(repo.join("gone.txt"), "x\n").unwrap();
        let lines: String = (1..=10).map(|n| format!("line {n}\n")).collect();
        std::fs::write(repo.join("moved.txt"), &lines).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        std::fs::remove_file(repo.join("gone.txt")).unwrap();
        git(&["mv", "moved.txt", "to.txt"]);
        std::fs::write(
            repo.join("to.txt"),
            &lines[..lines.len() - "line 10\n".len()],
        )
        .unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "head"]);

        let check = |base: &str, head: &str, specs: &[&str]| {
            let loaded = load::load(repo, base, head).unwrap();
            let mut store = Store::open(&loaded.git_dir);
            let rev = old_rev(loaded.merge_base);
            for spec in specs {
                add(&mut store, &loaded, &spec.replace("{rev}", &rev), "c", None).unwrap();
            }
            let sides = sides(&loaded);
            let want: HashSet<u64> = store
                .comments
                .iter()
                .filter(|c| sides.get(&c.anchor.blob) == Some(&Side::Old))
                .map(|c| c.id)
                .collect();
            let (named, ids) = on_old_side(repo, base, head, &store.comments).unwrap();
            assert_eq!((named, &ids), (rev, &want), "{base}...{head}");
            assert!(!ids.is_empty(), "{base}...{head}");
        };
        let changed = [
            "a.txt:1",
            "a.txt@{rev}:2",
            "gone.txt@{rev}:1",
            "to.txt@{rev}:10",
        ];
        check("HEAD~1", "HEAD", &changed);
        check("HEAD~1", WORKTREE_REV, &changed);

        std::fs::write(repo.join("a.txt"), "staged\n").unwrap();
        git(&["add", "a.txt"]);
        std::fs::write(repo.join("a.txt"), "unstaged\n").unwrap();
        check(INDEX_REV, WORKTREE_REV, &["a.txt:1", "a.txt@INDEX:1"]);
    }

    /// An old-side anchor printed against another range is refused, saying
    /// what it named and what this diff's old side is. An `@` that names no rev
    /// is part of the path.
    #[test]
    fn an_anchor_naming_another_old_side_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo_with_commit(repo);
        std::fs::write(repo.join("init.txt"), "a\n").unwrap();
        let loaded = load::load(repo, "HEAD", WORKTREE_REV).unwrap();
        let rev = old_rev(loaded.merge_base);
        let other = if rev.starts_with('0') { "1111" } else { "0000" };

        for named in [other, INDEX_REV] {
            match resolve_anchor(&format!("init.txt@{named}:1"), &loaded) {
                Err(error @ Error::AnchorOldSide { .. }) => {
                    let said = error.to_string();
                    assert!(said.contains(&format!("`@{named}`")), "{said}");
                    assert!(said.contains(&format!("`@{rev}`")), "{said}");
                }
                got => panic!("expected AnchorOldSide, got {got:?}"),
            }
        }
        assert!(resolve_anchor(&format!("init.txt@{}:1", &rev[..4]), &loaded).is_ok());
        assert!(matches!(
            resolve_anchor("init.txt@v2:1", &loaded),
            Err(Error::NotInDiff(path)) if path == "init.txt@v2"
        ));

        let index = load::load(repo, INDEX_REV, WORKTREE_REV).unwrap();
        assert!(matches!(
            resolve_anchor(&format!("init.txt@{rev}:1"), &index),
            Err(Error::AnchorOldSide { old, .. }) if old == INDEX_REV
        ));
    }
}
