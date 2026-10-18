//! The diff line: one row's code text, drawn span by span in the theme's syntax
//! colours. This is the row's non-interactive half. The `Gutter` to its left
//! owns hover, press and drag, and that is what keeps text drag-selection
//! working everywhere else.
//!
//! Selection itself belongs to the `PortalList`: it spans rows, and a row does
//! not know its neighbours. The list drives it through the `WidgetNode`
//! selection API, and this row answers from the same laid-out text it draws.

use concats_diff::LineKind;
use concats_syntax::{Hl, Span};

#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::{ROW_PAD, frame_theme, line_bg, makepad_widgets::*, theme::paint};

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
        // The words a comment was made on: underlined in the colour its marker
        // uses, so the line says which of it the thread is about.
        draw_quote +: {
            color: mod.app_theme.color_accent
        }
    }
}

/// How much of a row an indent may take and still carry the hang: past it the
/// continuation would have less room than the indent it lines up under, so the
/// line wraps at the margin instead.
const HANG_LIMIT: f64 = 0.5;

/// How thick the rule under a quoted run is drawn.
const RULE_HEIGHT: f64 = 1.5;

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
    #[live]
    draw_quote: DrawColor,

    #[rust]
    kind: Option<LineKind>,
    #[rust]
    text: String,
    #[rust]
    spans: Vec<Span>,
    #[rust]
    selected: bool,
    #[rust]
    pub wrap: bool,
    /// Set by the row's template: this line is authored text, not code — it
    /// sits on the page instead of in a card.
    #[live]
    prose: bool,

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
    pub hits: Vec<(usize, usize)>,
    /// Byte ranges of this row a comment was made on, from the document's
    /// reading of the comments against the content.
    #[rust]
    pub quotes: Vec<(usize, usize)>,
    /// Where the caret was drawn, in window coordinates. The list reads it back
    /// after drawing to place the IME — which has to know where the text being
    /// composed will appear.
    #[rust]
    pub caret_rect: Option<Rect>,
    /// Where this row painted the last of the selection that runs through it,
    /// in window coordinates. The chip that offers to comment on a selection
    /// needs somewhere to sit, and only the row that drew it knows where that
    /// is — the same reason the caret's rect is read back for the IME.
    #[rust]
    pub sel_rect: Option<Rect>,
    /// The top of every visual row this line drew, in window coordinates and in
    /// order. One entry for a line that fits; the rest are what soft wrap
    /// produced, and the gutter marks them.
    #[rust]
    pub rows: Vec<f64>,
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

    /// Draw the line's glyphs and record what they came out as: the indent
    /// first, then the rest of the line, coloured run by coloured run, in a
    /// turtle that hangs under that indent. Returns the right edge of the last
    /// row it drew.
    fn draw_line(&mut self, cx: &mut Cx2d, theme: &crate::theme::Theme, flow: Layout) -> f64 {
        self.sel_track.clear();
        self.rows.clear();
        let mut right = 0.0;
        let bytes = self.text.as_bytes();
        // What the line is indented by, drawn on its own so the rest of it can
        // flow in a turtle that starts after it. That is what makes a wrapped
        // line hang under its own code instead of resuming at the margin,
        // where it reads as a new statement.
        let indent = self
            .text
            .find(|c: char| c != ' ' && c != '\t')
            .unwrap_or(self.text.len());
        let mut hanging = false;
        if indent > 0 {
            self.draw_text.color = paint(theme.syntax_color(None));
            right = draw_run(
                cx,
                &mut self.draw_text,
                &mut self.sel_track,
                &mut self.rows,
                &self.text[..indent],
            );
            let inner = cx.turtle().inner_rect();
            // Unless the indent is deep enough that the hang would leave the
            // continuation less room than the indent itself.
            hanging = self.wrap && (right - inner.pos.x) < inner.size.x * HANG_LIMIT;
            if hanging {
                cx.begin_turtle(
                    Walk {
                        width: Size::fill(),
                        height: Size::fit(),
                        ..Walk::default()
                    },
                    flow,
                );
            }
        }
        for (start, end, hl) in color_runs(&self.spans, indent, bytes.len()) {
            self.draw_text.color = paint(theme.syntax_color(hl));
            if let Ok(s) = std::str::from_utf8(&bytes[start..end]) {
                right = draw_run(
                    cx,
                    &mut self.draw_text,
                    &mut self.sel_track,
                    &mut self.rows,
                    s,
                );
            }
        }
        if self.text.is_empty() {
            self.draw_text.color = paint(theme.syntax_color(None));
            right = draw_run(
                cx,
                &mut self.draw_text,
                &mut self.sel_track,
                &mut self.rows,
                " ",
            );
        }
        if hanging {
            cx.end_turtle();
        }
        right
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

/// The coloured runs of a line from byte `from` on, in draw order: each gap
/// before a span uncoloured, then the span in its highlight, then whatever
/// follows the last span. Spans are clipped to `len`, and one that overlaps a
/// run already yielded starts where that run ended.
fn color_runs(
    spans: &[Span],
    from: usize,
    len: usize,
) -> impl Iterator<Item = (usize, usize, Option<Hl>)> + '_ {
    let mut at = from;
    let tail = spans
        .iter()
        .map(|sp| sp.end.min(len))
        .fold(from, usize::max);
    spans
        .iter()
        .flat_map(move |sp| {
            let start = sp.start.min(len).max(at);
            let end = sp.end.min(len);
            let gap = (start > at).then_some((at, start, None));
            let run = (end > start).then_some((start, end, sp.hl));
            at = at.max(end);
            gap.into_iter().chain(run)
        })
        .chain((tail < len).then_some((tail, len, None)))
}

/// Draw one already-coloured run and record it for selection. Free-standing so
/// the caller can hold `text`/`spans` borrowed while `draw_text` and
/// `sel_track` are borrowed mutably. Returns the right edge of its last row.
///
/// A run resumes where the previous one ended and breaks at the edge of the
/// row when the line is wider than the pane, so a long line is read by
/// scrolling down rather than sideways. Both halves have to agree about that:
/// the layout recorded for selection is laid out with the same indent, width
/// and wrapping the draw uses, or the highlight and the caret land where the
/// glyphs are not. (`Fonts` caches the layout, so asking twice costs one.)
#[expect(
    clippy::cast_possible_truncation,
    reason = "Layout is asked for in f32 lpxs; a window is not 2^24 pixels wide."
)]
fn draw_run(
    cx: &mut Cx2d,
    draw_text: &mut DrawText,
    sel_track: &mut SelectionTracker,
    rows: &mut Vec<f64>,
    text: &str,
) -> f64 {
    let turtle_pos = cx.turtle().pos();
    let turtle_rect = cx.turtle().inner_rect();
    let indent = (turtle_pos.x - turtle_rect.pos.x) as f32;
    let row_height = draw_text.resumable_first_row_min_spacing(cx);
    let wrap = matches!(cx.turtle().layout().flow, Flow::Right { wrap: true, .. });
    // Only a wrapping line has a width to obey; handing one to a line that does
    // not wrap is how it would come back truncated instead.
    let max_width = (wrap && !turtle_rect.size.x.is_nan()).then_some(turtle_rect.size.x as f32);
    let align = draw_text.layout_align;
    let laidout = draw_text.layout(cx, indent, row_height, max_width, wrap, align, text);
    sel_track.push_text(
        laidout,
        dvec2(turtle_rect.pos.x, turtle_pos.y),
        draw_text.font_scale,
        text,
    );
    let mut right = turtle_pos.x;
    draw_text.draw_walk_resumable_with(cx, text, |_, rect, _| {
        right = rect.pos.x + rect.size.x;
        // Runs share the row they continue on, so the same top arrives once per
        // run; the line's rows are the distinct ones, in order.
        if rows.last() != Some(&rect.pos.y) {
            rows.push(rect.pos.y);
        }
    });
    right
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

        self.draw_bg.color = line_bg(theme, kind, self.prose, self.selected);

        let walk = Walk {
            width: Size::fill(),
            height: Size::fit(),
            ..walk
        };
        // Wrapping belongs to the turtle, not the layout: it flows the runs, so
        // it is the only thing that can break between them without applying the
        // offset twice. A line breaks at a run boundary. In code that is a
        // token boundary, which is where you would break it by hand anyway.
        let flow = if self.wrap {
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

        self.text_right = self.draw_line(cx, theme, flow);

        self.sel_rect = None;
        // A caret is a selection of nothing: it paints nothing, and there is
        // nothing to offer a comment on.
        if let Some((anchor, cursor)) = self.sel.filter(|(a, c)| a != c) {
            let (start, end) = (anchor.min(cursor), anchor.max(cursor));
            for rect in self.sel_track.selection_rects(start, end) {
                self.draw_sel.draw_abs(cx, rect);
                self.sel_rect = Some(rect);
            }
        }

        for (start, end) in &self.hits {
            for rect in self.sel_track.selection_rects(*start, *end) {
                self.draw_find.draw_abs(cx, rect);
            }
        }

        // A rule under the quoted words rather than a wash over them: a comment
        // is about text that stays as readable as the rest of the line.
        for (start, end) in &self.quotes {
            for rect in self.sel_track.selection_rects(*start, *end) {
                self.draw_quote.draw_abs(
                    cx,
                    Rect {
                        pos: dvec2(rect.pos.x, rect.pos.y + rect.size.y - RULE_HEIGHT),
                        size: dvec2(rect.size.x, RULE_HEIGHT),
                    },
                );
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

#[cfg(test)]
mod tests {
    use concats_syntax::{Hl, Span};

    use super::color_runs;

    fn span(start: usize, end: usize, hl: Hl) -> Span {
        Span {
            start,
            end,
            hl: Some(hl),
        }
    }

    #[test]
    fn color_runs_fill_the_gaps_between_spans() {
        let spans = [span(2, 5, Hl::Keyword), span(8, 10, Hl::String)];
        let runs: Vec<_> = color_runs(&spans, 0, 12).collect();
        assert_eq!(
            runs,
            [
                (0, 2, None),
                (2, 5, Some(Hl::Keyword)),
                (5, 8, None),
                (8, 10, Some(Hl::String)),
                (10, 12, None),
            ]
        );
    }

    #[test]
    fn color_runs_clip_to_the_indent_the_line_and_earlier_runs() {
        // The first span starts inside the indent, the second overlaps it,
        // and the third runs past the end of the line.
        let spans = [
            span(0, 6, Hl::Comment),
            span(5, 8, Hl::Keyword),
            span(9, 40, Hl::String),
        ];
        let runs: Vec<_> = color_runs(&spans, 4, 12).collect();
        assert_eq!(
            runs,
            [
                (4, 6, Some(Hl::Comment)),
                (6, 8, Some(Hl::Keyword)),
                (8, 9, None),
                (9, 12, Some(Hl::String)),
            ]
        );
    }

    #[test]
    fn color_runs_leave_a_line_without_spans_uncoloured() {
        assert_eq!(color_runs(&[], 3, 7).collect::<Vec<_>>(), [(3, 7, None)]);
        assert_eq!(color_runs(&[], 0, 0).count(), 0);
    }
}
