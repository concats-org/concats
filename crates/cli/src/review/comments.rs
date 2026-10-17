//! Review comments outside the GUI, so a hook can read them, an agent can
//! answer them, and they can be exported and imported from a pull request.
//!
//! What a comment anchors to, and how it exports, is
//! [`concats_review::comments`]' — the app's socket answers through the same
//! functions.

use std::{collections::HashMap, process::ExitCode};

use concats_diff::load::Loaded;
use concats_review::{
    comments::{self, document_cursors},
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
        }) => import(&input, author.as_deref(), dry_run, &range),
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
    let (path, at) = resolve_anchor(spec, &loaded, &root)?;

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
        // NOTE: the CLI comments on lines; a quote comes from a selection,
        // which only the app has.
        quote: None,
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
        let (path, at) = resolve_anchor(spec, loaded, root)?;
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
            quote: None,
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

/// [`comments::resolve_anchor`], with its diagnostics printed.
fn resolve_anchor(spec: &str, loaded: &Loaded, root: &str) -> Result<(String, Anchor), ExitCode> {
    comments::resolve_anchor(spec, loaded, root).map_err(|error| {
        eprintln!("error: {error}");
        ExitCode::from(FINDINGS)
    })
}

/// Ranges to copy, so a caller fixing an anchor never does line arithmetic.
fn print_reviewable_ranges(file: &concats_diff::FileChange, root: &str) {
    eprintln!("\n{}", comments::reviewable_ranges(file, root));
}

/// The whole store as one interchange document. Canonical by default;
/// `--prompt` emits the terse bot-style format instead, for handing the review
/// to an agent. Stdout by default so it pipes.
fn export(prompt: bool, out: Option<&str>, range: &RangeArgs) -> Outcome {
    let target = range.resolve()?;
    let (loaded, root) = super::load(&target)?;
    let store = Store::open(&loaded.git_dir);

    let (entries, stale) = comments::entries(&loaded, &store.comments);
    if stale > 0 {
        eprintln!(
            "warning: {stale} comment(s) do not anchor in {}...{} — exported as recorded, \
             they will not re-import against this range",
            target.base, target.head
        );
    }

    let markdown = if prompt {
        interchange::render_prompt(&entries)
    } else {
        interchange::render(&comments::meta(&loaded, &root), &entries)
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

/// Bulk import: parse, resolve every entry (collecting all failures, like
/// lint), then write.
///
/// The two input profiles differ in one place. An entry a hand-written document
/// cannot anchor is a mistake worth stopping for. A pull request's outdated
/// threads come with the source, so those are reported and skipped instead of
/// failing the batch.
fn import(input: &str, author: Option<&str>, dry: bool, range: &RangeArgs) -> Outcome {
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
    warn_on_range_mismatch(&doc, &comments::meta(&loaded, &root));

    // Roots must anchor; a reply anchors where it can and otherwise takes its
    // root's place, so an outdated line number in a reply cannot fail a batch.
    let replies: Vec<_> = doc
        .entries
        .iter()
        .filter(|e| e.reply_to.is_some())
        .collect();
    let resolved = if from_pull_request {
        resolve_pull_request_roots(&doc.entries, &loaded, input)
    } else {
        resolve_document_roots(&doc.entries, &loaded, input, &root)?
    };

    write_entries(&loaded, &resolved, &replies, author, dry);
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
fn warn_on_range_mismatch(doc: &interchange::Document, loaded: &interchange::Meta) {
    for (name, have, want) in [
        ("base", &doc.meta.base, &loaded.base),
        ("head", &doc.meta.head, &loaded.head),
    ] {
        if let (Some(have), Some(want)) = (have, want)
            && have != want
        {
            eprintln!(
                "warning: document {name} {have} is not the loaded range's {want} — \
                 anchors are validated against the loaded diff"
            );
        }
    }
}

fn resolve_document_roots<'a>(
    entries: &'a [interchange::Entry],
    loaded: &Loaded,
    input: &str,
    root: &str,
) -> Result<Vec<(&'a interchange::Entry, interchange::ResolvedEntry)>, ExitCode> {
    let (resolved, failed) = resolve_roots(entries, loaded, |entry, error| {
        print_resolution_error(entry, error, input, "error");
        if let interchange::ResolveError::MissingLines { file, .. } = error {
            print_reviewable_ranges(file, root);
        }
    });
    if failed > 0 {
        eprintln!(
            "\n{failed} of {} entr{} failed to resolve — nothing imported",
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" }
        );
        return Err(ExitCode::from(FINDINGS));
    }
    Ok(resolved)
}

fn resolve_pull_request_roots<'a>(
    entries: &'a [interchange::Entry],
    loaded: &Loaded,
    input: &str,
) -> Vec<(&'a interchange::Entry, interchange::ResolvedEntry)> {
    let (resolved, failed) = resolve_roots(entries, loaded, |entry, error| {
        print_resolution_error(entry, error, input, "warning");
    });
    if failed > 0 {
        eprintln!("\n{failed} thread(s) do not anchor in this range — skipped");
    }
    resolved
}

fn resolve_roots<'a>(
    entries: &'a [interchange::Entry],
    loaded: &Loaded,
    mut on_error: impl FnMut(&interchange::Entry, &interchange::ResolveError<'_>),
) -> (
    Vec<(&'a interchange::Entry, interchange::ResolvedEntry)>,
    usize,
) {
    let mut failed = 0;
    let resolved = entries
        .iter()
        .filter(|e| e.reply_to.is_none())
        .filter_map(|entry| match interchange::resolve_entry(loaded, entry) {
            Ok(anchor) => Some((entry, anchor)),
            Err(error) => {
                failed += 1;
                on_error(entry, &error);
                None
            }
        })
        .collect();
    (resolved, failed)
}

fn print_resolution_error(
    entry: &interchange::Entry,
    error: &interchange::ResolveError<'_>,
    input: &str,
    level: &str,
) {
    match error {
        interchange::ResolveError::UnknownPath => {
            eprintln!(
                "{level}: {input}:{}: `{}` is not part of this diff",
                entry.line, entry.path
            );
        }
        interchange::ResolveError::MissingLines { file, missing } => {
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
        }
    }
}

/// Write the batch, roots first so a reply threads onto the root this run just
/// stored. Re-importing converges: an entry matching a stored comment's `ref`,
/// or its anchor and body, is skipped as a duplicate.
fn write_entries(
    loaded: &concats_diff::load::Loaded,
    resolved: &[(&interchange::Entry, interchange::ResolvedEntry)],
    replies: &[&interchange::Entry],
    fallback_author: Option<&str>,
    dry: bool,
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

        let author = entry
            .author
            .clone()
            .or_else(|| fallback_author.map(str::to_string));
        // A dry run still records what each entry would become, so a reply
        // finds the root it would have threaded onto and the counts match a
        // real run. The placeholder is never used as a parent: nothing writes.
        let id = if dry {
            0
        } else if let Some(stored) = store_entry(&mut store, loaded, entry, anchor, parent, author)
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
    anchor: Option<&interchange::ResolvedEntry>,
    parent: Option<u64>,
    author: Option<String>,
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
        // NOTE: an imported entry names lines; a quote is the app's selection.
        quote: None,
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
