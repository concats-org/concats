//! The gutter: the line-number rail, and the one interactive region of a row.
//! Hover shows GitHub's `+`; press starts a comment selection, drag extends it
//! row by row, release opens the composer. All of that goes out as
//! [`GutterAction`]s, which the `ReviewPane` turns into compose gestures.
//! Keeping the interaction here lets the selectable list keep text
//! drag-selection over the code (the `DiffLine`).

use concats_diff::{LineKind, Row};

// NOTE: `WidgetActionData` is not in makepad_widgets' root re-exports, only in
// the (public) `widget` module — hence the long path.
use crate::makepad_widgets::widget::WidgetActionData;
#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::{ROW_PAD, frame_theme, line_bg, makepad_widgets::*, row_marker, theme::paint};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.FONT

    // The change bar. Deletions are striped, not just red: red against green is
    // the one difference a colour-blind reviewer cannot see. So the added bar
    // is solid and the deleted one a 1px comb over a wash of the same colour —
    // texture that works whatever the hue.
    set_type_default() do #(DrawMark::script_shader(vm)) {
        ..mod.draw.DrawQuad
        pixel: fn() {
            let mut a = self.color.w
            if self.striped > 0.5 {
                if fract(self.pos.y * self.rect_size.y * 0.5) >= 0.5 {
                    a = a * 0.36
                }
            }
            return vec4(self.color.xyz * a, a)
        }
    }

    mod.widgets.Gutter = #(Gutter::register_widget(vm)) {
        width: Fit
        height: Fit
        draw_text.text_style: FONT{line_spacing: 1.55}
        draw_mark +: {}
        // The + affordance. It fills the outermost marker slot and takes that
        // slot's shape (square, full row height, 6pt wide) instead of being a
        // rounded button off to one side. Hovering a row then previews the bar
        // a comment will leave behind, and pressing turns the preview into the
        // marker in place.
        draw_plus +: {
            color: mod.app_theme.color_accent
            plus_color: instance(mod.app_theme.color_on_accent)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                sdf.fill(self.color)
                // Sized to the bar, not to a 14pt button: a 6pt-wide slot
                // cannot hold the 6.5pt arms the old square used.
                let mx = self.rect_size.x * 0.5
                let my = self.rect_size.y * 0.5
                sdf.rect(mx - 2.0, my - 0.75, 4.0, 1.5)
                sdf.fill(self.plus_color)
                sdf.rect(mx - 0.75, my - 2.0, 1.5, 4.0)
                sdf.fill(self.plus_color)
                return sdf.result
            }
        }
    }
}

/// What a row that soft wrap added is marked with, in the column the line
/// numbers use — a continuation has no number of its own, and without a mark
/// there is nothing to tell it from a line that simply starts further in.
const WRAPPED: &str = "↳";

/// The outermost slot's full width: a comment, a range being dragged, and the
/// hover affordance all fill it. Also the width of the slot the change bar gets.
const MARKER_WIDE: f64 = 6.0;
/// Merely seen — the same colour in the same slot, at a third the width.
const MARKER_THIN: f64 = 2.0;

/// The gutter's comment gestures, GitHub-style: press starts a selection on
/// that line, dragging extends it row by row, release opens the composer.
#[derive(Clone, Debug, Default)]
pub enum GutterAction {
    DragStart {
        blob: u32,
        line: u32,
    },
    /// The pointer is at this window y. A position, not a row delta: turning
    /// distance into rows here would assume every row has the same height, and
    /// that stops being true the moment a long line wraps. The list knows what
    /// it drew and where, so the list answers.
    DragTo {
        y: f64,
    },
    DragEnd,
    #[default]
    None,
}

/// The 6px bar at a row's left edge. `striped` combs it, so a deletion reads as
/// a deletion without red against green.
#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawMark {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    striped: f32,
}

#[derive(Script, ScriptHook, Widget)]
pub struct Gutter {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    /// What `set_action_data` stores, and the only way an emitted action
    /// carries it. `cx.widget_action` hard-codes `data: None`; without this
    /// field (and `widget_action_with_data` below) every gesture reached
    /// `ReviewPane` with no `ReviewItemAction::Gutter` attached and was
    /// dropped, so commenting did nothing at all. Makepad's own Button and
    /// `CheckBox` declare the same field, which is why the tick box and the fold
    /// caret worked.
    #[action_data]
    #[rust]
    action_data: WidgetActionData,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_mark: DrawMark,
    #[live]
    draw_plus: DrawColor,
    #[live]
    draw_text: DrawText,

    /// Set by the row's template: this rail belongs to a line of authored text.
    /// It keeps the marker slots and the hover `+`, and drops the line number —
    /// a transcript entry is not a file and has none.
    #[live]
    prose: bool,

    #[rust]
    kind: Option<LineKind>,
    #[rust]
    old_no: Option<u32>,
    #[rust]
    new_no: Option<u32>,
    #[rust]
    number: String,
    /// How wide `number` laid out this draw: the rail's width is built on it,
    /// and [`Gutter::paint`] right-aligns the number by it.
    #[rust]
    number_width: f64,
    #[rust]
    blob: u32,
    #[rust]
    line: u32,
    #[rust]
    marks: RowMarks,
    /// The column this rail reserved this draw, which [`Gutter::paint`] fills
    /// once the row's height is known.
    #[rust]
    rect: Rect,
    #[rust]
    hovered: bool,
    /// The last y emitted during a drag, so a move that does not actually
    /// move does not spam actions.
    #[rust]
    last_y: f64,
}

/// The review state painted beside a code row.
#[derive(Clone, Copy, Default)]
pub struct RowMarks {
    pub seen: bool,
    pub commented: bool,
    pub selected: bool,
}

impl Gutter {
    pub fn set_row(&mut self, row: &Row, marks: RowMarks) {
        let Row::Code {
            kind,
            old_no,
            new_no,
            blob,
            line,
        } = *row
        else {
            return;
        };
        let number_changed =
            self.kind != Some(kind) || self.old_no != old_no || self.new_no != new_no;
        self.kind = Some(kind);
        self.old_no = old_no;
        self.new_no = new_no;
        if number_changed {
            let no = match kind {
                LineKind::Del => old_no,
                _ => new_no.or(old_no),
            };
            self.number.clear();
            match no {
                // The rail is drawn from this text, so a prose line still gets
                // a space: no column of numbers, and the same row height.
                _ if self.prose => self.number.push(' '),
                Some(no) => {
                    use std::fmt::Write;
                    let _ = write!(self.number, "{no:>5}");
                }
                None => self.number.push_str("     "),
            }
        }
        self.blob = blob;
        self.line = line;
        self.marks = marks;
    }

    /// 6 (comment slot) + 6 (change slot) + 12 of air before the numbers, like
    /// the design. A prose line keeps the slots and skips the air its numbers
    /// would need.
    fn lead(&self) -> f64 {
        if self.prose { 14.0 } else { 24.0 }
    }

    /// The air between the number column and the line.
    fn pad(&self) -> f64 {
        if self.prose { 4.0 } else { 10.0 }
    }

    /// Paint the rail, now that the line beside it has been laid out: the row
    /// is as tall as it is going to be, and the rows soft wrap added are known.
    ///
    /// Everything here waits for that. A soft-wrapped line is several rows of
    /// glyphs and decides the height of the row it sits in, while the rail is
    /// drawn first and one line tall — so the tint, the markers and the `+`,
    /// which all describe the whole line, have nothing to size themselves by
    /// until the line is done. Painting the rail from here also gives the
    /// hover the whole row to answer on.
    ///
    /// `wrapped` are the tops of the rows soft wrap added, in window
    /// coordinates, from the line that drew them.
    pub fn paint(&mut self, cx: &mut Cx2d, scope: &mut Scope, height: f64, wrapped: &[f64]) {
        let kind = self.kind.unwrap_or(LineKind::Context);
        let Some(theme) = frame_theme(scope) else {
            return;
        };
        self.draw_bg.color = line_bg(theme, kind, self.prose, self.marks.selected);
        let r = Rect {
            pos: self.rect.pos,
            size: dvec2(self.rect.size.x, height),
        };
        self.draw_bg.draw_abs(cx, r);

        // The number, and a `↳` on each row soft wrap added — a continuation
        // has no number of its own, and without the mark there is nothing to
        // tell it from a line that simply starts further in. Both are
        // right-aligned in the same column, so it reads as one.
        self.draw_text.color = paint(theme.text_faint);
        let right = r.pos.x + r.size.x - self.pad();
        self.draw_text.draw_abs(
            cx,
            dvec2(right - self.number_width, r.pos.y + ROW_PAD),
            &self.number,
        );
        let mark = self
            .draw_text
            .layout(cx, 0.0, 0.0, None, false, Align::default(), WRAPPED);
        let width = f64::from(mark.size_in_lpxs.width * self.draw_text.font_scale);
        for top in wrapped {
            self.draw_text
                .draw_abs(cx, dvec2(right - width, *top + ROW_PAD), WRAPPED);
        }

        // Two 6px marker slots at the card's left edge, like the design: the
        // outer one is the comment/selection bar (the comment strip below the
        // range continues it), the inner one the change bar. Separate slots
        // mean commenting a changed line never displaces its green. Seen and
        // commented share the outer slot and the accent colour and differ only
        // in width: 6pt for a comment, 2pt for merely seen. Same colour on
        // purpose — commenting on a line implies having seen it — so the width
        // tells the two apart. A comment takes the slot outright rather than
        // drawing both.
        let marker = if self.marks.commented || self.marks.selected {
            Some(MARKER_WIDE)
        } else if self.marks.seen {
            Some(MARKER_THIN)
        } else {
            None
        };
        if let Some(width) = marker {
            self.draw_mark.color = paint(theme.accent);
            self.draw_mark.striped = 0.0;
            self.draw_mark.draw_abs(
                cx,
                Rect {
                    pos: r.pos,
                    size: dvec2(width, r.size.y),
                },
            );
        }
        if let Some(color) = row_marker(theme, kind) {
            self.draw_mark.color = color;
            self.draw_mark.striped = f32::from(kind == LineKind::Del);
            self.draw_mark.draw_abs(
                cx,
                Rect {
                    pos: dvec2(r.pos.x + MARKER_WIDE, r.pos.y),
                    size: dvec2(MARKER_WIDE, r.size.y),
                },
            );
        }
        // The + affordance, in the outermost slot the marker itself uses, so
        // the hover, the drag and the posted comment all mark the same column.
        // Drawn last: while the pointer is on a row it stands in for whatever
        // marker that row already carries.
        if self.hovered {
            self.draw_plus.draw_abs(
                cx,
                Rect {
                    pos: r.pos,
                    size: dvec2(MARKER_WIDE, r.size.y),
                },
            );
        }
    }
}

impl Widget for Gutter {
    /// Only this rail reports interactive, so the list hands it the clicks and
    /// keeps text selection for everything to its right.
    fn is_interactive(&self) -> bool {
        true
    }

    #[expect(
        clippy::float_cmp,
        reason = "Exact equality detects an unchanged pointer position, not numerical proximity."
    )]
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(_) => {
                self.hovered = true;
                cx.set_cursor(MouseCursor::Hand);
                self.draw_bg.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.hovered = false;
                self.draw_bg.redraw(cx);
            }
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                self.last_y = fe.abs.y;
                cx.widget_action_with_data(
                    &self.action_data,
                    self.uid,
                    GutterAction::DragStart {
                        blob: self.blob,
                        line: self.line,
                    },
                );
            }
            Hit::FingerMove(fe) => {
                // The press captured the pointer, so every move lands here, on
                // the row the drag started on, whatever it is over now. Only
                // the position is known here; which row it names is for the
                // list to say.
                if fe.abs.y != self.last_y {
                    self.last_y = fe.abs.y;
                    cx.widget_action_with_data(
                        &self.action_data,
                        self.uid,
                        GutterAction::DragTo { y: fe.abs.y },
                    );
                }
            }
            Hit::FingerUp(fe) if fe.is_primary_hit() => {
                cx.widget_action_with_data(&self.action_data, self.uid, GutterAction::DragEnd);
            }
            _ => {}
        }
    }

    /// Reserve the rail's column and remember it. The painting happens in
    /// [`Gutter::paint`], once the line beside it has decided the row's height.
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let number =
            self.draw_text
                .layout(cx, 0.0, 0.0, None, false, Align::default(), &self.number);
        self.number_width = f64::from(number.size_in_lpxs.width * self.draw_text.font_scale);
        // The two marker slots, the air after them and the number's column
        // wide; one line of the rail tall, until the line beside it wraps and
        // makes the row taller.
        self.rect = cx.walk_turtle(Walk {
            width: Size::Fixed(self.lead() + self.number_width + self.pad()),
            height: Size::Fixed(
                f64::from(number.size_in_lpxs.height * self.draw_text.font_scale) + ROW_PAD * 2.0,
            ),
            ..walk
        });
        DrawStep::done()
    }
}
