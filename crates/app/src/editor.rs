//! Editing a buffer through the document: what a keystroke does to the
//! `ReviewDoc` once the list has routed it to the caret. These operations
//! mutate document values without reading widget state or performing I/O.
//!
//! `file_view` owns what happens to a file beyond its text — the save plan,
//! re-lowering after an edit — and is called by the widget after an edit.

use concats_diff::Row;

use crate::{
    makepad_widgets::{KeyCode, KeyEvent},
    review_doc::{
        Caret, ReviewDoc, Step, Stream, caret_row, caret_to, replace_selection, step_row, type_at,
    },
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
    let blob = &d.blobs[caret.blob as usize];
    let line = blob.line_text(caret.line as usize);
    let at = caret.byte;
    // Off the end of the line, the character to delete is the newline itself,
    // which joins the line below onto this one.
    let width = match (at + 1..=line.len()).find(|i| line.is_char_boundary(*i)) {
        Some(next) => next - at,
        None if blob.line_of(blob.line_starts[caret.line as usize] as usize + at + 1) > 0 => 1,
        None => return false,
    };
    let start = blob.line_starts[caret.line as usize] as usize + at;
    if start + width > blob.text.len() {
        return false;
    }
    let blob = &mut d.blobs[caret.blob as usize];
    if !blob.editable() {
        return false;
    }
    blob.edit(start..start + width, "");
    true
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "The diff row model uses u32 line indices; a buffer cannot practically contain 2^32 lines."
)]
fn undo_at(d: &mut ReviewDoc, redo: bool) -> bool {
    let Some(caret) = d.caret else {
        return false;
    };
    let blob = &mut d.blobs[caret.blob as usize];
    let Some(at) = (if redo { blob.redo() } else { blob.undo() }) else {
        return false;
    };
    let line = blob.line_of(at);
    d.caret = Some(Caret {
        blob: caret.blob,
        line: line as u32,
        byte: at - blob.line_starts[line] as usize,
    });
    true
}

/// The delimiter that closes `input`, when it is a lone opening one.
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

/// Literal matches in stream order, as (row, byte) positions. ASCII case
/// folding preserves byte offsets in Unicode text.
pub(crate) fn search_hits(d: &ReviewDoc, stream: Stream, query: &str) -> Vec<(usize, usize)> {
    use std::borrow::Cow;

    if query.is_empty() {
        return Vec::new();
    }
    let query = query.to_ascii_lowercase();
    d.stream(stream)
        .iter()
        .enumerate()
        .flat_map(|(row, value)| {
            let text: Cow<'_, str> = match value {
                Row::Title { text } | Row::Warning { text } => text.as_str().into(),
                Row::Prose { md } => md.as_str().into(),
                Row::Comment { body, meta, .. } => format!("{meta}\n{body}").into(),
                Row::FileHeader {
                    path,
                    from: Some(from),
                    ..
                } => format!("{from} → {path}").into(),
                Row::FileHeader { path, .. } => path.as_str().into(),
                Row::Code { blob, line, .. } => {
                    d.blobs[*blob as usize].line_text(*line as usize).into()
                }
                _ => "".into(),
            };
            text.to_ascii_lowercase()
                .match_indices(&query)
                .map(|(byte, _)| (row, byte))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Replace the match under the caret only when that code belongs to this tab.
pub(crate) fn replace_one(d: &mut ReviewDoc, tab: Stream, query: &str, insert: &str) -> bool {
    let Some(caret) = d.caret else {
        return false;
    };
    if query.is_empty() || caret_row(d.stream(tab), caret).is_none() {
        return false;
    }
    let blob = &mut d.blobs[caret.blob as usize];
    if !blob.editable() {
        return false;
    }
    let at = blob.line_starts[caret.line as usize] as usize + caret.byte;
    let hay = blob.text.to_ascii_lowercase();
    let query = query.to_ascii_lowercase();
    let Some((start, _)) = hay
        .match_indices(&query)
        .find(|(start, _)| *start <= at && at < *start + query.len())
    else {
        return false;
    };
    blob.break_group();
    blob.edit(start..start + query.len(), insert);
    caret_to(d, caret.blob, start + insert.len());
    true
}

/// Replace editable code matches in this tab. Repeated views of one line
/// name one edit; reverse byte order keeps the remaining positions valid.
pub(crate) fn replace_all(d: &mut ReviewDoc, tab: Stream, query: &str, insert: &str) -> bool {
    let hits: std::collections::BTreeSet<_> = search_hits(d, tab, query)
        .into_iter()
        .filter_map(|(row, byte)| {
            let Row::Code { blob, line, .. } = d.stream(tab).get(row)? else {
                return None;
            };
            let buffer = &d.blobs[*blob as usize];
            buffer
                .editable()
                .then_some((*blob, buffer.line_starts[*line as usize] as usize + byte))
        })
        .collect();
    let Some(&(first_blob, first_byte)) = hits.first() else {
        return false;
    };
    for (blob, at) in hits.into_iter().rev() {
        let buffer = &mut d.blobs[blob as usize];
        buffer.break_group();
        buffer.edit(at..at + query.len(), insert);
    }
    caret_to(d, first_blob, first_byte + insert.len());
    true
}

/// Record a settings save result after the caller has applied it.
pub(crate) fn record_settings(d: &mut ReviewDoc, text: &str, error: Option<&str>) {
    if let Some(rows) = d.stream_mut(Stream::File(crate::dock::settings_tab_id().0)) {
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

#[cfg(test)]
mod tests {
    use concats_diff::{Blob, LineKind};

    use super::*;

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
        assert_eq!(search_hits(&d, Stream::Files, "needle").len(), 2);
        assert_eq!(search_hits(&d, Stream::Comments, "needle").len(), 1);
        assert_eq!(search_hits(&d, Stream::Comments, "alice"), [(0, 0)]);
        assert_eq!(search_hits(&d, Stream::Guide, "needle").len(), 2);
        assert_eq!(search_hits(&d, Stream::File(42), "needle").len(), 1);
        assert!(search_hits(&d, Stream::Files, "").is_empty());
    }

    #[test]
    fn search_returns_unicode_byte_offsets_and_nonoverlapping_matches() {
        let d = document("é🙂→→ aaa\n");
        assert_eq!(search_hits(&d, Stream::Files, "→"), [(0, 6), (0, 9)]);
        assert_eq!(search_hits(&d, Stream::Files, "AA"), [(0, 13)]);
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
            Row::Code {
                kind: LineKind::Add,
                old_no: None,
                new_no: Some(1),
                blob: 1,
                line: 0,
            },
            Row::Code {
                kind: LineKind::Del,
                old_no: Some(1),
                new_no: None,
                blob: 2,
                line: 0,
            },
            Row::Comment {
                id: 1,
                parent: None,
                body: "x".into(),
                meta: "author".into(),
            },
        ];
        d.selection_anchor = d.caret;
        assert!(replace_all(&mut d, Stream::Guide, "x", "target"));
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
        assert!(!replace_one(&mut d, Stream::Guide, "x", "long"));
        assert_eq!(d.blobs[0].text, "é x x\noutside x\n");
        assert!(replace_one(&mut d, Stream::Files, "X", "long"));
        assert_eq!(d.blobs[0].text, "é x long\noutside x\n");
        assert_eq!(d.caret.unwrap().byte, 9);
        assert!(!replace_one(&mut d, Stream::Files, "", "anything"));
    }
}
