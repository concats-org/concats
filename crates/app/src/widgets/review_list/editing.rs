//! The list's editing shell: the caret, the find bar and save, driven by the
//! keys and pointer gestures the list routes here. What they do to the
//! document is [`crate::editor`]'s and [`crate::file_view`]'s.

use concats_diff::Row;

use super::{ReviewList, ReviewListAction};
#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::{
    editor,
    file_view::{relower_edited, save_plan, untitled},
    makepad_widgets::*,
    service::{ReviewCmd, review},
    widgets::find_row,
};

impl ReviewList {
    /// Find in the tab bar: the query as it is typed, Enter or the arrows to
    /// walk the matches, Enter in the second field to replace, × to close.
    pub(super) fn handle_find(&mut self, cx: &mut Cx, actions: &Actions) {
        let Some(current) = &self.find else {
            return;
        };
        // Every keystroke or flip: the hits are recomputed per row per draw, so
        // simply redrawing is what makes them follow the query so far.
        if let Some(changed) = find_row::changed(&self.view, cx, actions, current) {
            self.find = Some(changed);
            self.find_at = None;
            self.count_hits(cx);
            self.redraw(cx);
        }
        let find = self.view.text_input(cx, ids!(find_input));
        let back = self.view.button(cx, ids!(find_prev)).clicked(actions);
        let walked = find.returned(actions).map(|(_, modifiers)| modifiers.shift);
        let next = self.view.button(cx, ids!(find_next)).clicked(actions);
        if let Some(back) = walked.or(back.then_some(true)).or(next.then_some(false)) {
            self.seek(cx, back);
            self.count_hits(cx);
            find.set_key_focus(cx);
        }
        if self.view.button(cx, ids!(find_close)).clicked(actions) {
            self.close_find(cx);
            return;
        }
        let replace = self.view.text_input(cx, ids!(replace_input));
        if let Some((with, modifiers)) = replace.returned(actions) {
            self.replace(cx, &with, modifiers.logo || modifiers.control);
            self.count_hits(cx);
            replace.set_key_focus(cx);
        }
    }
}

impl ReviewList {
    /// The bar's tally over the stream this tab renders: which match the
    /// caret is on, of how many. 0 until Enter or an arrow walks to one.
    pub(super) fn count_hits(&mut self, cx: &mut Cx) {
        let find = self.find.clone().unwrap_or_default();
        let hits = self.drawn_tab.map_or_else(Vec::new, |tab| {
            self.state().read(|d| editor::search_hits(d, tab, &find))
        });
        let at = hits
            .iter()
            .position(|&(row, (start, _))| Some((row, start)) == self.find_at)
            .map_or(0, |i| i + 1);
        let label = if find.query().is_empty() {
            String::new()
        } else {
            format!("{at}/{}", hits.len())
        };
        self.view.label(cx, ids!(find_count)).set_text(cx, &label);
    }
}

impl ReviewList {
    /// Move the caret to wherever the list's pointer gesture just landed.
    ///
    /// NOTE: A drag supplies both endpoints so typing can replace the selection.
    pub(super) fn adopt_list_selection(&mut self, cx: &mut Cx) {
        let list = self.view.portal_list(cx, ids!(list));
        let Some((start, end)) = list.borrow().and_then(|l| l.get_selection_range()) else {
            return;
        };
        let row = self.row_at(end.0);
        let Some(tab) = self.drawn_tab else {
            return;
        };
        let head_row = row;
        let tail_row = self.row_at(start.0);
        let mut focus = false;
        self.state().with(|d| {
            let head = head_row.and_then(|row| editor::caret_at(d, tab, row, end.1));
            let tail = (start != end)
                .then(|| tail_row.and_then(|row| editor::caret_at(d, tab, row, start.1)))
                .flatten();
            if head.is_some() {
                // This stream owns the caret now, which is what routes
                // keystrokes to one list rather than to every instance.
                d.tab = tab;
                focus = true;
            }
            // NOTE: Editing needs both endpoints to replace the selected text.
            d.selection_anchor = tail;
            d.caret = head;
        });
        // Claim key focus for the surface the caret is on. Without it nothing
        // holds focus after a press on a code row (`DiffLine` is not
        // interactive on purpose) and typed characters have no owner.
        if focus {
            let area = self.view.portal_list(cx, ids!(list)).area();
            cx.set_key_focus(area);
        }
        self.redraw(cx);
    }

    /// Typed text — from the keyboard, an IME, or a paste. Returns whether the
    /// caret took it.
    pub(super) fn caret_text(&mut self, cx: &mut Cx, input: &str) -> bool {
        if !self.caret_focused(cx) {
            return false;
        }
        let took = self.state().with(|d| editor::type_text(d, input));
        if took {
            self.after_edit(cx);
        }
        took
    }

    /// Whether this list is the one a keystroke should reach.
    ///
    /// Key focus alone cannot decide it: several lists of the same kind exist
    /// at once (the dock keeps an instance per pane it can show), and they all
    /// answer `has_key_focus` for the same area, so gating on focus alone
    /// applied every keystroke once per instance. The caret belongs to a
    /// stream, and `ReviewDoc::tab` already names the stream the current
    /// gesture owns, so that settles it.
    pub(super) fn caret_focused(&mut self, cx: &mut Cx) -> bool {
        let list = self.view.portal_list(cx, ids!(list));
        let Some(tab) = self.drawn_tab else {
            return false;
        };
        // An empty area answers `has_key_focus` for whatever is focused when
        // nothing is, so an instance that never drew would claim every key.
        let area = list.area();
        !matches!(area, Area::Empty)
            && cx.has_key_focus(area)
            && self
                .state()
                .read(|d| d.caret.is_some() && d.tab == tab && d.composer_tab != Some(tab))
    }

    /// The list's own selection and the caret are two claims about where typing
    /// goes; a keystroke settles it in the caret's favour. Then the edited file
    /// is lowered again, so its add/removed marks describe what you have now
    /// rather than what was on disk when it opened.
    ///
    /// Re-lowering runs here rather than on a timer: it is one histogram diff
    /// of one file. The tree-sitter re-parse behind it needs no debounce
    /// either. A request carries the `rev` it was made for and the worker drops
    /// one whose blob has moved on, so fast typing coalesces on its own.
    fn after_edit(&mut self, cx: &mut Cx) {
        if let Some(mut l) = self.view.portal_list(cx, ids!(list)).borrow_mut() {
            l.clear_selection(cx);
        }
        let git_dir = self.state().read(|d| d.git_dir.clone());
        let comments = crate::service::review_state(git_dir.as_deref())
            .load()
            .comments
            .clone();
        self.state().with(|d| relower_edited(d, &comments));
        self.redraw(cx);
    }

    /// Arrow keys, Home and End, and the keys that change text: Return, Tab,
    /// Backspace, Delete, undo and redo. Returns whether the caret took the key
    /// — which is what stops the list from scrolling on the same press.
    pub(super) fn caret_keys(&mut self, cx: &mut Cx, ke: &KeyEvent) -> bool {
        if !self.caret_focused(cx) {
            return false;
        }
        if ke.key_code == KeyCode::KeyS && (ke.modifiers.logo || ke.modifiers.control) {
            return self.save(cx);
        }
        if let Some(took) = self.state().with(|d| editor::edit_key(d, ke)) {
            if took {
                self.after_edit(cx);
            }
            return took;
        }
        let Some(tab) = self.drawn_tab else {
            return false;
        };
        let took = self.state().with(|d| editor::move_caret(d, tab, ke));
        if took {
            if let Some(mut l) = self.view.portal_list(cx, ids!(list)).borrow_mut() {
                l.clear_selection(cx);
            }
            self.redraw(cx);
        }
        took
    }

    // NOTE: says whether it took the key, as every key handler here does; Escape closes the bar.
    // coland-ignore: @coland/read_write_operation
    pub(super) fn find_keys(&mut self, cx: &mut Cx, ke: &KeyEvent) -> bool {
        let Some(tab) = self.drawn_tab else {
            return false;
        };
        let areas = [
            self.view.portal_list(cx, ids!(list)).area(),
            self.view.text_input(cx, ids!(find_input)).area(),
            self.view.text_input(cx, ids!(replace_input)).area(),
        ];
        let focused = areas
            .into_iter()
            .any(|area| !matches!(area, Area::Empty) && cx.has_key_focus(area));
        if !focused || !self.state().read(|d| d.tab == tab) {
            return false;
        }
        match ke.key_code {
            KeyCode::KeyF if ke.modifiers.logo || ke.modifiers.control => self.open_find(cx),
            // Escape closes the bar and leaves the caret where searching put
            // it, so you carry on from what you found.
            KeyCode::Escape if self.find.is_some() => {
                self.close_find(cx);
                true
            }
            _ => false,
        }
    }

    /// Close find and give the keyboard back to the rows, leaving the caret
    /// where searching put it.
    fn close_find(&mut self, cx: &mut Cx) {
        self.find = None;
        self.find_at = None;
        cx.set_key_focus(self.view.portal_list(cx, ids!(list)).area());
        self.redraw(cx);
    }

    /// Open the find bar and put the keyboard in it.
    fn open_find(&mut self, cx: &mut Cx) -> bool {
        self.find.get_or_insert_default();
        // Shown and focused from the draw, not here: a node that has never been
        // drawn does not resolve, so `set_visible` on a bar that starts hidden
        // is a silent no-op. Same reason the pinned header is always laid out
        // rather than toggled.
        self.focus_find = true;
        self.redraw(cx);
        true
    }

    /// Walk matches in this tab, unfolding and scrolling to the matching row.
    pub(super) fn seek(&mut self, cx: &mut Cx, back: bool) {
        let Some(find) = self.find.clone() else {
            return;
        };
        let Some(tab) = self.drawn_tab else {
            return;
        };
        let hits: Vec<_> = self
            .state()
            .read(|d| editor::search_hits(d, tab, &find))
            .into_iter()
            .map(|(row, (start, _))| (row, start))
            .collect();
        let found = if back {
            hits.iter()
                .rev()
                .find(|hit| self.find_at.is_none_or(|at| **hit < at))
                .or_else(|| hits.last())
        } else {
            hits.iter()
                .find(|hit| self.find_at.is_none_or(|at| **hit > at))
                .or_else(|| hits.first())
        };
        let Some(&(row, byte)) = found else {
            return;
        };
        self.find_at = Some((row, byte));
        self.seek_row = Some((row, 0.0));
        self.state().with(|d| {
            d.tab = tab;
            d.caret = editor::caret_at(d, tab, row, byte);
            d.selection_anchor = None;
            if let Some(path) = d.stream(tab)[..=row]
                .iter()
                .rev()
                .find_map(|row| match row {
                    Row::FileHeader { path, .. } => Some(path.clone()),
                    _ => None,
                })
            {
                d.folded.remove(&path);
            }
        });
        self.redraw(cx);
    }

    /// Replace the occurrence the caret sits on, or every one of them.
    fn replace(&mut self, cx: &mut Cx, with: &str, all: bool) {
        let Some(find) = self.find.clone() else {
            return;
        };
        let Some(tab) = self.drawn_tab else {
            return;
        };
        let changed = self.state().with(|d| {
            if all {
                editor::replace_all(d, tab, &find, with)
            } else {
                editor::replace_one(d, tab, &find, with)
            }
        });
        if changed {
            self.after_edit(cx);
        }
    }

    /// Write the caret's buffer back to its file, and carry everything anchored
    /// to its old content across to the new hash.
    ///
    /// The two halves are kept apart: the arithmetic is worked out here as a
    /// plan over the document, and the service, which alone owns the store,
    /// performs the write and the anchor moves.
    fn save(&mut self, cx: &mut Cx) -> bool {
        let settings = self.state().read(|d| {
            let path = crate::theme::config_file()?;
            let blob = d.blobs.get(d.caret?.blob as usize)?;
            (blob.dirty() && blob.origin.as_ref() == Some(&path)).then(|| blob.text.clone())
        });
        if let Some(text) = settings {
            return self.apply_settings(cx, &text);
        }
        let untitled = self.state().read(|d| {
            let blob = d.caret?.blob;
            d.files_open
                .iter()
                .find(|view| view.head == blob && untitled(&d.blobs[blob as usize]))
                .map(|view| view.tab)
        });
        if let Some(tab) = untitled {
            cx.widget_action(self.widget_uid(), ReviewListAction::SaveAs(tab));
            return true;
        }
        let Some(plan) = self.state().with(|d| save_plan(d, d.caret?.blob)) else {
            return false;
        };
        let sent = self.state().with(|d| {
            let Some(git_dir) = d.git_dir.clone() else {
                return false;
            };
            review().send(ReviewCmd::SaveFile {
                window: self.state().id,
                git_dir,
                plan,
            });
            true
        });
        if sent {
            self.redraw(cx);
        }
        sent
    }

    /// Saving the settings: parse them first, and only then let them land.
    /// A parse error is spliced in as the loud row the document already has
    /// for "this does not refer to anything real".
    fn apply_settings(&mut self, cx: &mut Cx, text: &str) -> bool {
        let applied = crate::theme::apply_settings_text(text);
        let error = applied.as_ref().err().map(ToString::to_string);
        self.state()
            .with(|d| editor::record_settings(d, text, error.as_deref()));
        if applied.is_ok() {
            cx.request_live_edit();
        }
        self.redraw(cx);
        true
    }
}
