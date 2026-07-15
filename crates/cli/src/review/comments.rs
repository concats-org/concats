//! Review comments outside the GUI, so a hook can read them, an agent can
//! answer them, and they can be exported and imported from a pull request.
//!
//! Everything here anchors to content: a comment names a blob and a line range,
//! so it renders in every view that shows those lines. Line numbers come in as
//! 1-based new-side numbers, the same the manifest's links carry (never
//! computed by hand), and every anchor is checked against the loaded diff
//! before anything is stored.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    process::ExitCode,
};

use concats_diff::{Blob, load::Loaded};
use concats_review::{
    github, interchange,
    store::{self, Anchor, Comment, Store},
};

use super::{BAD_INPUT, CommentsAction, FINDINGS, Outcome, RangeArgs};

pub(crate) fn run(
    action: Option<CommentsAction>,
    repo: Option<&str>,
    delete: Option<u64>,
) -> Outcome {
    match action {
        Some(CommentsAction::Add {
            anchor,
            body,
            author,
            range,
        }) => add(&anchor, &body, author, &range),
        Some(CommentsAction::Reply {
            id,
            anchor,
            body,
            author,
            range,
        }) => reply(&id, anchor.as_deref(), &body, author, &range),
        Some(CommentsAction::Export { prompt, out, range }) => {
            export(prompt, out.as_deref(), &range)
        }
        Some(CommentsAction::Import {
            input,
            author,
            dry_run,
            range,
        }) => import(&input, author, dry_run, &range),
        None => list(repo, delete),
    }
}

/// The stored comments, threads whole — the same shape the app renders.
fn list(repo: Option<&str>, delete: Option<u64>) -> Outcome {
    let mut store = super::open_store(repo)?;

    if let Some(id) = delete {
        let before = store.comments.len();
        store.delete_comment(id);
        if store.comments.len() == before {
            eprintln!("no comment with id {id}");
            return Err(ExitCode::from(FINDINGS));
        }
        println!("deleted comment {id}");
        return Ok(());
    }

    if store.comments.is_empty() {
        println!("no review comments recorded for {}", super::repo_arg(repo));
        return Ok(());
    }
    // Threads read in order, replies indented under their root.
    let mut threaded: Vec<&Comment> = store.comments.iter().collect();
    threaded.sort_by_key(|c| (store::thread_key(c), c.id));
    for c in threaded {
        let lead = if c.parent.is_some() { "    " } else { "" };
        let by = c
            .author
            .as_ref()
            .map_or_else(String::new, |a| format!("  by {a}"));
        let from = c
            .external
            .as_ref()
            .map_or_else(String::new, |e| format!("  [{e}]"));
        println!(
            "{lead}#{}  {}:{}–{}  (blob {}){by}{from}\n{lead}    {}\n",
            c.id,
            c.path,
            c.anchor.start + 1,
            c.anchor.end + 1,
            &c.anchor.blob.to_string()[..10],
            c.body.replace('\n', &format!("\n{lead}    "))
        );
    }
    println!("{} comment(s)", store.comments.len());
    Ok(())
}

fn add(spec: &str, body: &str, author: Option<String>, range: &RangeArgs) -> Outcome {
    let body = non_empty(body)?;
    let (loaded, root) = super::load(&range.resolve()?)?;
    let Some((path, at)) = resolve_anchor(spec, &loaded, &root) else {
        return Err(ExitCode::from(FINDINGS));
    };

    let mut store = Store::open(&loaded.git_dir);
    let id = store.add_comment(Comment {
        id: 0,
        path: path.clone(),
        anchor: at,
        body,
        author,
        created_at: store::now(),
        parent: None,
        external: None,
        cursors: document_cursors(&loaded, &path, at),
    });
    println!(
        "added comment #{id} on {path}:{}–{}",
        at.start + 1,
        at.end + 1
    );
    Ok(())
}

fn reply(
    id: &str,
    moved: Option<&str>,
    body: &str,
    author: Option<String>,
    range: &RangeArgs,
) -> Outcome {
    let Ok(parent) = id.trim_start_matches('#').parse::<u64>() else {
        eprintln!("error: `{id}` is not a comment id — run `concats comments` for the stored ones");
        return Err(ExitCode::from(BAD_INPUT));
    };
    let body = non_empty(body)?;

    // Only a reply written elsewhere needs the diff — its anchor is validated
    // against it like `add`'s. A plain reply takes the root's place and stays
    // cheap: no range, no load.
    let diff = match moved {
        Some(_) => Some(super::load(&range.resolve()?)?),
        None => None,
    };
    let mut store = match &diff {
        Some((loaded, _)) => Store::open(&loaded.git_dir),
        None => super::open_store(range.repo.as_deref())?,
    };
    // The root, not the id given: a reply to a reply threads under the root.
    let Some(thread) = store.root_of(parent).map(|c| c.id) else {
        return Err(no_such_comment(parent));
    };

    if let (Some(spec), Some((loaded, root))) = (moved, &diff) {
        let Some((path, at)) = resolve_anchor(spec, loaded, root) else {
            return Err(ExitCode::from(FINDINGS));
        };
        let id = store.add_comment(Comment {
            id: 0,
            path: path.clone(),
            anchor: at,
            body,
            author,
            created_at: store::now(),
            parent: Some(thread),
            external: None,
            cursors: document_cursors(loaded, &path, at),
        });
        println!(
            "added comment #{id} replying to #{thread}, on {path}:{}–{}",
            at.start + 1,
            at.end + 1
        );
        return Ok(());
    }

    let Some(id) = store.reply_comment(parent, body, author, store::now(), None) else {
        return Err(no_such_comment(parent));
    };
    println!("added comment #{id} replying to #{thread}");
    Ok(())
}

fn no_such_comment(id: u64) -> ExitCode {
    eprintln!("no comment with id {id}");
    eprintln!("       run `concats comments` for the stored comments");
    ExitCode::from(FINDINGS)
}

/// A comment body, trimmed. Whitespace is not a comment.
fn non_empty(body: &str) -> Result<String, ExitCode> {
    let body = body.trim();
    if body.is_empty() {
        eprintln!("error: empty comment body");
        return Err(ExitCode::from(FINDINGS));
    }
    Ok(body.to_string())
}

/// `<path>:<start>[-<end>]` resolved against the loaded diff. `add` and `reply`
/// share this grammar, so they cannot disagree about what addresses a line.
///
/// Every line of the range must be in the diff; context lines count, the GUI
/// lets you comment on them too. Pure deletions have no new-side number, so you
/// cannot address them from here — `import` reaches them via `old L…` anchors.
/// Prints its own diagnostics, including the file's reviewable ranges, so a fix
/// is a copy rather than fresh line arithmetic.
fn resolve_anchor(
    spec: &str,
    loaded: &concats_diff::load::Loaded,
    root: &str,
) -> Option<(String, Anchor)> {
    let Some((path_part, lines)) = spec.rsplit_once(':') else {
        eprintln!("error: anchor must be <path>:<start>[-<end>]");
        return None;
    };
    let (first, last) = lines.split_once('-').unwrap_or((lines, lines));
    let (Ok(start), Ok(end)) = (first.trim().parse::<u32>(), last.trim().parse::<u32>()) else {
        eprintln!("error: cannot parse line range `{lines}` — lines are 1-based new-side numbers");
        return None;
    };
    if start == 0 || end == 0 {
        eprintln!("error: lines are 1-based");
        return None;
    }
    let (start, end) = (start.min(end), start.max(end));

    let Some(file) = concats_review::guide::find_file(loaded, path_part.trim_end_matches('/'))
    else {
        eprintln!("error: `{path_part}` is not part of this diff");
        eprintln!("       run `concats manifest` for the reviewable files");
        return None;
    };
    let (new_side, _) = interchange::line_maps(file);
    let missing: Vec<u32> = (start..=end)
        .filter(|n| !new_side.contains_key(n))
        .collect();
    if !missing.is_empty() {
        let shown: Vec<String> = missing.iter().take(5).map(u32::to_string).collect();
        eprintln!(
            "error: line(s) {} of {} are not part of this diff",
            shown.join(", "),
            file.path
        );
        print_reviewable_ranges(file, root);
        return None;
    }
    let (blob, line_start) = new_side[&start];
    let (_, line_end) = new_side[&end];
    Some((
        file.path.clone(),
        Anchor {
            blob: loaded.blobs[blob as usize].oid,
            start: line_start,
            end: line_end,
        },
    ))
}

/// The comment's lines as a cursor pair in its file's document, for a comment
/// on a worktree file — minted where the app mints its own, so the comment
/// rides edits from the start. The document is the app's cached one when there
/// is one, else a fresh one on the bytes this load read; either way it is
/// cached back, so the app and the next CLI run read the same history. `None`
/// for a git blob, which never moves and needs none.
fn document_cursors(loaded: &Loaded, path: &str, at: Anchor) -> Option<store::Cursors> {
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
    in_range: &HashSet<gix::ObjectId>,
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

/// Ranges to copy, so a caller fixing an anchor never does line arithmetic.
fn print_reviewable_ranges(file: &concats_diff::FileChange, root: &str) {
    eprintln!("\nThe file's reviewable ranges — comment within one of these:\n");
    for hunk in &file.hunks {
        eprintln!("{}\n", concats_review::guide::hunk_link(file, hunk, root));
    }
}

/// The whole store as one interchange document. Canonical by default;
/// `--prompt` emits the terse bot-style format instead, for handing the review
/// to an agent. Stdout by default so it pipes.
fn export(prompt: bool, out: Option<&str>, range: &RangeArgs) -> Outcome {
    let target = range.resolve()?;
    let (loaded, root) = super::load(&target)?;
    let store = Store::open(&loaded.git_dir);

    let rows = loaded
        .files
        .iter()
        .flat_map(|f| f.hunks.iter())
        .flat_map(|h| h.rows.iter());
    let (old, new) = interchange::blob_sides(rows, &loaded.blobs);
    // A comment on a worktree file that changed since it was made reads where
    // its cursors sit in the file now, not where it was written.
    let in_range: HashSet<gix::ObjectId> = old.union(&new).copied().collect();
    let mut buffers = HashMap::new();
    let comments: Vec<Comment> = store
        .comments
        .iter()
        .map(|c| relocate(&loaded, &mut buffers, &in_range, c))
        .collect();
    let stale = comments
        .iter()
        .filter(|c| interchange::outside_range(c, &old, &new))
        .count();
    if stale > 0 {
        eprintln!(
            "warning: {stale} comment(s) do not anchor in {}...{} — exported as recorded, \
             they will not re-import against this range",
            target.base, target.head
        );
    }

    let mut entries = interchange::entries_from(&comments, &old, &new);
    // Diff order, stale paths after, then by line, then by thread — the order a
    // reviewer reads, with each conversation kept whole.
    let file_ix: HashMap<&str, usize> = loaded
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_str(), i))
        .collect();
    let thread = |e: &interchange::Entry| e.reply_to.or(e.id);
    entries.sort_by(|x, y| {
        let ix =
            |e: &interchange::Entry| file_ix.get(e.path.as_str()).copied().unwrap_or(usize::MAX);
        (ix(x), &x.path, x.start, thread(x), x.id).cmp(&(ix(y), &y.path, y.start, thread(y), y.id))
    });

    let markdown = if prompt {
        interchange::render_prompt(&entries)
    } else {
        interchange::render(&document_meta(&loaded, &root), &entries)
    };

    match out {
        Some(path) => {
            if let Err(error) = std::fs::write(path, markdown) {
                eprintln!("error: cannot write {path}: {error}");
                return Err(ExitCode::from(BAD_INPUT));
            }
            println!("exported {} comment(s) to {path}", entries.len());
        }
        None => print!("{markdown}"),
    }
    Ok(())
}

/// The frontmatter an export carries: what repository, and which two oids the
/// anchors were resolved against.
fn document_meta(loaded: &concats_diff::load::Loaded, root: &str) -> interchange::Meta {
    interchange::Meta {
        repo: Path::new(root)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
        base: Some(endpoint(loaded.merge_base, concats_diff::load::INDEX_REV)),
        head: Some(endpoint(loaded.head, concats_diff::load::WORKTREE_REV)),
    }
}

fn endpoint(oid: Option<gix::ObjectId>, sentinel: &str) -> String {
    oid.map_or_else(|| sentinel.to_string(), |o| o.to_string())
}

/// Bulk import: parse, resolve every entry (collecting all failures, like
/// lint), then write.
///
/// The two input profiles differ in one place. An entry a hand-written document
/// cannot anchor is a mistake worth stopping for. A pull request's outdated
/// threads come with the source, so those are reported and skipped instead of
/// failing the batch.
fn import(input: &str, author: Option<String>, dry: bool, range: &RangeArgs) -> Outcome {
    let text = read_input(input)?;
    // A payload, not a document: `gh api` output starts with its array.
    let from_pull_request = text.trim_start().starts_with(['[', '{']);
    let parsed = if from_pull_request {
        github::parse(&text)
    } else {
        interchange::parse(&text)
    };
    let doc = match parsed {
        Ok(doc) => doc,
        Err(error) => {
            eprintln!("error: {error}");
            return Err(ExitCode::from(FINDINGS));
        }
    };
    for warning in &doc.warnings {
        // A markdown warning names its source line; a payload's names a comment
        // id, and there is no `{input}` line to blame.
        if from_pull_request {
            eprintln!("warning: {warning}");
        } else {
            eprintln!("warning: {input}:{warning}");
        }
    }
    if doc.entries.is_empty() {
        eprintln!("error: no comment entries found in {input}");
        return Err(ExitCode::from(FINDINGS));
    }

    let (loaded, root) = super::load(&range.resolve()?)?;
    warn_on_range_mismatch(&doc, &loaded);

    // Roots must anchor; a reply anchors where it can and otherwise takes its
    // root's place, so an outdated line number in a reply cannot fail a batch.
    let (roots, replies): (Vec<_>, Vec<_>) = doc.entries.iter().partition(|e| e.reply_to.is_none());
    let (resolved, failed) = resolve_roots(&roots, &loaded, input, &root, from_pull_request);
    if failed > 0 {
        if !from_pull_request {
            eprintln!(
                "\n{failed} of {} entr{} failed to resolve — nothing imported",
                doc.entries.len(),
                if doc.entries.len() == 1 { "y" } else { "ies" }
            );
            return Err(ExitCode::from(FINDINGS));
        }
        eprintln!("\n{failed} thread(s) do not anchor in this range — skipped");
    }

    write_entries(&loaded, &resolved, &replies, (author, dry));
    Ok(())
}

fn read_input(input: &str) -> Result<String, ExitCode> {
    if input == "-" {
        return std::io::read_to_string(std::io::stdin()).map_err(|_| {
            eprintln!("error: cannot read stdin");
            ExitCode::from(BAD_INPUT)
        });
    }
    std::fs::read_to_string(input).map_err(|error| {
        eprintln!("error: cannot read {input}: {error}");
        ExitCode::from(BAD_INPUT)
    })
}

/// A document that names endpoints is checked against the loaded ones — the
/// anchors are validated against the diff either way, so this is a warning
/// rather than a refusal.
fn warn_on_range_mismatch(doc: &interchange::Document, loaded: &concats_diff::load::Loaded) {
    for (name, have, want) in [
        (
            "base",
            &doc.meta.base,
            endpoint(loaded.merge_base, concats_diff::load::INDEX_REV),
        ),
        (
            "head",
            &doc.meta.head,
            endpoint(loaded.head, concats_diff::load::WORKTREE_REV),
        ),
    ] {
        if have.as_ref().is_some_and(|have| *have != want) {
            let have = have.as_ref().expect("checked");
            eprintln!(
                "warning: document {name} {have} is not the loaded range's {want} — \
                 anchors are validated against the loaded diff"
            );
        }
    }
}

/// Every root entry that anchors, and how many did not. A failure gets the same
/// reviewable-ranges help `add` prints, so the fix is a copy.
fn resolve_roots<'a>(
    roots: &[&'a interchange::Entry],
    loaded: &concats_diff::load::Loaded,
    input: &str,
    root: &str,
    from_pull_request: bool,
) -> (
    Vec<(&'a interchange::Entry, interchange::ResolvedEntry)>,
    usize,
) {
    let level = if from_pull_request {
        "warning"
    } else {
        "error"
    };
    let mut resolved = Vec::new();
    let mut failed = 0usize;
    for entry in roots {
        match interchange::resolve_entry(loaded, entry) {
            Ok(r) => resolved.push((*entry, r)),
            Err(interchange::ResolveError::UnknownPath) => {
                failed += 1;
                eprintln!(
                    "{level}: {input}:{}: `{}` is not part of this diff",
                    entry.line, entry.path
                );
            }
            Err(interchange::ResolveError::MissingLines { file, missing }) => {
                failed += 1;
                let shown: Vec<String> = missing.iter().take(5).map(u32::to_string).collect();
                let side = if entry.side == interchange::Side::Old {
                    "old-side "
                } else {
                    ""
                };
                eprintln!(
                    "{level}: {input}:{}: {side}line(s) {} of {} are not part of this diff",
                    entry.line,
                    shown.join(", "),
                    file.path
                );
                // Ranges to copy help someone fixing a document; nobody can fix
                // a pull request comment that the diff has moved past.
                if !from_pull_request {
                    print_reviewable_ranges(file, root);
                }
            }
        }
    }
    (resolved, failed)
}

/// Write the batch, roots first so a reply threads onto the root this run just
/// stored. Re-importing converges: an entry matching a stored comment's `ref`,
/// or its anchor and body, is skipped as a duplicate.
fn write_entries(
    loaded: &concats_diff::load::Loaded,
    resolved: &[(&interchange::Entry, interchange::ResolvedEntry)],
    replies: &[&interchange::Entry],
    (fallback_author, dry): (Option<String>, bool),
) {
    let mut store = Store::open(&loaded.git_dir);
    let mut tally = Tally::default();
    // Document id → store id, so a reply threads onto the root this batch just
    // wrote. Roots first, which is what the partition is for.
    let mut ids: HashMap<u64, u64> = HashMap::new();

    let replies: Vec<(&interchange::Entry, Option<interchange::ResolvedEntry>)> = replies
        .iter()
        .map(|e| (*e, interchange::resolve_entry(loaded, e).ok()))
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
        if let Some((id, same)) =
            stored_match(&store, entry, anchor, parent).map(|c| (c.id, c.body == entry.body))
        {
            // Same text is a duplicate; different text was edited upstream
            // since the last import, so keep the anchor and the thread and
            // take the new words.
            if same {
                tally.skipped += 1;
            } else {
                tally.updated += 1;
                if !dry {
                    store.set_body(id, entry.body.clone());
                }
            }
            remember(&mut ids, entry, id);
            continue;
        }

        let author = entry.author.clone().or_else(|| fallback_author.clone());
        // A dry run still records what each entry would become, so a reply
        // finds the root it would have threaded onto and the counts match a
        // real run. The placeholder is never used as a parent: nothing writes.
        let id = if dry {
            0
        } else if let Some(stored) =
            store_entry(&mut store, loaded, entry, (anchor, parent, author))
        {
            stored
        } else {
            continue;
        };
        remember(&mut ids, entry, id);
        tally.added += 1;
    }

    tally.report(dry);
}

/// What a batch did, so the pass that does it can stay one pass.
#[derive(Default)]
struct Tally {
    added: usize,
    updated: usize,
    skipped: usize,
    /// Replies whose thread is not in this range — nothing to attach them to.
    orphaned: usize,
}

impl Tally {
    fn report(&self, dry: bool) {
        let Self {
            added,
            updated,
            skipped,
            orphaned,
        } = *self;
        if orphaned > 0 {
            eprintln!(
                "warning: {orphaned} repl{} could not be threaded — the comment they \
                 answer is not in this range",
                if orphaned == 1 { "y" } else { "ies" }
            );
        }
        if dry {
            println!(
                "dry run — would import {added} comment(s), update {updated}, \
                 skip {skipped} duplicate(s)"
            );
        } else {
            println!(
                "imported {added} comment(s), updated {updated}, skipped {skipped} duplicate(s)"
            );
        }
    }
}

/// Note where a document id landed, so a reply later in the batch threads onto
/// the root this run just wrote.
fn remember(ids: &mut HashMap<u64, u64>, entry: &interchange::Entry, id: u64) {
    if let Some(doc_id) = entry.id {
        ids.insert(doc_id, id);
    }
}

/// Write one entry: at its own anchor when it has one — a root, or a reply
/// written on other lines than its root — else into `parent`'s thread at the
/// root's place. `None` when the thread went away between resolving and
/// writing.
fn store_entry(
    store: &mut Store,
    loaded: &concats_diff::load::Loaded,
    entry: &interchange::Entry,
    (anchor, parent, author): (
        Option<&interchange::ResolvedEntry>,
        Option<u64>,
        Option<String>,
    ),
) -> Option<u64> {
    let created_at = entry.created_at.unwrap_or_else(store::now);
    let Some(r) = anchor else {
        let root = parent.expect("a reply without a thread was skipped above");
        return store.reply_comment(
            root,
            entry.body.clone(),
            author,
            created_at,
            entry.external.clone(),
        );
    };
    let at = Anchor {
        blob: r.blob,
        start: r.start,
        end: r.end,
    };
    Some(store.add_comment(Comment {
        id: 0,
        path: r.path.clone(),
        anchor: at,
        body: entry.body.clone(),
        author,
        created_at,
        parent,
        external: entry.external.clone(),
        cursors: document_cursors(loaded, &r.path, at),
    }))
}

/// The thread a reply joins, as its root's id: a comment this batch just wrote,
/// or one an earlier import stored under the same upstream id, normalized to
/// the root so a reply to a reply threads like GitHub's. `None` for a root
/// entry, and for a reply whose thread is not stored — that reply is orphaned.
fn thread_of(
    entry: &interchange::Entry,
    store: &Store,
    written: &HashMap<u64, u64>,
) -> Option<u64> {
    let doc_id = entry.reply_to?;
    let id = written.get(&doc_id).copied().or_else(|| {
        let want = github::provenance(doc_id);
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
fn stored_match<'a>(
    store: &'a Store,
    entry: &interchange::Entry,
    anchor: Option<&interchange::ResolvedEntry>,
    parent: Option<u64>,
) -> Option<&'a Comment> {
    store.comments.iter().find(|c| {
        if let (Some(have), Some(want)) = (&c.external, &entry.external) {
            return have == want;
        }
        match anchor {
            Some(r) => {
                c.anchor.blob == r.blob
                    && (c.anchor.start, c.anchor.end) == (r.start, r.end)
                    && c.parent == parent
                    && c.body == entry.body
            }
            None => c.parent == parent && c.body == entry.body,
        }
    })
}
