//! Editing a buffer through the document: what a keystroke does to the
//! `ReviewDoc` once the list has routed it to the caret. These operations
//! mutate document values without reading widget state or performing I/O.
//!
//! `file_view` owns what happens to a file beyond its text — the save plan,
//! re-lowering after an edit — and is called by the widget after an edit.

use concats_diff::Row;

use crate::{
    find::Find,
    makepad_widgets::{KeyCode, KeyEvent},
    review_doc::{Caret, ReviewDoc, Stream, caret_row, header_title},
};

/// One press of Tab. Spaces, not a tab character: the row renderer lays text
/// out by the font and has no tab stops to align one to.
const INDENT: &str = "    ";

/// Typed text — from the keyboard, an IME, or a paste. Returns whether the
/// caret took it.
///
/// A paste carries newlines; a typed Return arrives as a key and is handled by
/// [`edit_key`], so anything landing here is text either way.
pub(crate) fn type_text(d: &mut ReviewDoc, input: &str) -> bool {
    match closer_for(input) {
        Some(close) => {
            // Type the pair and sit between it. Only for a bare delimiter: a
            // paste that happens to start with one is text, not a gesture.
            if !type_at(d, &format!("{input}{close}"), 0) {
                return false;
            }
            if let Some(caret) = d.caret.as_mut() {
                caret.byte -= close.len();
            }
            true
        }
        // Typing the closing half of a pair the editor just wrote steps over
        // it instead of doubling it.
        None if steps_over(d, input) => {
            if let Some(caret) = d.caret.as_mut() {
                caret.byte += input.len();
            }
            true
        }
        None => type_at(d, input, 0),
    }
}

/// The keys that change text: Return, Tab, Backspace, Delete, undo and redo.
/// `None` when this is not one of them, so the caller can go on to try it as
/// a motion.
pub(crate) fn edit_key(d: &mut ReviewDoc, ke: &KeyEvent) -> Option<bool> {
    let took = match ke.key_code {
        KeyCode::ReturnKey => {
            // Auto-indent: a new line starts where the one it came off starts.
            // Typing a block would be a pain otherwise.
            let indent = d.caret.map_or(String::new(), |c| {
                let line = d.blobs[c.blob as usize].line_text(c.line as usize);
                line[..c.byte]
                    .chars()
                    .take_while(|ch| *ch == ' ' || *ch == '\t')
                    .collect()
            });
            type_at(d, &format!("\n{indent}"), 0)
        }
        KeyCode::Tab => type_at(d, INDENT, 0),
        KeyCode::Backspace => type_at(d, "", 1),
        KeyCode::Delete => delete_forward(d),
        KeyCode::KeyZ if ke.modifiers.logo || ke.modifiers.control => {
            undo_at(d, ke.modifiers.shift)
        }
        _ => return None,
    };
    Some(took)
}

/// Arrow keys, Home and End. Returns whether the caret took the key — which
/// is what stops the list from scrolling on the same press.
pub(crate) fn move_caret(d: &mut ReviewDoc, tab: Stream, ke: &KeyEvent) -> bool {
    let Some(caret) = d.caret else {
        return false;
    };
    // Shift extends: the position the caret is leaving becomes the fixed end,
    // unless there is one already. Decided before the motion, and once here
    // rather than in each branch, so every arrow, Home and End extend by the
    // same rule.
    if ke.modifiers.shift {
        d.selection_anchor = d.selection_anchor.or(Some(caret));
    } else {
        d.selection_anchor = None;
    }
    let text = d.blobs[caret.blob as usize].line_text(caret.line as usize);
    let byte = caret.byte;
    let moved = match ke.key_code {
        KeyCode::ArrowUp => step_caret(d, tab, caret, Step::Up),
        KeyCode::ArrowDown => step_caret(d, tab, caret, Step::Down),
        KeyCode::Home => set_byte(d, caret, 0),
        KeyCode::End => set_byte(d, caret, text.len()),
        KeyCode::ArrowLeft => match (0..byte).rev().find(|i| text.is_char_boundary(*i)) {
            Some(at) => set_byte(d, caret, at),
            // Off the front of the line: carry on to the end of the one above,
            // the way every editor does.
            None => step_caret(d, tab, caret, Step::Up),
        },
        KeyCode::ArrowRight => match (byte + 1..=text.len()).find(|i| text.is_char_boundary(*i)) {
            Some(at) => set_byte(d, caret, at),
            None => step_caret(d, tab, caret, Step::Down),
        },
        _ => return false,
    };
    // A motion ends the typing run, so undo stops at where you were rather
    // than swallowing everything back to the last newline.
    if moved && let Some(c) = d.caret {
        d.blobs[c.blob as usize].break_group();
    }
    moved
}

// NOTE: Home, End and both arrows land the caret on a byte through it.
// coland-ignore: @coland/single_caller_private_function
fn set_byte(d: &mut ReviewDoc, caret: Caret, byte: usize) -> bool {
    d.caret = Some(Caret { byte, ..caret });
    true
}

/// Where a pointer gesture landed, in blob coordinates: a row index names a
/// position in one stream's current shape, and every resplice renumbers it.
pub(crate) fn caret_at(d: &ReviewDoc, tab: Stream, row: usize, byte: usize) -> Option<Caret> {
    let Some(Row::Code { blob, line, .. }) = d.stream(tab).get(row) else {
        return None;
    };
    // `DiffLine` reports one space for an empty line so blank lines survive a
    // copied range; a position must not follow it past the end of text that
    // is not there.
    let text = d.blobs[*blob as usize].line_text(*line as usize);
    Some(Caret {
        blob: *blob,
        line: *line,
        byte: text.floor_char_boundary(byte),
    })
}

/// Forward delete: the same edit as backspace, one character to the right.
fn delete_forward(d: &mut ReviewDoc) -> bool {
    // Forward delete over a selection takes the selection, like backspace does.
    if replace_selection(d, "") {
        return true;
    }
    let Some(caret) = d.caret else {
        return false;
    };
    let Some(start) = offset_of(d, caret) else {
        return false;
    };
    let blob = &mut d.blobs[caret.blob as usize];
    if start >= blob.text.len() || !blob.editable() {
        return false;
    }
    // Off the end of the line, the character to delete is the line break,
    // which joins the line below onto this one.
    let end = blob.text.ceil_char_boundary(start + 1);
    blob.edit(start..end, "");
    true
}

fn undo_at(d: &mut ReviewDoc, redo: bool) -> bool {
    let Some(caret) = d.caret else {
        return false;
    };
    let blob = &mut d.blobs[caret.blob as usize];
    let Some(at) = (if redo { blob.redo() } else { blob.undo() }) else {
        return false;
    };
    d.caret = Some(caret_at_offset(d, caret.blob, at));
    true
}

/// The delimiter that closes `input`, when it is a lone opening one.
// NOTE: the auto-pair table; steps_over reads its closing halves.
// coland-ignore: @coland/single_caller_private_function
fn closer_for(input: &str) -> Option<&'static str> {
    match input {
        "(" => Some(")"),
        "[" => Some("]"),
        "{" => Some("}"),
        _ => None,
    }
}

/// Whether the caret already sits in front of exactly this text, in which case
/// typing it should move past rather than write a second copy.
// NOTE: the step-over rule, the other half of closer_for's pairs.
// coland-ignore: @coland/single_caller_private_function
fn steps_over(d: &ReviewDoc, input: &str) -> bool {
    if !matches!(input, ")" | "]" | "}") {
        return false;
    }
    d.caret.is_some_and(|caret| {
        d.blobs[caret.blob as usize]
            .line_text(caret.line as usize)
            .get(caret.byte..)
            .is_some_and(|rest| rest.starts_with(input))
    })
}

/// Step the caret onto the code row above or below, keeping its column where
/// the new line is long enough. Walks the row stream rather than line numbers:
/// a diff interleaves two blobs' lines and puts prose between them, so "the
/// line above" is a property of the stream, not of the file.
fn step_caret(d: &mut ReviewDoc, tab: Stream, caret: Caret, step: Step) -> bool {
    let rows = d.stream(tab);
    let landed = caret_row(rows, caret)
        .and_then(|row| step_row(rows, row, step))
        .and_then(|next| rows.get(next));
    let Some(Row::Code { blob, line, .. }) = landed else {
        return false;
    };
    let (blob, line) = (*blob, *line);
    let text = d.blobs[blob as usize].line_text(line as usize);
    d.caret = Some(Caret {
        blob,
        line,
        byte: text.floor_char_boundary(caret.byte),
    });
    true
}

/// Matches in stream order, as a row and the byte range on it.
pub(crate) fn search_hits(
    d: &ReviewDoc,
    stream: Stream,
    find: &Find,
) -> Vec<(usize, (usize, usize))> {
    use std::borrow::Cow;

    d.stream(stream)
        .iter()
        .enumerate()
        .flat_map(|(row, value)| {
            let text: Cow<'_, str> = match value {
                Row::Title { text } | Row::Warning { text } => text.as_str().into(),
                Row::Prose { md } => md.as_str().into(),
                Row::Comment { body, meta, .. } => format!("{meta}\n{body}").into(),
                Row::FileHeader { path, from, .. } => header_title(path, from.as_deref()),
                Row::Code { blob, line, .. } => {
                    d.blobs[*blob as usize].line_text(*line as usize).into()
                }
                Row::Collapsed { .. }
                | Row::Removed { .. }
                | Row::HunkBar { .. }
                | Row::Spacer
                | Row::CardEnd
                | Row::Composer => "".into(),
            };
            find.hits(&text)
                .into_iter()
                .map(|hit| (row, hit))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Replace the match under the caret only when that code belongs to this tab.
pub(crate) fn replace_one(d: &mut ReviewDoc, tab: Stream, find: &Find, insert: &str) -> bool {
    let Some(caret) = d.caret else {
        return false;
    };
    if caret_row(d.stream(tab), caret).is_none() {
        return false;
    }
    let Some(at) = offset_of(d, caret) else {
        return false;
    };
    let blob = &mut d.blobs[caret.blob as usize];
    if !blob.editable() {
        return false;
    }
    let Some((start, end)) = find
        .hits(blob.line_text(caret.line as usize))
        .into_iter()
        .find(|&(start, end)| start <= caret.byte && caret.byte < end)
    else {
        return false;
    };
    let line_start = at - caret.byte;
    blob.break_group();
    blob.edit(line_start + start..line_start + end, insert);
    caret_to(d, caret.blob, line_start + start + insert.len());
    true
}

/// Replace editable code matches in this tab. Repeated views of one line
/// name one edit; reverse byte order keeps the remaining positions valid.
pub(crate) fn replace_all(d: &mut ReviewDoc, tab: Stream, find: &Find, insert: &str) -> bool {
    let hits: std::collections::BTreeSet<_> = search_hits(d, tab, find)
        .into_iter()
        .filter_map(|(row, (start, end))| {
            let &Row::Code { blob, line, .. } = d.stream(tab).get(row)? else {
                return None;
            };
            let at = offset_of(
                d,
                Caret {
                    blob,
                    line,
                    byte: start,
                },
            )?;
            d.blobs[blob as usize]
                .editable()
                .then_some((blob, at, at + end - start))
        })
        .collect();
    let Some(&(first_blob, first_byte, _)) = hits.first() else {
        return false;
    };
    for (blob, start, end) in hits.into_iter().rev() {
        let buffer = &mut d.blobs[blob as usize];
        buffer.break_group();
        buffer.edit(start..end, insert);
    }
    caret_to(d, first_blob, first_byte + insert.len());
    true
}

/// Record a settings save result after the caller has applied it.
pub(crate) fn record_settings(d: &mut ReviewDoc, text: &str, error: Option<&str>) {
    if let Some(rows) = d.stream_mut(Stream::File(crate::dock::settings_tab_spec().id.0)) {
        rows.retain(|r| !matches!(r, Row::Warning { .. }));
        if let Some(message) = error {
            rows.insert(
                0,
                Row::Warning {
                    text: message.to_string(),
                },
            );
        }
    }
    if error.is_none()
        && let Some(caret) = d.caret
    {
        d.blobs[caret.blob as usize].saved(concats_sync::hash_object(text.as_bytes()));
    }
    d.rows_rev += 1;
}

/// A caret as an absolute byte offset into its blob.
pub(crate) fn offset_of(d: &ReviewDoc, at: Caret) -> Option<usize> {
    let blob = d.blobs.get(at.blob as usize)?;
    Some(*blob.line_starts.get(at.line as usize)? as usize + at.byte)
}

/// The caret at an absolute byte offset into `blob`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "The diff row model uses u32 line indices; a buffer cannot practically contain 2^32 lines."
)]
pub(crate) fn caret_at_offset(d: &ReviewDoc, blob: u32, at: usize) -> Caret {
    let b = &d.blobs[blob as usize];
    let line = b.line_of(at);
    Caret {
        blob,
        line: line as u32,
        byte: at.saturating_sub(b.line_starts[line] as usize),
    }
}

/// The selected byte range, with `from < to`.
///
/// `None` when there is no selection, when it is empty, or when its two ends
/// sit on different blobs. A range spanning the old and new sides of a diff is
/// fine to read, but it cannot be replaced: the text between its ends exists in
/// neither blob.
pub fn selection(d: &ReviewDoc) -> Option<Selection> {
    let (head, anchor) = (d.caret?, d.selection_anchor?);
    if head.blob != anchor.blob {
        return None;
    }
    let (at, from) = (offset_of(d, head)?, offset_of(d, anchor)?);
    (at != from).then(|| Selection {
        blob: head.blob,
        from: at.min(from),
        to: at.max(from),
    })
}

/// Selected bytes in one blob, as a nonempty half-open range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    pub blob: u32,
    pub from: usize,
    pub to: usize,
}

/// Put the caret at an absolute byte offset in `blob`, with nothing selected.
pub(crate) fn caret_to(d: &mut ReviewDoc, blob: u32, at: usize) {
    d.caret = Some(caret_at_offset(d, blob, at));
    d.selection_anchor = None;
}

/// Replace the selection with `insert`, leaving the caret after it.
///
/// The primitive: deleting a selection is replacing it with nothing, typing over
/// one is replacing it with a character, and pasting is replacing it with the
/// clipboard. `false` when there is nothing selected, so the caller falls
/// through to its own single-position behaviour.
pub fn replace_selection(d: &mut ReviewDoc, insert: &str) -> bool {
    let Some(Selection { blob, from, to }) = selection(d) else {
        return false;
    };
    if !d.blobs[blob as usize].editable() {
        return false;
    }
    d.blobs[blob as usize].edit(from..to, insert);
    caret_to(d, blob, from + insert.len());
    true
}

/// Apply one edit at the caret and leave the caret after what went in.
///
/// The one gate on editing: the blob has to name a file to write back to. A git
/// object does not (its bytes exist only in the object database), so a caret on
/// a deleted-side row, or anywhere in a commit range, silently takes no text.
/// `back` deletes preceding bytes, rounded back to a whole character.
// NOTE: the one edit at the caret; the editor's keys and the tests pinning it are its fan-in.
// coland-ignore: @coland/high_fan_in_complex_function, @coland/high_call_degree
pub fn type_at(d: &mut ReviewDoc, insert: &str, back: usize) -> bool {
    // A selection is what the edit lands on: typing replaces it, and backspace
    // takes the whole of it rather than one byte off its end. `back` is the
    // single-position behaviour and does not apply when a range is selected.
    if replace_selection(d, insert) {
        return true;
    }
    let Some(caret) = d.caret else {
        return false;
    };
    let Some(start) = offset_of(d, caret) else {
        return false;
    };
    let blob = &mut d.blobs[caret.blob as usize];
    if !blob.editable() {
        return false;
    }
    // Backspace at column 0 joins this line onto the one above, so the range
    // reaches back past a newline rather than stopping at the line's edge.
    let from = blob.text.floor_char_boundary(start.saturating_sub(back));
    if from == start && insert.is_empty() {
        return false;
    }
    blob.edit(from..start, insert);
    caret_to(d, caret.blob, from + insert.len());
    true
}

/// Which way a caret motion goes. The row stream is the only thing that knows
/// what line is "above" another — lines of two blobs interleave in a diff, and
/// prose sits between them — so motion walks rows, not line numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Up,
    Down,
}

/// The code row one step up or down from `row`, skipping the chrome between
/// them (hunk bars, comments, card caps). Stops at the first row that is not
/// code — a caret does not jump from the end of one file into the next.
pub fn step_row(rows: &[Row], row: usize, step: Step) -> Option<usize> {
    let mut idx = row;
    loop {
        idx = match step {
            Step::Up => idx.checked_sub(1)?,
            Step::Down => idx + 1,
        };
        match rows.get(idx)? {
            Row::Code { .. } => return Some(idx),
            // Chrome a caret passes straight through: none of it is text.
            Row::HunkBar { .. } | Row::Comment { .. } | Row::Spacer => {}
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use concats_diff::{Blob, LineKind};

    use super::*;
    use crate::{
        find::Options,
        review_doc::tests::{code, oid},
    };

    #[expect(
        clippy::cast_possible_truncation,
        reason = "This test fixture contains only a few lines."
    )]
    fn document(text: &str) -> ReviewDoc {
        let mut blob = Blob::new(
            concats_sync::hash_object(text.as_bytes()),
            "txt".into(),
            text.into(),
        );
        blob.origin = Some("/tmp/editor-test.txt".into());
        let rows = (0..blob.line_count() as u32)
            .map(|line| Row::Code {
                kind: LineKind::Context,
                old_no: None,
                new_no: Some(line + 1),
                blob: 0,
                line,
            })
            .collect();
        ReviewDoc {
            blobs: vec![blob],
            files_rows: rows,
            tab: Stream::Files,
            caret: Some(Caret {
                blob: 0,
                line: 0,
                byte: 0,
            }),
            ..Default::default()
        }
    }

    /// Forward delete takes the character after the caret, a whole one, and at
    /// the end of a line the line break, which joins the next line on.
    #[test]
    fn delete_forward_takes_the_next_character_or_line_break() {
        let delete = |text: &str, line, byte| {
            let mut d = document(text);
            d.caret = Some(Caret {
                blob: 0,
                line,
                byte,
            });
            let deleted = delete_forward(&mut d);
            (deleted, d.blobs[0].text.clone())
        };
        assert_eq!(delete("ab\ncd\n", 0, 0), (true, "b\ncd\n".into()));
        assert_eq!(delete("é!\n", 0, 0), (true, "!\n".into()));
        assert_eq!(delete("ab\ncd\n", 0, 2), (true, "abcd\n".into()));
        assert_eq!(delete("ab\ncd\n", 1, 2), (true, "ab\ncd".into()));
        assert_eq!(delete("ab\ncd", 1, 2), (false, "ab\ncd".into()));
        assert_eq!(
            delete("ab\r\ncd\r\nef", 1, 2),
            (true, "ab\r\ncd\nef".into())
        );
        // The first line is no different.
        assert_eq!(delete("ab\r\ncd", 0, 2), (true, "ab\ncd".into()));
        assert_eq!(delete("ab\n", 0, 2), (true, "ab".into()));
    }

    #[test]
    fn backspace_removes_a_whole_character_and_undo_restores_it() {
        let mut d = document("aé🙂\n");
        d.caret.as_mut().unwrap().byte = 7;
        let backspace = KeyEvent {
            key_code: KeyCode::Backspace,
            ..Default::default()
        };
        assert_eq!(edit_key(&mut d, &backspace), Some(true));
        assert_eq!(d.blobs[0].text, "aé\n");
        assert_eq!(d.caret.unwrap().byte, 3);
        assert_eq!(edit_key(&mut d, &backspace), Some(true));
        assert_eq!(d.blobs[0].text, "a\n");
        assert!(undo_at(&mut d, false));
        assert_eq!(d.blobs[0].text, "aé\n");
        assert!(undo_at(&mut d, false));
        assert_eq!(d.blobs[0].text, "aé🙂\n");
    }

    #[test]
    fn vertical_motion_and_pointer_positions_stay_on_character_boundaries() {
        let mut d = document("ab\né🙂\n");
        d.caret.as_mut().unwrap().byte = 1;
        let down = KeyEvent {
            key_code: KeyCode::ArrowDown,
            ..Default::default()
        };
        assert!(move_caret(&mut d, Stream::Files, &down));
        assert_eq!(
            d.caret.unwrap(),
            Caret {
                blob: 0,
                line: 1,
                byte: 0
            }
        );
        assert_eq!(caret_at(&d, Stream::Files, 1, 4).unwrap().byte, 2);
        assert!(type_text(&mut d, "x"));
        assert_eq!(d.blobs[0].text, "ab\nxé🙂\n");
    }
    #[test]
    fn search_follows_the_requested_stream_without_a_caret() {
        let mut d = document("one NEEDLE\ntwo needle\n");
        d.caret = None;
        d.comments_rows = vec![Row::Comment {
            id: 1,
            parent: None,
            body: "a needle in the comment".into(),
            meta: "Alice".into(),
            quote: None,
        }];
        d.guide_rows = vec![
            Row::Title {
                text: "Needle overview".into(),
            },
            Row::Prose {
                md: "Explain this needle".into(),
            },
        ];
        d.files_open.push(crate::review_doc::FileView {
            tab: 42,
            path: "one.txt".into(),
            rows: d.files_rows[..1].to_vec(),
            base: None,
            head: 0,
            heading: None,
        });
        let hits = |stream, query| search_hits(&d, stream, &literal(query));
        assert_eq!(hits(Stream::Files, "needle").len(), 2);
        assert_eq!(hits(Stream::Comments, "needle").len(), 1);
        assert_eq!(hits(Stream::Comments, "alice"), [(0, (0, 5))]);
        assert_eq!(hits(Stream::Guide, "needle").len(), 2);
        assert_eq!(hits(Stream::File(42), "needle").len(), 1);
        assert_eq!(hits(Stream::Files, ""), []);
    }

    /// Find searches a renamed card's title as it is drawn, so its offsets
    /// land on the drawn text.
    #[test]
    fn search_finds_a_rename_where_its_title_draws_it() {
        let mut d = document("x\n");
        d.files_rows.insert(
            0,
            Row::FileHeader {
                path: "new.rs".into(),
                lang: "rust",
                adds: 0,
                dels: 0,
                from: Some("old.rs".into()),
                similarity: Some(100),
            },
        );
        let title = header_title("new.rs", Some("old.rs"));
        let hits = search_hits(&d, Stream::Files, &literal("new.rs"));
        let at = title.find("new.rs").unwrap();
        assert_eq!(hits, [(0, (at, at + 6))]);
        assert_eq!(
            search_hits(&d, Stream::Files, &literal("old.rs  →  new")).len(),
            1
        );
    }

    #[test]
    fn search_returns_unicode_byte_offsets_and_nonoverlapping_matches() {
        let d = document("é🙂→→ aaa\n");
        assert_eq!(
            search_hits(&d, Stream::Files, &literal("→")),
            [(0, (6, 9)), (0, (9, 12))]
        );
        assert_eq!(
            search_hits(&d, Stream::Files, &literal("AA")),
            [(0, (13, 15))]
        );
    }
    #[test]
    fn replace_all_edits_only_the_tabs_code_and_deduplicates_repeated_rows() {
        let mut d = document("é x x\noutside x\n");
        let mut second = Blob::new(
            concats_sync::hash_object(b"x\n"),
            "txt".into(),
            "x\n".into(),
        );
        second.origin = Some("/tmp/second.txt".into());
        let readonly = Blob::new(
            concats_sync::hash_object(b"x\n"),
            "txt".into(),
            "x\n".into(),
        );
        d.blobs.extend([second, readonly]);
        d.guide_rows = vec![
            d.files_rows[0].clone(),
            d.files_rows[0].clone(),
            Row::add(1, 0),
            Row::del(2, 0),
            Row::Comment {
                id: 1,
                parent: None,
                body: "x".into(),
                meta: "author".into(),
                quote: None,
            },
        ];
        d.selection_anchor = d.caret;
        assert!(replace_all(&mut d, Stream::Guide, &literal("x"), "target"));
        assert_eq!(d.blobs[0].text, "é target target\noutside x\n");
        assert_eq!(d.blobs[1].text, "target\n");
        assert_eq!(d.blobs[2].text, "x\n");
        assert!(matches!(&d.guide_rows[4], Row::Comment { body, .. } if body == "x"));
        assert_eq!(d.caret.unwrap().byte, 9);
        assert!(d.selection_anchor.is_none());
    }

    #[test]
    fn replace_one_refuses_a_caret_from_another_tab() {
        let mut d = document("é x x\noutside x\n");
        d.guide_rows = vec![d.files_rows[1].clone()];
        d.caret.as_mut().unwrap().byte = 5;
        assert!(!replace_one(&mut d, Stream::Guide, &literal("x"), "long"));
        assert_eq!(d.blobs[0].text, "é x x\noutside x\n");
        assert!(replace_one(&mut d, Stream::Files, &literal("X"), "long"));
        assert_eq!(d.blobs[0].text, "é x long\noutside x\n");
        assert_eq!(d.caret.unwrap().byte, 9);
        assert!(!replace_one(
            &mut d,
            Stream::Files,
            &literal(""),
            "anything"
        ));
    }

    #[test]
    fn replace_takes_the_length_of_each_match() {
        let runs = Find::new(
            "x+".into(),
            Options {
                regex: true,
                ..Options::default()
            },
        );
        let mut d = document("a xx x xxx\n");
        assert!(replace_all(&mut d, Stream::Files, &runs, "y"));
        assert_eq!(d.blobs[0].text, "a y y y\n");
        let mut d = document("a xx x\n");
        d.caret.as_mut().unwrap().byte = 3;
        assert!(replace_one(&mut d, Stream::Files, &runs, "y"));
        assert_eq!(d.blobs[0].text, "a y x\n");
    }

    fn literal(query: &str) -> Find {
        Find::new(query.to_owned(), Options::default())
    }

    fn blob(text: &str) -> Blob {
        Blob::new(oid(1), "rs".into(), text.into())
    }

    fn doc_with(text: &str, editable: bool) -> ReviewDoc {
        let mut b = blob(text);
        if editable {
            b.origin = Some("/tmp/a.rs".into());
        }
        ReviewDoc {
            blobs: vec![b],
            caret: Some(Caret {
                blob: 0,
                line: 1,
                byte: 1,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn typing_lands_at_the_caret_and_carries_it_along() {
        let mut d = doc_with("ab\ncd\n", true);
        assert!(type_at(&mut d, "X", 0));
        assert_eq!(d.blobs[0].text, "ab\ncXd\n");
        assert_eq!(
            d.caret,
            Some(Caret {
                blob: 0,
                line: 1,
                byte: 2
            })
        );
    }

    #[test]
    fn backspace_at_the_start_of_a_line_joins_it_to_the_one_above() {
        let mut d = doc_with("ab\ncd\n", true);
        d.caret = Some(Caret {
            blob: 0,
            line: 1,
            byte: 0,
        });
        assert!(type_at(&mut d, "", 1));
        assert_eq!(d.blobs[0].text, "abcd\n");
        assert_eq!(
            d.caret,
            Some(Caret {
                blob: 0,
                line: 0,
                byte: 2
            })
        );
    }

    #[test]
    fn a_newline_puts_the_caret_at_the_start_of_the_line_it_made() {
        let mut d = doc_with("ab\ncd\n", true);
        assert!(type_at(&mut d, "\n", 0));
        assert_eq!(d.blobs[0].text, "ab\nc\nd\n");
        assert_eq!(
            d.caret,
            Some(Caret {
                blob: 0,
                line: 2,
                byte: 0
            })
        );
    }

    /// The one gate on editing: a blob with no file behind it is read-only.
    /// That is what makes a commit range and every deleted-side row immutable
    /// with no check at the call sites.
    #[test]
    fn a_blob_with_no_file_behind_it_takes_no_text() {
        let mut d = doc_with("ab\ncd\n", false);
        assert!(!type_at(&mut d, "X", 0));
        assert_eq!(d.blobs[0].text, "ab\ncd\n");
        assert!(!d.blobs[0].dirty());
    }

    #[test]
    fn caret_motion_walks_code_rows_and_steps_over_the_chrome_between_them() {
        let rows = vec![
            Row::FileHeader {
                path: "a.rs".into(),
                lang: "rust",
                adds: 0,
                dels: 0,
                from: None,
                similarity: None,
            },
            code(LineKind::Context, 0, 0),
            Row::Comment {
                quote: None,
                id: 1,
                parent: None,
                body: String::new(),
                meta: String::new(),
            },
            Row::HunkBar {
                old: None,
                new: None,
            },
            code(LineKind::Add, 0, 1),
            Row::CardEnd,
        ];
        assert_eq!(
            step_row(&rows, 1, Step::Down),
            Some(4),
            "over comment + bar"
        );
        assert_eq!(step_row(&rows, 4, Step::Up), Some(1));
        // A caret does not walk out of its file into the chrome of the next.
        assert_eq!(step_row(&rows, 4, Step::Down), None);
        assert_eq!(step_row(&rows, 1, Step::Up), None);

        let caret = Caret {
            blob: 0,
            line: 1,
            byte: 0,
        };
        assert_eq!(caret_row(&rows, caret), Some(4));
    }

    /// A buffer with a caret at `head` and, when given, an anchor at `tail` —
    /// both as `(line, byte)` on the one editable blob.
    fn with_selection(text: &str, head: (u32, usize), tail: Option<(u32, usize)>) -> ReviewDoc {
        let mut blob = blob(text);
        blob.origin = Some(std::path::PathBuf::from("/repo/a.rs"));
        let at = |(line, byte): (u32, usize)| Caret {
            blob: 0,
            line,
            byte,
        };
        ReviewDoc {
            blobs: vec![blob],
            caret: Some(at(head)),
            selection_anchor: tail.map(at),
            ..Default::default()
        }
    }

    #[test]
    fn a_selection_is_the_byte_range_between_its_two_ends() {
        // "b" on the second line, both ways round: which end moved is not what
        // decides the range.
        let forward = with_selection("aa\nbb\ncc\n", (1, 2), Some((1, 0)));
        assert_eq!(
            selection(&forward),
            Some(Selection {
                blob: 0,
                from: 3,
                to: 5
            })
        );
        let backward = with_selection("aa\nbb\ncc\n", (1, 0), Some((1, 2)));
        assert_eq!(
            selection(&backward),
            Some(Selection {
                blob: 0,
                from: 3,
                to: 5
            })
        );
        // Across lines.
        let across = with_selection("aa\nbb\ncc\n", (2, 1), Some((0, 1)));
        assert_eq!(
            selection(&across),
            Some(Selection {
                blob: 0,
                from: 1,
                to: 7
            })
        );
        // No anchor, or an empty range, is not a selection.
        assert_eq!(selection(&with_selection("aa\n", (0, 1), None)), None);
        assert_eq!(
            selection(&with_selection("aa\n", (0, 1), Some((0, 1)))),
            None
        );
    }

    /// The thing that could not be done before: select text and delete it.
    #[test]
    fn typing_over_a_selection_replaces_the_whole_of_it() {
        // Bytes 4..8 of `let x = 1;` are `x = `, so this replaces the name and
        // the assignment with one word.
        let mut d = with_selection("let x = 1;\nlet y = 2;\n", (0, 8), Some((0, 4)));
        assert!(type_at(&mut d, "value", 0));
        assert_eq!(d.blobs[0].text, "let value1;\nlet y = 2;\n");
        assert_eq!(
            d.caret,
            Some(Caret {
                blob: 0,
                line: 0,
                byte: 9
            }),
            "and leaves the caret after what went in"
        );
        assert_eq!(d.selection_anchor, None, "the selection is consumed");
    }

    #[test]
    fn backspace_over_a_selection_takes_the_selection_and_not_one_byte() {
        let mut d = with_selection("let x = 1;\nlet y = 2;\n", (1, 10), Some((0, 0)));
        // `back` is the single-position behaviour and must not apply here.
        assert!(type_at(&mut d, "", 1));
        assert_eq!(d.blobs[0].text, "\n");
        assert_eq!(
            d.caret,
            Some(Caret {
                blob: 0,
                line: 0,
                byte: 0
            })
        );
    }

    #[test]
    fn a_selection_spanning_two_blobs_is_readable_but_not_replaceable() {
        // The old and new sides of a hunk: the text between the ends does not
        // exist in either blob, so there is no range to replace.
        let mut d = with_selection("aa\nbb\n", (0, 0), None);
        d.blobs.push(blob("cc\n"));
        d.selection_anchor = Some(Caret {
            blob: 1,
            line: 0,
            byte: 2,
        });
        assert_eq!(selection(&d), None);
        assert!(!replace_selection(&mut d, "x"), "nothing to replace");
    }

    #[test]
    fn a_read_only_blob_refuses_to_have_its_selection_replaced() {
        let mut d = with_selection("aa\nbb\n", (0, 2), Some((0, 0)));
        d.blobs[0].origin = None;
        assert!(!replace_selection(&mut d, "x"));
        assert_eq!(d.blobs[0].text, "aa\nbb\n");
    }
}
