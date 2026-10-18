//! Review comments outside the GUI, so a hook can read them, an agent can
//! answer them, and they can be exported and imported from a pull request.
//!
//! What a comment anchors to, and how it exports, is
//! [`concats_review::comments`]' — the app's socket answers through the same
//! functions.

use std::process::ExitCode;

use concats_diff::load::Loaded;
use concats_review::{
    Error,
    comments::{self, Roots, Scope, Tally},
    github, interchange,
    store::{self, Comment, Store},
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
        Some(CommentsAction::Export {
            prompt,
            all,
            out,
            range,
        }) => export(prompt, all, out.as_deref(), &range),
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
        println!("no review comments recorded for {}", repo.unwrap_or("."));
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
    let loaded = super::load(&range.resolve()?)?;
    let mut store = Store::open(&loaded.git_dir);
    let c =
        comments::add(&mut store, &loaded, spec, body, author).map_err(|error| report(&error))?;
    println!("{}", comments::added(&c, true));
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

    // Only a reply written elsewhere needs the diff — its anchor is validated
    // against it like `add`'s. A plain reply takes the root's place and stays
    // cheap: no range, no load.
    let loaded = match moved {
        Some(_) => Some(super::load(&range.resolve()?)?),
        None => None,
    };
    let mut store = match &loaded {
        Some(loaded) => Store::open(&loaded.git_dir),
        None => super::open_store(range.repo.as_deref())?,
    };
    let c = comments::reply(&mut store, parent, body, author, loaded.as_ref().zip(moved))
        .map_err(|error| report(&error))?;
    println!("{}", comments::added(&c, moved.is_some()));
    Ok(())
}

/// A comment `add` or `reply` could not leave, said on stderr.
fn report(error: &Error) -> ExitCode {
    match error {
        Error::NoSuchComment(_) => {
            eprintln!("{error}");
            eprintln!("       run `concats comments` for the stored comments");
        }
        _ => eprintln!("error: {error}"),
    }
    ExitCode::from(FINDINGS)
}

/// The range's threads as one interchange document; `--all` for the whole
/// store. Every worktree shares the store, so without narrowing an agent
/// answers threads that belong to another checkout. Canonical by default;
/// `--prompt` emits the terse bot-style format instead, for handing the review
/// to an agent. Stdout by default so it pipes.
fn export(prompt: bool, all: bool, out: Option<&str>, range: &RangeArgs) -> Outcome {
    let target = range.resolve()?;
    let loaded = super::load(&target)?;
    let store = Store::open(&loaded.git_dir);

    let scope = if all { Scope::All } else { Scope::Range };
    let (entries, elsewhere) = comments::entries(&loaded, &store.comments, scope);
    if !elsewhere.is_empty() {
        let (base, head) = (&target.base, &target.head);
        if all {
            eprintln!(
                "warning: {} thread(s) do not anchor in {base}...{head} — exported as recorded, \
                 they will not re-import against this range",
                elsewhere.len()
            );
        } else {
            eprintln!(
                "note: left out {} thread(s) that do not anchor in {base}...{head} — \
                 `--all` exports the whole store",
                elsewhere.len()
            );
        }
    }

    let markdown = if prompt {
        interchange::render_prompt(&entries)
    } else {
        interchange::render(&comments::meta(&loaded), &entries)
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

    let loaded = super::load(&range.resolve()?)?;
    warn_on_range_mismatch(&doc, &comments::meta(&loaded));

    let resolved = if from_pull_request {
        resolve_pull_request_roots(&doc.entries, &loaded, input)
    } else {
        resolve_document_roots(&doc.entries, &loaded, input)?
    };

    let tally = comments::write_entries(&loaded, &resolved, &doc.entries, author, dry);
    print_tally(&tally, dry);
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
    loaded: &'a Loaded,
    input: &str,
) -> Result<Roots<'a>, ExitCode> {
    let (resolved, failed) = comments::resolve_roots(entries, loaded);
    for (entry, error) in &failed {
        print_resolution_error(entry, error, input, "error");
        if let interchange::ResolveError::MissingLines { file, .. } = error {
            eprintln!("\n{}", comments::reviewable_ranges(loaded, file));
        }
    }
    if !failed.is_empty() {
        eprintln!(
            "\n{} of {} entr{} failed to resolve — nothing imported",
            failed.len(),
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" }
        );
        return Err(ExitCode::from(FINDINGS));
    }
    Ok(resolved)
}

fn resolve_pull_request_roots<'a>(
    entries: &'a [interchange::Entry],
    loaded: &'a Loaded,
    input: &str,
) -> Roots<'a> {
    let (resolved, failed) = comments::resolve_roots(entries, loaded);
    for (entry, error) in &failed {
        print_resolution_error(entry, error, input, "warning");
    }
    if !failed.is_empty() {
        eprintln!(
            "\n{} thread(s) do not anchor in this range — skipped",
            failed.len()
        );
    }
    resolved
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

fn print_tally(tally: &Tally, dry: bool) {
    let Tally {
        added,
        updated,
        skipped,
        orphaned,
    } = *tally;
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
        println!("imported {added} comment(s), updated {updated}, skipped {skipped} duplicate(s)");
    }
}
