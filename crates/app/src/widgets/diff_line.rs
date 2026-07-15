//! The diff line: one row's code text, drawn span by span in the theme's syntax
//! colours. This is the row's non-interactive half. The `Gutter` to its left
//! owns hover, press and drag, and that is what keeps text drag-selection
//! working everywhere else.
//!
//! Selection itself belongs to the `PortalList`: it spans rows, and a row does
//! not know its neighbours. The list drives it through the `WidgetNode`
//! selection API, and this row answers from the same laid-out text it draws.

use concats_diff::LineKind;
use concats_syntax::Span;

use crate::{frame_theme, makepad_widgets::*, row_bg, row_selected_bg, theme::paint, ROW_PAD};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.FONT

    mod.widgets.DiffLine = #(DiffLine::register_widget(vm)) {
        width: Fill
        height: Fit
        // Without an explicit text_style the DrawText has no font family and
        // silently draws nothing — you get the row tint and no glyphs.
        draw_text.text_style: FONT{line_spacing: 1.55}
        // The terminal's selection tint, so a selection reads the same in both.
        draw_sel +: {
            color: mod.app_theme.color_sel_focus
        }
        draw_caret +: {
            color: mod.app_theme.color_accent
        }
        // Find hits read as a marked passage, not as a selection: the two can
        // be on screen together and must not be mistaken for each other.
        draw_find +: {
            color: mod.app_theme.color_find
        }
    }
}

/// Caret width. Solid, not blinking: this row lives in a virtualized list, so
/// a blink would redraw every visible row twice a second to animate one quad.
const CARET_WIDTH: f64 = 1.5;

// The granular derives rather than `Widget`: that one also writes the
// `WidgetNode` impl, and this row answers the selection half of it itself.
#[derive(Script, ScriptHook, WidgetRef, WidgetSet, WidgetRegister)]
pub struct DiffLine {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_sel: DrawColor,
    #[live]
    draw_caret: DrawColor,
    #[live]
    draw_find: DrawColor,

    #[rust]
    kind: Option<LineKind>,
    #[rust]
    text: String,
    #[rust]
    spans: Vec<Span>,
    #[rust]
    selected: bool,

    /// This row's laid-out runs, rebuilt every draw. Maps a click to a byte
    /// offset and a byte range back to highlight rects.
    #[rust]
    sel_track: SelectionTracker,
    /// The list's text selection over this row, as byte offsets into `sel_text`.
    #[rust]
    sel: Option<(usize, usize)>,
    /// Right edge of the drawn glyphs, so a press in the empty run past a short
    /// line still anchors on this row instead of falling to the row above.
    #[rust]
    text_right: f64,
    /// The caret's byte offset into this row's text, when it is on this row.
    #[rust]
    caret: Option<usize>,
    /// Byte ranges of this row's find hits. Recomputed by the list each draw
    /// from the live text, so an edit can never leave a stale one behind.
    #[rust]
    hits: Vec<(usize, usize)>,
    /// Where the caret was drawn, in window coordinates. The list reads it back
    /// after drawing to place the IME — which has to know where the text being
    /// composed will appear.
    #[rust]
    caret_rect: Option<Rect>,
}

impl DiffLine {
    /// Point this row's widget at one diff line — its content and selected tint.
    /// (Called by `ReviewList` as it recycles list items.)
    pub fn set_row(
        &mut self,
        kind: LineKind,
        text: &str,
        spans: &[Span],
        selected: bool,
        caret: Option<usize>,
    ) {
        self.kind = Some(kind);
        self.text.clear();
        self.text.push_str(text);
        self.spans.clear();
        self.spans.extend_from_slice(spans);
        self.selected = selected;
        self.caret = caret;
    }

    /// Mark this row's find hits. Set beside `set_row` rather than through it:
    /// what a row is and what a search found in it are two different things,
    /// and only the second changes as you type in the find bar.
    pub fn set_hits(&mut self, hits: Vec<(usize, usize)>) {
        self.hits = hits;
    }

    /// Where this row drew the caret, if it had one. Valid until the next draw.
    pub fn caret_rect(&self) -> Option<Rect> {
        self.caret_rect
    }

    /// The caret as a thin quad, taken from the geometry of the character it
    /// sits in front of.
    ///
    /// A zero-width query does not work here. The tracker skips a run whose end
    /// is at or before the start of its text, so a caret landing exactly on a
    /// run boundary falls through every segment and vanishes. So we ask for one
    /// character and take its leading edge — or, at the end of the line, the
    /// trailing edge of the character before it.
    fn caret_quad(&self, at: usize) -> Option<Rect> {
        let text = self.sel_text();
        let at = at.min(text.len());
        if let Some(next) = (at + 1..=text.len()).find(|i| text.is_char_boundary(*i)) {
            let r = *self.sel_track.selection_rects(at, next).first()?;
            return Some(Rect {
                pos: r.pos,
                size: dvec2(CARET_WIDTH, r.size.y),
            });
        }
        let prev = (0..at).rev().find(|i| text.is_char_boundary(*i))?;
        let r = *self.sel_track.selection_rects(prev, at).last()?;
        Some(Rect {
            pos: dvec2(r.pos.x + r.size.x, r.pos.y),
            size: dvec2(CARET_WIDTH, r.size.y),
        })
    }

    /// What this row contributes to a selection: the text it draws, no more. An
    /// empty line draws one space (`DrawText` skips an empty string), and
    /// reporting that space keeps blank lines in a copied range: the list joins
    /// two rows with a newline only when both carry text.
    fn sel_text(&self) -> &str {
        if self.text.is_empty() {
            " "
        } else {
            &self.text
        }
    }
}

/// Draw one already-coloured run and record it for selection. Free-standing so
/// the caller can hold `text`/`spans` borrowed while `draw_text` and
/// `sel_track` are borrowed mutably. Returns the run's right edge.
///
/// The run is laid out unpositioned and the turtle places it. Handing the
/// layout a `first_row_indent` as well would apply the offset twice, once in
/// the glyph positions and once by the flow. That compounds across a line and
/// tears it apart — though only once syntax colours split the line into runs at
/// all.
fn draw_run(
    cx: &mut Cx2d,
    draw_text: &mut DrawText,
    sel_track: &mut SelectionTracker,
    text: &str,
) -> f64 {
    // Laid out here rather than through `draw_walk` so one layout answers both
    // the glyphs and the selection geometry (`Fonts` caches it either way).
    let laidout = draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
    let rect = draw_text.draw_walk_laidout(cx, Walk::fit(), &laidout);
    sel_track.push_text(laidout, rect.pos, draw_text.font_scale, text);
    rect.pos.x + rect.size.x
}

impl Widget for DiffLine {
    /// `Widget::is_interactive` defaults to true, and the list only starts a
    /// selection where it finds nothing interactive under the pointer. A code
    /// row that claims to be a click target stops code from being selectable at
    /// all; that was the bug. The `Gutter` beside it stays interactive and
    /// keeps the gestures.
    fn is_interactive(&self) -> bool {
        false
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let kind = self.kind.unwrap_or(LineKind::Context);
        let Some(theme) = frame_theme(scope) else {
            return DrawStep::done();
        };

        self.draw_bg.color = if self.selected {
            row_selected_bg(theme, kind)
        } else {
            row_bg(theme, kind)
        };

        let walk = Walk {
            width: Size::fill(),
            height: Size::fit(),
            ..walk
        };
        // Wrapping belongs to the turtle, not the layout: it flows the runs, so
        // it is the only thing that can break between them without applying the
        // offset twice. A line breaks at a run boundary. In code that is a
        // token boundary, which is where you would break it by hand anyway.
        let flow = if crate::theme::active_font().wrap {
            Layout::flow_right_wrap()
        } else {
            Layout::flow_right()
        };
        self.draw_bg.begin(
            cx,
            walk,
            flow.with_padding(Inset {
                left: 0.0,
                right: 16.0,
                top: ROW_PAD,
                bottom: ROW_PAD,
            }),
        );

        // Claim the highlight's draw call before any glyph goes out, so the
        // quads appended below merge into it and land under the text instead of
        // washing over it. Same trick as makepad's `TextInput`.
        self.draw_sel.append_to_draw_call(cx);

        // the line, span by span
        self.sel_track.clear();
        let mut right = 0.0;
        let bytes = self.text.as_bytes();
        let mut at = 0usize;
        for sp in &self.spans {
            let start = sp.start.min(bytes.len());
            let end = sp.end.min(bytes.len());
            if start > at {
                self.draw_text.color = paint(theme.syntax_color(None));
                if let Ok(s) = std::str::from_utf8(&bytes[at..start]) {
                    right = draw_run(cx, &mut self.draw_text, &mut self.sel_track, s);
                }
            }
            if end > start {
                self.draw_text.color = paint(theme.syntax_color(sp.hl));
                if let Ok(s) = std::str::from_utf8(&bytes[start..end]) {
                    right = draw_run(cx, &mut self.draw_text, &mut self.sel_track, s);
                }
            }
            at = at.max(end);
        }
        if at < bytes.len() {
            self.draw_text.color = paint(theme.syntax_color(None));
            if let Ok(s) = std::str::from_utf8(&bytes[at..]) {
                right = draw_run(cx, &mut self.draw_text, &mut self.sel_track, s);
            }
        }
        if self.text.is_empty() {
            self.draw_text.color = paint(theme.syntax_color(None));
            right = draw_run(cx, &mut self.draw_text, &mut self.sel_track, " ");
        }
        self.text_right = right;

        if let Some((anchor, cursor)) = self.sel {
            let (start, end) = (anchor.min(cursor), anchor.max(cursor));
            for rect in self.sel_track.selection_rects(start, end) {
                self.draw_sel.draw_abs(cx, rect);
            }
        }

        for (start, end) in &self.hits {
            for rect in self.sel_track.selection_rects(*start, *end) {
                self.draw_find.draw_abs(cx, rect);
            }
        }

        self.caret_rect = self.caret.and_then(|at| self.caret_quad(at));
        if let Some(rect) = self.caret_rect {
            self.draw_caret.draw_abs(cx, rect);
        }

        self.draw_bg.end(cx);
        DrawStep::done()
    }
}

/// The selection half is driven by the enclosing `PortalList`, which owns the
/// range across rows and asks each row to map points, text and highlights.
impl WidgetNode for DiffLine {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }

    fn walk(&mut self, _cx: &mut Cx) -> Walk {
        self.walk
    }

    fn area(&self) -> Area {
        self.draw_bg.area()
    }

    fn redraw(&mut self, cx: &mut Cx) {
        self.draw_bg.redraw(cx);
    }

    fn selection_text_len(&self) -> usize {
        self.sel_text().len()
    }

    fn selection_point_to_char_index(&self, cx: &Cx, abs: DVec2) -> Option<usize> {
        if let Some(index) = self.sel_track.point_to_index(cx, abs) {
            return Some(index);
        }
        // The tracker only answers within the glyphs it laid out. A press to
        // the right of a short line is still a press on this row and has to
        // anchor at its end; otherwise dragging to it drops the row.
        if !self.draw_bg.area().rect(cx).contains(abs) {
            return None;
        }
        Some(if abs.x >= self.text_right {
            self.sel_text().len()
        } else {
            0
        })
    }

    fn selection_set(&mut self, anchor: usize, cursor: usize) {
        self.sel = Some((anchor, cursor));
    }

    fn selection_clear(&mut self) {
        self.sel = None;
    }

    fn selection_select_all(&mut self) {
        self.sel = Some((0, self.sel_text().len()));
    }

    fn selection_get_text_for_range(&self, start: usize, end: usize) -> String {
        let text = self.sel_text();
        text.get(start.min(text.len())..end.min(text.len()))
            .unwrap_or_default()
            .to_string()
    }

    fn selection_get_full_text(&self) -> String {
        self.sel_text().to_string()
    }
}
