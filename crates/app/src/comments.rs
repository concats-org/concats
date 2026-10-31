//! Review comments in the document: held by the buffers they sit on,
//! spliced below their lines in every stream, and framed as cards of their
//! code in the Comments tab.

use std::collections::HashSet;

use concats_diff::{Blob, LineKind, Row, Side};
use concats_review::{
    interchange,
    store::{self, Comment},
};

use crate::review_doc::{ReviewDoc, Stream, finalize_cards, splice_composer, strip_composer};

/// Hand every comment on an editable file to that file's text, as a cursor
/// pair.
///
/// A comment that carries cursors — minted when it was made, here or by the
/// CLI in the same document — is adopted as it is. One that does not is held by
/// the lines it names on the blob it names, once. Either way the document
/// carries it from then on: the run stretches and shrinks with the text, and
/// only deleting the text detaches it. Resolving a conversation stays a
/// decision, not an accident.
///
/// Only editable blobs: a git blob is immutable, so there is nothing for a
/// cursor to ride, and the line it names is exact.
///
/// Returns the pairs it minted for comments that had none — older ones — so
/// the caller can hand them to the store.
#[expect(
    clippy::cast_possible_truncation,
    reason = "The diff row model uses u32 blob indices; a document cannot practically contain 2^32 blobs."
)]
pub(crate) fn hold_comments(
    d: &mut ReviewDoc,
    comments: &[store::Comment],
) -> Vec<(u64, store::Cursors)> {
    if comments.is_empty() {
        return Vec::new();
    }
    let mut minted = Vec::new();
    let editable: Vec<(u32, String)> = d
        .blobs
        .iter()
        .enumerate()
        .filter(|(_, b)| b.editable())
        .filter_map(|(i, _)| Some((i as u32, d.blob_paths.get(&(i as u32))?.clone())))
        .collect();
    for (index, path) in editable {
        let fresh: Vec<&store::Comment> = comments
            .iter()
            .filter(|c| c.path == path && !d.blobs[index as usize].holds(c.id))
            .collect();
        for c in fresh {
            let blob = &mut d.blobs[index as usize];
            let adopted = match &c.cursors {
                Some(cursors) => blob.adopt(c.id, cursors),
                None => false,
            };
            if !adopted && let Some(lines) = store::run_in(blob, c) {
                blob.hold(c.id, *lines.start(), *lines.end());
                if c.cursors.is_none() {
                    minted.extend(blob.cursors_of(c.id).map(|pair| (c.id, pair)));
                }
            }
        }
    }
    minted
}

/// Splice stored comments below their anchor lines in every stream, then close
/// each file card. Both steps strip their previous output first, so this is
/// idempotent and reruns after every comment add/delete; an empty `comments`
/// slice just re-caps the cards (what `build_review` wants with no store yet).
///
/// Pure: returns the cursor pairs the buffers minted along the way, for
/// `load::resplice_comments` to store.
// NOTE: the one pass a new comment list takes through the document; its callees are its steps.
// coland-ignore: @coland/high_call_degree
pub(crate) fn splice_comments(
    d: &mut ReviewDoc,
    comments: &[store::Comment],
) -> Vec<(u64, store::Cursors)> {
    let had_composer = d.composer_tab.is_some();
    strip_composer(d);
    let minted = hold_comments(d, comments);
    // The Comments tab is rebuilt here rather than patched: which threads
    // exist, where their content lives now and who answered last all change
    // with the comment list, and this is the one place that list arrives.
    let of_name = blobs_by_name(&d.blobs, &d.blob_paths);
    d.quote_spans = quote_spans(&d.blobs, &of_name, comments);
    let sides = interchange::blob_sides(d.files_rows.iter(), &d.blobs);
    let old = |c: &Comment| {
        (sides.get(&c.anchor.blob) == Some(&interchange::Side::Old)).then_some(d.old_rev.as_str())
    };
    let here = d.git_dir.as_deref().map(store::worktree_of);
    let (comments_rows, outdated_paths) =
        comments_stream(&d.blobs, &of_name, comments, old, here.as_deref());
    d.comments_rows = comments_rows;
    // Union over the streams: every stream reads the same blob table, so a
    // thread that places in one places in every stream that shows its file.
    let mut placed = std::collections::HashSet::new();
    d.for_each_stream(|rows, blobs, show_all, about, tab| {
        // The Comments tab always shows every thread — an outdated conversation
        // is what it exists to keep track of — so its cards reveal outdated
        // threads without asking the per-card toggles.
        let show = if tab == Stream::Comments {
            &outdated_paths
        } else {
            show_all
        };
        placed.extend(splice_stream(rows, blobs, comments, show, about));
    });
    d.placed_threads = placed;
    if had_composer {
        splice_composer(d);
    }
    minted
}

/// Name -> blob indices, ascending: the order the loader interned them.
/// Authored text is in here under the name it carries — it has no path, and a
/// thread on a transcript entry is tracked like any other.
#[expect(
    clippy::cast_possible_truncation,
    reason = "Blob indices are u32 throughout the diff model."
)]
fn blobs_by_name<'a>(
    blobs: &'a [Blob],
    blob_paths: &'a std::collections::HashMap<u32, String>,
) -> std::collections::HashMap<&'a str, Vec<u32>> {
    let mut out: std::collections::HashMap<&str, Vec<u32>> = std::collections::HashMap::new();
    for (blob, path) in blob_paths {
        out.entry(path.as_str()).or_default().push(*blob);
    }
    for (blob, name) in blobs
        .iter()
        .enumerate()
        .filter_map(|(blob, b)| Some((blob as u32, b.prose.as_deref()?)))
    {
        out.entry(name).or_default().push(blob);
    }
    for list in out.values_mut() {
        list.sort_unstable();
    }
    out
}

/// Where every comment's quoted words sit now, as byte ranges by `(blob, line)`.
///
/// Located by [`store::place`], the same way the thread itself is: a comment on
/// a file being typed into is held by cursors, and its words have to follow the
/// line those cursors moved to rather than the line it was written on.
///
/// Within the line, the offsets the comment stored say where to look and the
/// words themselves settle it — the occurrence nearest where they were, so a
/// line that says the same thing twice underlines the half the thread is about.
/// A quote dragged across several lines reads as one line and cannot be looked
/// for, so it keeps what it stored.
#[expect(
    clippy::cast_possible_truncation,
    reason = "Blob indices and byte offsets are u32 throughout the diff model."
)]
fn quote_spans(
    blobs: &[Blob],
    of_name: &std::collections::HashMap<&str, Vec<u32>>,
    comments: &[store::Comment],
) -> std::collections::HashMap<(u32, u32), Vec<(u32, u32)>> {
    let mut out: std::collections::HashMap<(u32, u32), Vec<(u32, u32)>> =
        std::collections::HashMap::new();
    for comment in comments {
        let Some(quote) = &comment.quote else {
            continue;
        };
        let path_blobs = of_name
            .get(comment.path.as_str())
            .map_or(&[][..], Vec::as_slice);
        let Some(run) = store::place(comment, blobs, path_blobs) else {
            continue;
        };
        // Every blob holding that content underlines it, because every one of
        // them renders the thread: `inject_comments` places by oid, not index.
        let oid = blobs[run.blob as usize].oid;
        for &blob in path_blobs.iter().filter(|&&i| blobs[i as usize].oid == oid) {
            for line in run.start..=run.end {
                let text = blobs[blob as usize].line_text(line as usize);
                let from = if line == run.start { quote.from } else { 0 } as usize;
                let to =
                    (if line == run.end { quote.to } else { u32::MAX } as usize).min(text.len());
                let stored = (from <= to).then_some((from, to));
                let range =
                    if run.start == run.end && text.get(from..to) != Some(quote.text.as_str()) {
                        text.match_indices(quote.text.as_str())
                            .min_by_key(|(at, _)| at.abs_diff(from))
                            .map(|(at, _)| (at, at + quote.text.len()))
                            .or(stored)
                    } else {
                        stored
                    };
                if let Some((from, to)) = range {
                    out.entry((blob, line))
                        .or_default()
                        .push((from as u32, to as u32));
                }
            }
        }
    }
    out
}

pub(crate) fn splice_stream(
    rows: &mut Vec<Row>,
    blobs: &[Blob],
    comments: &[store::Comment],
    show_all: &HashSet<String>,
    about: Option<&str>,
) -> HashSet<u64> {
    let placed = store::inject_comments(rows, blobs, comments, show_all, about);
    finalize_cards(rows, blobs);
    placed
}

// NOTE: comments_stream opens a card with it at both places a file's threads start.
// coland-ignore: @coland/single_caller_private_function
fn context_header(path: &str) -> Row {
    Row::FileHeader {
        path: path.to_string(),
        lang: "plain",
        adds: 0,
        dels: 0,
        from: None,
        similarity: None,
    }
}

/// Whether the thread rooted at `root` is from before `opened_at`, when the
/// window loaded its range: none of its comments is newer. A new reply makes
/// it current again, as new activity does in a chat.
pub(crate) fn is_earlier(comments: &[store::Comment], root: u64, opened_at: u64) -> bool {
    comments
        .iter()
        .filter(|c| store::thread_key(c) == root)
        .all(|c| c.created_at < opened_at)
}

/// Lines of context shown either side of a thread's code in the Comments tab —
/// the same margin a standard hunk keeps.
const COMMENT_CONTEXT: u32 = 3;

/// The Comments tab's rows: every thread as a card of its code with
/// [`COMMENT_CONTEXT`] lines either side, overlapping threads sharing one card.
/// After them come the threads whose content is not in this range: those made
/// in this checkout (`here`) shown, newest file first, then those from another
/// checkout or from before checkouts were recorded, behind their card's
/// outdated toggle. Returns the rows and the paths whose outdated threads the
/// injection must show. `old` is the old-side rev a root's anchor prints
/// with, for a root on deleted lines.
///
/// Only the context rows are built here. The comment rows are spliced in by the
/// same `inject_comments` pass every stream gets, so this tab cannot disagree
/// with the diff about where a thread sits.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "Thread context is merged with a running right edge in the diff model’s u32 line coordinates."
)]
// NOTE: one pass building the Comments tab's cards; the tests pinning it are most of its fan-in.
// coland-ignore: @coland/function_too_large, @coland/high_fan_in_complex_function
fn comments_stream<'a>(
    blobs: &[Blob],
    of_name: &std::collections::HashMap<&str, Vec<u32>>,
    comments: &[store::Comment],
    old: impl Fn(&store::Comment) -> Option<&'a str>,
    here: Option<&str>,
) -> (Vec<Row>, HashSet<String>) {
    let mut outdated_paths: HashSet<String> = HashSet::new();
    if comments.is_empty() {
        return (Vec::new(), outdated_paths);
    }
    let mut replies: std::collections::HashMap<u64, Vec<&store::Comment>> =
        std::collections::HashMap::new();
    for reply in comments {
        if let Some(parent) = reply.parent {
            replies.entry(parent).or_default().push(reply);
        }
    }

    // One line per thread: where it sits, and who has answered.
    let line = |root: &store::Comment, suffix: &str| -> String {
        let meta = match replies.get(&root.id) {
            None => format!(
                "**unanswered** — {}",
                root.author.as_deref().unwrap_or("unattributed")
            ),
            Some(answers) => format!(
                "{} repl{}, last by **{}**",
                answers.len(),
                if answers.len() == 1 { "y" } else { "ies" },
                answers
                    .iter()
                    .max_by_key(|c| c.id)
                    .and_then(|c| c.author.as_deref())
                    .unwrap_or("unattributed"),
            ),
        };
        let at = concats_review::comments::anchor_spec(root, old(root));
        format!("`{at}` · {meta}{suffix}")
    };

    // Roots in reading order: by path, then by where the anchor was left.
    let mut roots: Vec<&store::Comment> = comments.iter().filter(|c| c.parent.is_none()).collect();
    roots.sort_by_key(|c| (c.path.clone(), c.anchor.start, c.id));
    let unanswered = roots
        .iter()
        .filter(|c| !replies.contains_key(&c.id))
        .count();

    // A thread sits where its newest placeable comment does, in that comment's
    // file; one that cannot be placed is listed under its root's file.
    let path_blobs = |path: &str| of_name.get(path).map_or(&[][..], Vec::as_slice);
    let mut located: Vec<(&str, Side, &store::Comment)> = Vec::new();
    let mut unplaced: Vec<&store::Comment> = Vec::new();
    for &root in &roots {
        let thread = replies.get(&root.id).into_iter().flatten().copied();
        match store::thread_place(thread.chain([root]), blobs, path_blobs) {
            Some((at, run)) => located.push((at.path.as_str(), run, root)),
            None => unplaced.push(root),
        }
    }
    located.sort_by_key(|&(path, run, root)| (path, run.blob, run.start, root.id));
    let paths: std::collections::BTreeSet<&str> = located.iter().map(|&(path, ..)| path).collect();

    let mut cards: Vec<Row> = Vec::new();
    for path in paths {
        // Threads whose context overlaps share one card: the same line must not
        // render in two cards, or the splice would attach every thread at both
        // occurrences.
        let mut merged: Vec<(Vec<&store::Comment>, Side)> = Vec::new();
        for &(_, run, root) in located.iter().filter(|&&(at, ..)| at == path) {
            match merged.last_mut() {
                Some((members, card))
                    if card.blob == run.blob
                        && run.start.saturating_sub(COMMENT_CONTEXT)
                            <= card.end + COMMENT_CONTEXT =>
                {
                    card.end = card.end.max(run.end);
                    members.push(root);
                }
                _ => merged.push((vec![root], run)),
            }
        }
        for (members, card) in merged {
            let lines: Vec<String> = members.iter().map(|c| line(c, "")).collect();
            cards.push(Row::Prose {
                md: lines.join("  \n"),
            });
            // Authored text gets no card and no line numbers: it is not a
            // file, and framing a prompt as one would claim it is.
            let prose = blobs[card.blob as usize].prose.is_some();
            if !prose {
                cards.push(context_header(path));
            }
            let last = (blobs[card.blob as usize].line_count() as u32).saturating_sub(1);
            let context =
                card.start.saturating_sub(COMMENT_CONTEXT)..=(card.end + COMMENT_CONTEXT).min(last);
            cards.extend(context.map(|line| Row::Code {
                kind: LineKind::Context,
                old_no: None,
                new_no: (!prose).then_some(line + 1),
                blob: card.blob,
                line,
            }));
        }
    }

    // What the range can't place: this checkout's threads first, newest file
    // first, then everyone else's.
    let (mine, elsewhere): (Vec<&store::Comment>, Vec<&store::Comment>) = unplaced
        .iter()
        .partition(|c| here.is_some() && c.worktree.as_deref() == here);
    for (stale, suffix, shown) in [
        (
            &mine,
            " · **content not in this range** — thread below",
            true,
        ),
        (
            &elsewhere,
            " · **from another checkout** — its card's toggle shows it",
            false,
        ),
    ] {
        let mut by_path: Vec<(&str, Vec<&store::Comment>)> = Vec::new();
        for &c in stale {
            match by_path.iter_mut().find(|(path, _)| *path == c.path) {
                Some((_, threads)) => threads.push(c),
                None => by_path.push((c.path.as_str(), vec![c])),
            }
        }
        by_path.sort_by_key(|(_, threads)| {
            std::cmp::Reverse(threads.iter().map(|c| c.created_at).max())
        });
        for (path, threads) in by_path {
            if shown {
                outdated_paths.insert(path.to_string());
            }
            let lines: Vec<String> = threads.iter().map(|c| line(c, suffix)).collect();
            cards.push(Row::Prose {
                md: lines.join("  \n"),
            });
            cards.push(context_header(path));
        }
    }

    let mut rows = vec![
        Row::Title {
            text: "# Comments".into(),
        },
        Row::Prose {
            md: format!(
                "**{} thread(s)** · {} unanswered · {} with content not in this range · {} from other checkouts. Reply from any card; a reply with a new location moves its whole thread.",
                roots.len(),
                unanswered,
                mine.len(),
                elsewhere.len(),
            ),
        },
    ];
    rows.extend(cards);
    (rows, outdated_paths)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use gix::ObjectId;

    use super::*;
    use crate::review_doc::tests::oid;

    /// The words underlined on a line are the ones the comment was made on,
    /// wherever they have moved to since: the offsets it stored are a starting
    /// point, and the words themselves are what settles it.
    #[test]
    fn a_quote_is_underlined_where_its_words_are_now() {
        let blobs = vec![Blob::new(
            oid(1),
            "rs".into(),
            "let names = resolve(key);\n".into(),
        )];
        let quote = |from, to| {
            Some(store::Quote {
                text: "resolve(key)".into(),
                from,
                to,
            })
        };
        let comment = |quote| store::Comment {
            quote,
            ..stored(1, "a.rs", oid(1), 0, 0)
        };
        let names = HashMap::from([("a.rs", vec![0])]);

        // Where it was written, and where a line that has grown by four bytes
        // in front of it puts it now.
        let spans = quote_spans(&blobs, &names, &[comment(quote(12, 24))]);
        assert_eq!(spans.get(&(0, 0)), Some(&vec![(12, 24)]));
        let spans = quote_spans(&blobs, &names, &[comment(quote(8, 20))]);
        assert_eq!(spans.get(&(0, 0)), Some(&vec![(12, 24)]));

        // Gone from the line: the thread keeps its line, and nothing is
        // underlined that is not what it is about.
        let gone = store::Comment {
            quote: Some(store::Quote {
                text: "resolve(other)".into(),
                from: 40,
                to: 54,
            }),
            ..stored(1, "a.rs", oid(1), 0, 0)
        };
        assert_eq!(quote_spans(&blobs, &names, &[gone]).get(&(0, 0)), None);

        // A comment made on lines, not words, underlines nothing.
        assert!(quote_spans(&blobs, &names, &[comment(None)]).is_empty());
    }

    /// A line that says the same thing twice underlines the half the thread is
    /// about: the offsets it stored pick between the occurrences.
    #[test]
    fn a_quote_takes_the_occurrence_nearest_where_it_was() {
        let blobs = vec![Blob::new(
            oid(1),
            "rs".into(),
            "  total = cost(x) + cost(x);\n".into(),
        )];
        let names = HashMap::from([("a.rs", vec![0])]);
        let on = |from, to| {
            let comment = store::Comment {
                quote: Some(store::Quote {
                    text: "cost(x)".into(),
                    from,
                    to,
                }),
                ..stored(1, "a.rs", oid(1), 0, 0)
            };
            quote_spans(&blobs, &names, &[comment])
                .get(&(0, 0))
                .cloned()
        };

        // Both occurrences reachable from the offsets they were written with,
        // and from offsets two bytes off after the line was indented.
        assert_eq!(on(10, 17), Some(vec![(10, 17)]));
        assert_eq!(on(20, 27), Some(vec![(20, 27)]));
        assert_eq!(on(8, 15), Some(vec![(10, 17)]));
        assert_eq!(on(18, 25), Some(vec![(20, 27)]));
    }

    /// A comment on a file being typed into is held by cursors, and its words
    /// follow the line those cursors moved to — not the line it was written on.
    #[test]
    fn a_quote_follows_the_line_its_comment_is_held_by() {
        let mut blob = Blob::new(oid(1), "rs".into(), "let names = resolve(key);\n".into());
        blob.hold(1, 0, 0);
        blob.edit(0..0, "\n\n");
        let blobs = vec![blob];
        let names = HashMap::from([("a.rs", vec![0])]);
        let comment = store::Comment {
            quote: Some(store::Quote {
                text: "resolve(key)".into(),
                from: 12,
                to: 24,
            }),
            ..stored(1, "a.rs", oid(1), 0, 0)
        };

        // Two lines above it: the underline came down with the line.
        let spans = quote_spans(&blobs, &names, &[comment]);
        assert_eq!(spans.get(&(0, 0)), None);
        assert_eq!(spans.get(&(0, 2)), Some(&vec![(12, 24)]));
    }

    /// One thread on `a.rs`, so the file's card can be checked end to end.
    fn stored(id: u64, path: &str, blob: ObjectId, start: u32, end: u32) -> Comment {
        Comment {
            id,
            path: path.into(),
            anchor: store::Anchor { blob, start, end },
            body: "b".into(),
            author: Some("ada".into()),
            created_at: 0,
            parent: None,
            external: None,
            worktree: None,
            cursors: None,
            quote: None,
        }
    }

    #[test]
    #[expect(
        clippy::cognitive_complexity,
        clippy::format_collect,
        reason = "Generated fixture lines and row assertions keep the expected context visible together."
    )]
    fn comments_tab_frames_a_thread_in_the_context_a_hunk_gets() {
        let text: String = (0..20).map(|n| format!("line {n}\n")).collect();
        let blobs = vec![Blob::new(oid(1), "rs".into(), text)];
        let names = HashMap::from([("a.rs", vec![0])]);
        let comments = vec![stored(1, "a.rs", oid(1), 9, 10)];
        let (rows, outdated) = comments_stream(&blobs, &names, &comments, |_| None, None);
        assert!(outdated.is_empty());
        assert!(matches!(&rows[0], Row::Title { .. }));
        let Row::Prose { md } = &rows[1] else {
            panic!("row 1 is not the summary");
        };
        assert!(md.contains("**1 thread(s)** · 1 unanswered · 0 with content"));
        let Row::Prose { md } = &rows[2] else {
            panic!("row 2 is not the thread meta");
        };
        assert!(md.contains("`a.rs:10-11`"), "meta was: {md}");
        assert!(md.contains("**unanswered** — ada"));
        assert!(matches!(&rows[3], Row::FileHeader { path, .. } if path == "a.rs"));
        // COMMENT_CONTEXT lines either side of the 9..=10 anchor: 6..=13.
        let code: Vec<(u32, Option<u32>)> = rows[4..]
            .iter()
            .map(|r| match r {
                Row::Code {
                    kind: LineKind::Context,
                    old_no: None,
                    blob: 0,
                    line,
                    new_no,
                } => (*line, *new_no),
                _ => panic!("expected only context lines after the header"),
            })
            .collect();
        let expect: Vec<(u32, Option<u32>)> = (6..=13).map(|n| (n, Some(n + 1))).collect();
        assert_eq!(code, expect);
    }

    /// A thread on a transcript entry is tracked like any other — in the content
    /// it was written on, under the name that content carries. No file card
    /// around it: a prompt is not a file, and a header would claim it is.
    #[test]
    #[expect(
        clippy::format_collect,
        reason = "Generated fixture lines make the expected context explicit."
    )]
    fn the_comments_tab_frames_a_prose_thread_without_a_card() {
        let text: String = (0..6).map(|n| format!("line {n}\n")).collect();
        let name = "turn/8ce374d0a3/prompt/0";
        let blobs = vec![Blob::prose(name.into(), text)];
        let comments = vec![stored(1, name, blobs[0].oid, 1, 2)];
        let names = HashMap::from([(name, vec![0])]);
        let (rows, outdated) = comments_stream(&blobs, &names, &comments, |_| None, None);
        assert!(outdated.is_empty());
        assert!(!rows.iter().any(|r| matches!(r, Row::FileHeader { .. })));
        let Row::Prose { md } = &rows[2] else {
            panic!("row 2 is not the thread meta");
        };
        assert!(md.contains("`turn/8ce374d0a3/prompt/0:2-3`"), "meta: {md}");
        // Its own lines, numberless: the whole entry here, since three lines of
        // context either side of 1..=2 covers it.
        let lines: Vec<(u32, Option<u32>)> = rows[3..]
            .iter()
            .map(|r| match r {
                Row::Code { line, new_no, .. } => (*line, *new_no),
                _ => panic!("expected only the entry's lines after the meta"),
            })
            .collect();
        assert_eq!(lines, (0..=5).map(|n| (n, None)).collect::<Vec<_>>());
    }

    #[test]
    #[expect(
        clippy::format_collect,
        reason = "Generated fixture lines make the expected context explicit."
    )]
    fn comment_context_uses_the_newest_anchor_regardless_of_input_order() {
        let text: String = (0..20).map(|n| format!("line {n}\n")).collect();
        let blobs = vec![Blob::new(oid(1), "rs".into(), text)];
        let names = HashMap::from([("a.rs", vec![0])]);
        let root = stored(1, "a.rs", oid(1), 0, 0);
        let reply = Comment {
            parent: Some(1),
            ..stored(2, "a.rs", oid(1), 8, 8)
        };
        for comments in [[root.clone(), reply.clone()], [reply, root]] {
            let (rows, _) = comments_stream(&blobs, &names, &comments, |_| None, None);
            let lines: Vec<u32> = rows
                .iter()
                .filter_map(|row| match row {
                    Row::Code { line, .. } => Some(*line),
                    _ => None,
                })
                .collect();
            assert_eq!(lines, (5..=11).collect::<Vec<_>>());
        }
    }

    #[test]
    #[expect(
        clippy::format_collect,
        reason = "Generated fixture lines make the expected context explicit."
    )]
    fn threads_with_overlapping_context_share_a_card() {
        let text: String = (0..20).map(|n| format!("line {n}\n")).collect();
        let blobs = vec![Blob::new(oid(1), "rs".into(), text)];
        let names = HashMap::from([("a.rs", vec![0])]);
        let reply = Comment {
            id: 3,
            parent: Some(2),
            author: Some("bob".into()),
            ..stored(3, "a.rs", oid(1), 8, 8)
        };
        let comments = vec![
            stored(1, "a.rs", oid(1), 5, 5),
            stored(2, "a.rs", oid(1), 8, 8),
            reply,
        ];
        let (rows, _) = comments_stream(&blobs, &names, &comments, |_| None, None);
        let headers = rows
            .iter()
            .filter(|r| matches!(r, Row::FileHeader { .. }))
            .count();
        assert_eq!(headers, 1, "overlapping threads must not split into cards");
        let Row::Prose { md } = &rows[2] else {
            panic!("row 2 is not the thread meta");
        };
        assert!(md.contains("`a.rs:6-6` · **unanswered** — ada"));
        assert!(md.contains("`a.rs:9-9` · 1 reply, last by **bob**"));
        // The merged card runs from 5-3 to 8+3, once.
        let lines: Vec<u32> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Code { line, .. } => Some(*line),
                _ => None,
            })
            .collect();
        let expect: Vec<u32> = (2..=11).collect();
        assert_eq!(lines, expect);
    }

    /// A card prints the anchor the CLI reads back: a thread on deleted lines
    /// names the old side of the range's diff, one on new lines does not.
    #[test]
    fn a_card_on_deleted_lines_prints_an_old_side_anchor() {
        let mut d = ReviewDoc {
            blobs: vec![
                Blob::new(oid(1), "rs".into(), "gone\n".into()),
                Blob::new(oid(2), "rs".into(), "kept\n".into()),
            ],
            blob_paths: HashMap::from([(0, "a.rs".to_string()), (1, "a.rs".to_string())]),
            files_rows: vec![Row::del(0, 0), Row::add(1, 0)],
            old_rev: "1a2b3c4".into(),
            ..Default::default()
        };
        let comments = [
            stored(1, "a.rs", oid(1), 0, 0),
            stored(2, "a.rs", oid(2), 0, 0),
        ];
        splice_comments(&mut d, &comments);
        let cards: String = d
            .comments_rows
            .iter()
            .filter_map(|r| match r {
                Row::Prose { md } => Some(md.as_str()),
                _ => None,
            })
            .collect();
        assert!(cards.contains("`a.rs@1a2b3c4:1-1`"), "{cards}");
        assert!(cards.contains("`a.rs:1-1`"), "{cards}");
    }

    /// A reply that moved its thread to another file takes the thread's card
    /// with it: the context is built in the file the reply names.
    #[test]
    fn a_thread_moved_to_another_file_is_framed_in_that_file() {
        let blobs = vec![
            Blob::new(oid(1), "rs".into(), "a\nb\n".into()),
            Blob::new(oid(2), "rs".into(), "c\nd\n".into()),
        ];
        let names = HashMap::from([("old.rs", vec![0]), ("new.rs", vec![1])]);
        let reply = Comment {
            parent: Some(1),
            ..stored(2, "new.rs", oid(2), 1, 1)
        };
        let comments = vec![stored(1, "old.rs", oid(1), 0, 0), reply];
        let (rows, outdated) = comments_stream(&blobs, &names, &comments, |_| None, None);
        assert!(outdated.is_empty());
        let headers: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                Row::FileHeader { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headers, ["new.rs"]);
        assert!(
            rows.iter()
                .filter(|r| matches!(r, Row::Code { .. }))
                .all(|r| matches!(r, Row::Code { blob: 1, .. }))
        );
    }

    #[test]
    #[expect(
        clippy::cognitive_complexity,
        reason = "The row assertions read the stream top to bottom in one place."
    )]
    fn a_thread_left_behind_by_this_range_is_listed_for_the_flush() {
        // Anchors on blobs this range does not carry, and no blob of either
        // path to re-find them in: both threads are content-outdated. One was
        // made in this checkout, the other in another.
        let comments = vec![
            Comment {
                worktree: Some("other".into()),
                ..stored(1, "elsewhere.rs", oid(8), 2, 2)
            },
            Comment {
                worktree: Some("here".into()),
                ..stored(2, "gone.rs", oid(9), 4, 4)
            },
        ];
        let (rows, outdated) =
            comments_stream(&[], &HashMap::new(), &comments, |_| None, Some("here"));
        // Only this checkout's thread is shown; the other waits behind its
        // card's toggle.
        assert_eq!(outdated, HashSet::from(["gone.rs".to_string()]));
        let Row::Prose { md } = &rows[1] else {
            panic!("row 1 is not the summary");
        };
        assert!(md.contains("1 with content not in this range · 1 from other checkouts"));
        assert!(matches!(&rows[2], Row::Prose { md } if md.contains("content not in this range")));
        // A bare card and no code: the injection flushes the thread under it.
        assert!(matches!(&rows[3], Row::FileHeader { path, .. } if path == "gone.rs"));
        assert!(matches!(&rows[4], Row::Prose { md } if md.contains("from another checkout")));
        assert!(matches!(&rows[5], Row::FileHeader { path, .. } if path == "elsewhere.rs"));
        assert!(!rows.iter().any(|r| matches!(r, Row::Code { .. })));
    }

    /// A comment written before cursors existed is held from its exact lines,
    /// and the splice hands back the pair it minted so the store can keep it.
    /// Another process — a fresh document over the same bytes — adopts that
    /// pair instead of minting again.
    #[test]
    #[expect(
        clippy::format_collect,
        reason = "Generated fixture lines make the expected context explicit."
    )]
    fn the_splice_mints_cursors_for_a_comment_that_has_none() {
        let text: String = (0..20).map(|n| format!("line {n}\n")).collect();
        let editable = || {
            let mut b = Blob::new(oid(1), "rs".into(), text.clone());
            b.origin = Some("/tmp/a.rs".into());
            ReviewDoc {
                blobs: vec![b],
                blob_paths: HashMap::from([(0u32, "a.rs".to_string())]),
                ..Default::default()
            }
        };
        let mut d = editable();
        let bare = vec![stored(1, "a.rs", oid(1), 9, 10)];
        let minted = splice_comments(&mut d, &bare);
        assert_eq!(minted.len(), 1);
        assert_eq!(minted[0].0, 1);
        assert!(d.blobs[0].holds(1));
        assert!(splice_comments(&mut d, &bare).is_empty(), "held already");

        let mut elsewhere = editable();
        let carried = vec![Comment {
            cursors: Some(minted[0].1.clone()),
            ..stored(1, "a.rs", oid(1), 9, 10)
        }];
        assert!(
            splice_comments(&mut elsewhere, &carried).is_empty(),
            "adopted"
        );
        assert!(elsewhere.blobs[0].holds(1));
    }

    #[test]
    fn a_thread_is_earlier_until_something_in_it_is_newer() {
        let at = |id, parent, created_at| Comment {
            parent,
            created_at,
            ..stored(id, "a.rs", oid(1), 1, 1)
        };
        let comments = vec![at(1, None, 10), at(2, Some(1), 20), at(3, None, 50)];
        // Opened at 30: the first thread's newest comment is older.
        assert!(is_earlier(&comments, 1, 30));
        assert!(!is_earlier(&comments, 3, 30));
        // A reply after the window opened makes the thread current.
        let replied = [comments.clone(), vec![at(4, Some(1), 40)]].concat();
        assert!(!is_earlier(&replied, 1, 30));
    }
}
