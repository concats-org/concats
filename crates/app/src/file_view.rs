//! File tabs: one open file, its whole content at the range's head, as a stream
//! of its own lines you can comment on and edit.
//!
//! Unlike the four fixed streams there is one of these per open file, and it is
//! the only stream without a `FileHeader`: which file it is lives in the dock
//! tab, not in a caption over the text. The Settings tab rides the same
//! mechanism, because editable, highlighted JSON is a file view over text that
//! just doesn't come from git.

use concats_diff::{load, Blob, Row};
use concats_review::store::{self, Comment};

use crate::review_doc::{finalize_cards, strip_composer, FileView, ReviewDoc};

/// Both sides of one file for the File tab: its content at the range's base
/// (`None` when the range creates it) and at its head.
///
/// The base side turns the view from a listing into a review: the marks come
/// from the diff against it, and it is the only place a deleted line still
/// exists. An unchanged file resolves both sides to the same oid, and the empty
/// diff marks nothing.
pub(crate) fn read_file_sides(
    repo: &str,
    range: (Option<gix::ObjectId>, Option<gix::ObjectId>),
    path: &str,
) -> Result<(Option<Blob>, Blob), concats_diff::Error> {
    let repo = std::path::Path::new(repo);
    let (base, head) = range;
    let ext = path.rsplit('.').next().unwrap_or("").to_string();
    let blob = |oid, bytes: Vec<u8>| {
        Blob::new(
            oid,
            ext.clone(),
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    };

    let (head_oid, head_bytes) = load::read_at_head(repo, head, path)?;
    // With no head commit the bytes came off the working tree, so this side is
    // the file itself and can be written back; with one they came out of the
    // object database, and there is nothing to write to. The path is taken off
    // the discovered root, the way the diff loader builds it, not off the
    // `--repo` argument, which may be relative or a symlink: `origin` is the
    // key an open buffer is carried across reloads by, and two spellings of one
    // file would silently drop the buffer and every comment cursor in it.
    let origin = head
        .is_none()
        .then(|| load::discover(repo).as_deref().unwrap_or(repo).join(path));
    // Refused rather than rendered as mojibake — the same screen the lowerer
    // applies to a diff's blobs.
    if head_bytes.contains(&0) {
        return Err(concats_diff::Error::Binary {
            path: path.to_string(),
        });
    }
    let base = load::read_at_base(repo, base, path)?
        .filter(|(_, bytes)| !bytes.contains(&0))
        .map(|(oid, bytes)| blob(oid, bytes));
    let mut head = blob(head_oid, head_bytes);
    head.origin = origin;
    Ok((base, head))
}

/// Intern a blob into the doc's table, reusing an existing entry where there is
/// one.
///
/// Content-addressed, so browsing the same file twice does not grow the table.
/// More importantly, a changed file's head blob resolves to the index the diff
/// already gave it, so one comment thread renders in the file view and in the
/// diff view alike.
fn intern(d: &mut ReviewDoc, path: &str, blob: Blob) -> u32 {
    // Two rules for reuse. A blob that names a file in the working tree is that
    // file's buffer, whatever revision its oid names right now, so it is
    // matched by path: every stream then shares one entry, one document, one
    // set of comment cursors and one undo history. Matching it by oid instead
    // would fork a second buffer for the same file the moment its content
    // moved. Everything else is matched by oid, but only while clean: a blob
    // that has been typed into no longer holds the content its oid names, and
    // reusing it would put edited text under diff rows whose line numbers came
    // from the pristine file. So a dirty entry is left alone and a fresh one
    // goes in beside it; the two converge on save, when the file is re-read.
    let reusable = |b: &Blob| match (b.origin.as_deref(), blob.origin.as_deref()) {
        (Some(open), Some(incoming)) => open == incoming,
        _ => b.oid == blob.oid && !b.dirty(),
    };
    let index = match d.blobs.iter().position(reusable) {
        Some(i) => {
            // An unchanged worktree file resolves both sides to one oid, and
            // the base side is interned first. Without this the entry we keep
            // is the read-only one, and the file can't be typed into even
            // though it sits right there in the working tree.
            if d.blobs[i].origin.is_none() {
                d.blobs[i].origin = blob.origin;
            }
            i as u32
        }
        None => {
            d.blobs.push(blob);
            (d.blobs.len() - 1) as u32
        }
    };
    d.blob_paths
        .entry(index)
        .or_insert_with(|| path.to_string());
    index
}

/// Point the File tab at one file's whole content at the head, replacing
/// whatever it held.
///
/// `sides` is the file at the range's base (absent when the range creates it)
/// and at its head. Both are interned: what the range removed only exists on
/// the base blob, a `Row::Removed` marker names it there, and the reveal reads
/// it from there.
pub(crate) fn open_file(
    d: &mut ReviewDoc,
    path: &str,
    sides: (Option<Blob>, Blob),
    comments: &[Comment],
) {
    let (old, new) = sides;
    let old = old.map(|b| intern(d, path, b));
    let index = intern(d, path, new);
    // Keyed by path, so opening a file already open replaces that tab's rows
    // rather than stacking a second copy of it.
    let view = FileView {
        tab: crate::dock::file_tab_id(path).0,
        path: path.to_string(),
        rows: Vec::new(),
        base: old,
        head: index,
        heading: None,
    };
    place(d, view, comments);
    // `rows_rev`, not `generation`: this is not a landed load, and every cache
    // keyed by row index is stale — see the field's doc on `ReviewDoc`.
    d.rows_rev += 1;
}

/// What a File tab is called: the file, the revision its content is at, and
/// whether it has unsaved edits.
///
/// The revision sits here rather than over the text because the tab is the only
/// chrome an editor has. A worktree range has no commit to name, and that is
/// also when the file can be typed into.
pub(crate) fn file_tab_title(d: &ReviewDoc, path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let dirty = d
        .files_open
        .iter()
        .find(|f| f.path == path)
        .is_some_and(|f| d.blobs[f.head as usize].dirty());
    let at = match d.head_oid {
        Some(oid) => oid.to_string()[..7].to_string(),
        None => "worktree".into(),
    };
    format!("{}{name} · {at}", if dirty { "• " } else { "" })
}

/// Lower one file's whole content to rows, from blobs already in the table.
///
/// Split out of `open_file` because typing runs it again against the same two
/// indices: the head side is the buffer being edited, and re-reading the file
/// would throw the edit away. Re-lowering also keeps the add/removed marks
/// right while you type; they come out of the diff, not out of a patch applied
/// to the rows.
fn file_rows(d: &ReviewDoc, view: &FileView, comments: &[Comment]) -> Vec<Row> {
    let index = view.head;
    let code = load::whole_file_rows(&d.blobs, view.base, index);
    // No card header and no caption: a File tab is an editor, so path, revision
    // and dirty state live in its tab. The diff streams still build both; there
    // the file is one code block among others and has to say which file it is.
    let mut rows: Vec<Row> = view
        .heading
        .iter()
        .map(|md| Row::Prose { md: md.clone() })
        .collect();
    rows.extend(code);
    // The two steps resplice_comments runs per stream — here rather than
    // resplicing all five, which walks every row of every stream on a click.
    store::inject_comments(
        &mut rows,
        &d.blobs,
        comments,
        &d.show_all_comments,
        Some(&view.path),
    );
    finalize_cards(&mut rows);
    rows
}

/// Point the Settings tab at `config.json`.
///
/// It is a File tab like any other: one blob with an origin, lowered by the
/// same whole-file path, typed into by the same caret, undone by the same
/// stack. Both sides are the same blob, so every line reads as context rather
/// than as an addition against nothing; the settings are not a change.
///
/// Only what saving means differs, and that lives at the keystroke: the text is
/// applied as settings, not just written.
pub(crate) fn open_settings(d: &mut ReviewDoc) {
    let path = crate::theme::config_file();
    let text = crate::theme::settings_text();
    let mut blob = Blob::new(
        concats_sync::hash_object(text.as_bytes()),
        "json".into(),
        text,
    );
    blob.origin = Some(path.clone());
    let name = path.to_string_lossy().into_owned();
    let index = intern(d, &name, blob);
    let heading = format!(
        "`config.json` · themes: {}",
        crate::theme::theme_names().join(", ")
    );
    // No comments: this file has no place in the repo, so nothing anchors to it.
    let view = FileView {
        tab: crate::dock::settings_tab_id().0,
        path: name,
        rows: Vec::new(),
        base: Some(index),
        head: index,
        heading: Some(heading),
    };
    place(d, view, &[]);
}

/// Lower a file view and put it in the document, replacing the one its tab
/// already holds. The one place a tab's content is established, so opening a
/// file and opening the settings differ only in the view they hand over.
fn place(d: &mut ReviewDoc, mut view: FileView, comments: &[Comment]) {
    view.rows = file_rows(d, &view, comments);
    match d.files_open.iter_mut().find(|f| f.tab == view.tab) {
        Some(open) => *open = view,
        None => d.files_open.push(view),
    }
    // `rows_rev`, not `generation`: this is not a landed load, and every cache
    // keyed by row index is stale — see the field's doc on `ReviewDoc`.
    d.rows_rev += 1;
}

/// What a save has to do, worked out before anything touches the disk.
///
/// Pure so the arithmetic is testable: the service performs it, and the two
/// halves — write the bytes, carry the ticks — must describe the same file.
pub(crate) struct SavePlan {
    pub path: std::path::PathBuf,
    pub text: String,
    pub old: gix::ObjectId,
    pub new: gix::ObjectId,
    /// Where each unchanged line of the text as last read from disk sits now,
    /// from the document. An edited or deleted line is absent, so its tick
    /// stays on the old oid.
    pub lines: std::collections::HashMap<u32, u32>,
}

pub(crate) fn save_plan(d: &ReviewDoc, blob: u32) -> Option<SavePlan> {
    let b = d.blobs.get(blob as usize)?;
    let path = b.origin.clone()?;
    if !b.dirty() {
        return None;
    }
    let tip = b.doc.as_ref()?.oplog_frontiers();
    let lines = b.line_moves(&b.disk, &tip);
    Some(SavePlan {
        path,
        text: b.text.clone(),
        old: b.oid,
        new: concats_sync::hash_object(b.text.as_bytes()),
        lines,
    })
}

/// Re-lower every open file whose buffer has been typed into.
///
/// This keeps the add/removed marks live: the rows are rebuilt from a fresh
/// diff of base against the buffer, so a context line you type on becomes an
/// addition and the line it replaced shows up as a removal marker. Nothing
/// patches rows in place.
pub(crate) fn relower_edited(d: &mut ReviewDoc, comments: &[Comment]) {
    let edited: Vec<usize> = d
        .files_open
        .iter()
        .enumerate()
        .filter(|(_, f)| d.blobs[f.head as usize].dirty())
        .map(|(i, _)| i)
        .collect();
    if edited.is_empty() {
        return;
    }
    // A re-lower replaces a stream's whole row vector, and `compose_anchor` is
    // a row index into the old one. You are typing code, not a comment.
    strip_composer(d);
    for i in edited {
        let rows = file_rows(d, &d.files_open[i], comments);
        d.files_open[i].rows = rows;
    }
    d.rows_rev += 1;
}

#[cfg(test)]
mod tests {
    use concats_diff::{Blob, LineKind};
    use gix::ObjectId;

    use super::*;
    use crate::review_doc::{reveal_removed, type_at, Caret, Tab};

    fn oid(n: u8) -> ObjectId {
        ObjectId::from_hex(format!("{n:040x}").as_bytes()).expect("valid hex")
    }

    /// The rows of the one file the test opened.
    fn opened(d: &ReviewDoc) -> &[Row] {
        &d.files_open.first().expect("a file is open").rows
    }

    /// Every code row of a stream as (kind, old_no, new_no).
    fn code_of(rows: &[Row]) -> Vec<(LineKind, Option<u32>, Option<u32>)> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Code {
                    kind,
                    old_no,
                    new_no,
                    ..
                } => Some((*kind, *old_no, *new_no)),
                _ => None,
            })
            .collect()
    }

    #[test]
    /// A File tab is an editor, not a code block among others: nothing but the
    /// file's own lines. Path, revision and dirty state live in the tab, so a
    /// header and a caption would only repeat it, and a card boundary would put
    /// a frame around the thing you type into.
    fn an_untouched_file_opens_as_a_plain_stream_of_its_own_lines() {
        let same = || Blob::new(oid(1), "rs".into(), "a\nb\nc\n".into());
        let mut d = ReviewDoc {
            head: "HEAD".into(),
            head_oid: Some(oid(9)),
            ..Default::default()
        };
        let at_rest = d.rows_rev;

        open_file(&mut d, "src/a.rs", (Some(same()), same()), &[]);

        assert_eq!(d.files_open[0].path, "src/a.rs");
        // Both sides are the same blob, so the diff is empty and nothing is
        // marked — the file reads plain, which is the point.
        assert_eq!(
            code_of(opened(&d)),
            [
                (LineKind::Context, Some(1), Some(1)),
                (LineKind::Context, Some(2), Some(2)),
                (LineKind::Context, Some(3), Some(3)),
            ]
        );
        assert!(
            opened(&d).iter().all(|r| matches!(r, Row::Code { .. })),
            "no header, no caption and no card cap — just the file"
        );
        // What the header used to say, said by the tab instead.
        let title = file_tab_title(&d, "src/a.rs");
        assert!(title.starts_with("a.rs"), "{title}");
        assert!(title.contains(&oid(9).to_string()[..7]), "{title}");
        assert!(
            d.rows_rev > at_rest,
            "every cache keyed by row index is stale"
        );
    }

    /// The File tab shows a file whether or not the range touched it — that is
    /// what lets a comment land on code nobody changed. An unchanged file
    /// resolves both sides to one blob, which is the case most likely to fall
    /// through the lowering.
    #[test]
    fn an_unchanged_file_still_renders_every_line_and_stays_editable() {
        let mut d = ReviewDoc::default();
        let text = "one\ntwo\nthree\n";
        let mut head = Blob::new(oid(1), "txt".into(), text.into());
        head.origin = Some("/tmp/a.txt".into());
        open_file(
            &mut d,
            "a.txt",
            (Some(Blob::new(oid(1), "txt".into(), text.into())), head),
            &[],
        );

        assert_eq!(
            code_of(opened(&d)),
            [
                (LineKind::Context, Some(1), Some(1)),
                (LineKind::Context, Some(2), Some(2)),
                (LineKind::Context, Some(3), Some(3)),
            ],
            "an unchanged file is all context, not nothing"
        );
        let head_ix = d.files_open[0].head;
        assert!(
            d.blobs[head_ix as usize].editable(),
            "both sides interned to one blob; the writable side has to win"
        );
    }

    /// A save moves a file to a new content hash, and every anchor in the store
    /// names content, so the plan has to say where each line went, not just
    /// that the file changed.
    #[test]
    fn a_save_plan_maps_every_surviving_line_to_where_it_sits_now() {
        let mut d = ReviewDoc::default();
        let mut head = Blob::new(oid(2), "rs".into(), "a\nb\nc\n".into());
        head.origin = Some("/tmp/a.rs".into());
        d.blobs.push(head);
        // Nothing typed yet: nothing to save.
        assert!(save_plan(&d, 0).is_none());

        // Split line 1 in two, which pushes line 2 down.
        d.caret = Some(Caret {
            blob: 0,
            line: 1,
            byte: 0,
        });
        assert!(type_at(&mut d, "X\n", 0));

        let plan = save_plan(&d, 0).expect("a dirty buffer has a plan");
        assert_eq!(plan.text, "a\nX\nb\nc\n");
        assert_eq!(plan.path, std::path::PathBuf::from("/tmp/a.rs"));
        assert_ne!(plan.new, plan.old, "saving gives the file a new hash");
        assert_eq!(plan.lines.get(&0), Some(&0), "the line above is untouched");
        assert_eq!(plan.lines.get(&1), Some(&2), "'b' was pushed down one");
        assert_eq!(plan.lines.get(&2), Some(&3), "and so was 'c'");
    }

    #[test]
    fn a_line_typed_away_leaves_its_anchor_behind() {
        let mut d = ReviewDoc::default();
        let mut head = Blob::new(oid(2), "rs".into(), "a\nb\nc\n".into());
        head.origin = Some("/tmp/a.rs".into());
        d.blobs.push(head);
        d.caret = Some(Caret {
            blob: 0,
            line: 1,
            byte: 0,
        });
        // Delete the whole of line 1 including its newline.
        d.blobs[0].edit(2..4, "");

        let plan = save_plan(&d, 0).expect("a dirty buffer has a plan");
        assert_eq!(plan.text, "a\nc\n");
        assert_eq!(plan.lines.get(&0), Some(&0));
        assert_eq!(
            plan.lines.get(&1),
            None,
            "the line is gone, so a thread on it has nowhere to move to and \
             stays on the old oid — which is how it renders as outdated"
        );
        assert_eq!(plan.lines.get(&2), Some(&1));
    }

    /// After a save the buffer is the file, so it is no longer kept apart from
    /// the diff's copy of the same content.
    #[test]
    fn a_saved_buffer_stops_being_dirty_and_takes_the_new_oid() {
        let mut b = Blob::new(oid(1), "rs".into(), "a\n".into());
        b.origin = Some("/tmp/a.rs".into());
        b.edit(1..1, "X");
        assert!(b.dirty());
        b.saved(oid(2));
        assert!(!b.dirty());
        assert_eq!(b.oid, oid(2));
        assert_eq!(b.anchor_line(0), Some(0), "anchors resolve against it now");
    }

    /// The mutation-driven twin of the test below: nothing re-reads the file,
    /// the buffer is typed into and the rows are lowered again from it. That is
    /// how the add/removed marks describe what you have right now.
    #[test]
    fn typing_on_a_context_line_re_lowers_it_as_an_addition_over_a_marker() {
        let mut d = ReviewDoc::default();
        let mut head = Blob::new(oid(2), "rs".into(), "a\nb\nc\n".into());
        head.origin = Some("/tmp/a.rs".into());
        open_file(
            &mut d,
            "src/a.rs",
            (
                Some(Blob::new(oid(1), "rs".into(), "a\nb\nc\n".into())),
                head,
            ),
            &[],
        );
        // Identical sides: every line is context and nothing is marked.
        assert_eq!(
            code_of(opened(&d)),
            [
                (LineKind::Context, Some(1), Some(1)),
                (LineKind::Context, Some(2), Some(2)),
                (LineKind::Context, Some(3), Some(3)),
            ]
        );
        assert!(!opened(&d).iter().any(|r| matches!(r, Row::Removed { .. })));

        // Type one character on the middle line.
        let head_ix = d.files_open[0].head;
        d.caret = Some(Caret {
            blob: head_ix,
            line: 1,
            byte: 1,
        });
        assert!(type_at(&mut d, "X", 0));
        let rows_rev = d.rows_rev;
        relower_edited(&mut d, &[]);

        assert_eq!(
            code_of(opened(&d)),
            [
                (LineKind::Context, Some(1), Some(1)),
                (LineKind::Add, None, Some(2)),
                (LineKind::Context, Some(3), Some(3)),
            ],
            "the typed line reads as an addition"
        );
        assert!(
            opened(&d).iter().any(|r| matches!(
                r,
                Row::Removed {
                    start: 1,
                    end: 1,
                    ..
                }
            )),
            "what it replaced is marked on the base side"
        );
        assert!(d.rows_rev > rows_rev, "row-indexed caches are invalidated");
        // The caret is in blob coordinates, so a rebuilt stream does not move it.
        assert_eq!(
            d.caret,
            Some(Caret {
                blob: head_ix,
                line: 1,
                byte: 2
            })
        );
    }

    #[test]
    fn an_edited_line_reads_as_an_addition_over_a_marker_for_what_went() {
        let mut d = ReviewDoc::default();

        open_file(
            &mut d,
            "src/a.rs",
            (
                Some(Blob::new(oid(1), "rs".into(), "a\nOLD\nc\n".into())),
                Blob::new(oid(2), "rs".into(), "a\nNEW\nc\n".into()),
            ),
            &[],
        );

        // The head file, entire and in order — the replaced line marked added,
        // its neighbours still numbered on both sides.
        assert_eq!(
            code_of(opened(&d)),
            [
                (LineKind::Context, Some(1), Some(1)),
                (LineKind::Add, None, Some(2)),
                (LineKind::Context, Some(3), Some(3)),
            ]
        );
        // What went is a marker on the old blob, not a row of content the head
        // never had. It sits where the removal was.
        let at = opened(&d)
            .iter()
            .position(|r| matches!(r, Row::Removed { .. }))
            .expect("the removal is marked");
        assert!(matches!(
            opened(&d)[at],
            Row::Removed {
                blob: 0,
                start: 1,
                end: 1
            }
        ));
        assert!(matches!(
            opened(&d)[at + 1],
            Row::Code {
                new_no: Some(2),
                ..
            }
        ));
        // The hunk anchor comes too, so a tick in the file view is a tick in
        // the diff.
        assert!(opened(&d).iter().any(|r| matches!(r, Row::HunkBar { .. })));
        // …and no +N/−N tally over the text: a File tab has no header to put it
        // in, because it is an editor. The diff streams still carry one.
        assert!(
            !opened(&d)
                .iter()
                .any(|r| matches!(r, Row::FileHeader { .. })),
            "the header belongs to the diff view, not to the editor"
        );
    }

    #[test]
    fn a_file_the_range_creates_is_all_addition() {
        let mut d = ReviewDoc::default();

        open_file(
            &mut d,
            "src/new.rs",
            (None, Blob::new(oid(1), "rs".into(), "a\nb\n".into())),
            &[],
        );

        assert_eq!(
            code_of(opened(&d)),
            [
                (LineKind::Add, None, Some(1)),
                (LineKind::Add, None, Some(2))
            ]
        );
        // Nothing was removed, so nothing is marked as removed.
        assert!(!opened(&d).iter().any(|r| matches!(r, Row::Removed { .. })));
    }

    #[test]
    fn revealing_a_removal_puts_its_lines_back_where_they_were() {
        let mut d = ReviewDoc::default();
        open_file(
            &mut d,
            "src/a.rs",
            (
                Some(Blob::new(oid(1), "rs".into(), "a\nGONE\nGONE2\nc\n".into())),
                Blob::new(oid(2), "rs".into(), "a\nc\n".into()),
            ),
            &[],
        );
        let at = opened(&d)
            .iter()
            .position(|r| matches!(r, Row::Removed { .. }))
            .expect("the removal is marked");
        let marked = d.rows_rev;

        let tab = Tab::File(d.files_open[0].tab);
        reveal_removed(&mut d, tab, at);

        // The marker is gone and its lines stand in its place, on the old blob
        // and numbered on the old side — the only side they ever had.
        assert!(!opened(&d).iter().any(|r| matches!(r, Row::Removed { .. })));
        assert_eq!(
            code_of(opened(&d)),
            [
                (LineKind::Context, Some(1), Some(1)),
                (LineKind::Del, Some(2), None),
                (LineKind::Del, Some(3), None),
                (LineKind::Context, Some(4), Some(2)),
            ]
        );
        assert!(
            d.rows_rev > marked,
            "the stream changed shape under every row-indexed cache"
        );
    }

    #[test]
    fn opening_a_file_reuses_the_blob_the_diff_already_interned() {
        let mut d = ReviewDoc {
            blobs: vec![
                Blob::new(oid(1), "rs".into(), "other\n".into()),
                Blob::new(oid(2), "rs".into(), "a\nb\n".into()),
            ],
            ..Default::default()
        };

        open_file(
            &mut d,
            "src/a.rs",
            (None, Blob::new(oid(2), "rs".into(), "a\nb\n".into())),
            &[],
        );

        // Reused by oid, not appended. Sharing the index is what lets one
        // comment render in the file view and in the diff.
        assert_eq!(d.blobs.len(), 2);
        assert!(opened(&d)
            .iter()
            .all(|r| !matches!(r, Row::Code { blob, .. } if *blob != 1)));
        assert_eq!(d.blob_paths.get(&1).map(String::as_str), Some("src/a.rs"));
    }

    #[test]
    fn a_comment_on_the_head_blob_renders_in_the_file_view() {
        let mut d = ReviewDoc::default();
        let comment = Comment {
            id: 1,
            path: "src/a.rs".into(),
            anchor: store::Anchor {
                blob: oid(1),
                start: 1,
                end: 1,
            },
            body: "why this?".into(),
            author: None,
            created_at: 0,
            parent: None,
            external: None,
            cursors: None,
        };

        open_file(
            &mut d,
            "src/a.rs",
            (None, Blob::new(oid(1), "rs".into(), "a\nb\nc\n".into())),
            std::slice::from_ref(&comment),
        );

        // Anchored to content, so a whole-file view needs no diff to carry a
        // conversation — the comment lands under its line.
        let at = opened(&d)
            .iter()
            .position(|r| matches!(r, Row::Comment { id: 1, .. }))
            .expect("the thread renders");
        assert!(matches!(opened(&d)[at - 1], Row::Code { line: 1, .. }));
    }
}
