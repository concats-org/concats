//! The gutter: the line-number rail, and the one interactive region of a row.
//! Hover shows GitHub's `+`; press starts a comment selection, drag extends it
//! row by row, release opens the composer. All of that goes out as
//! [`GutterAction`]s, which the `ReviewPane` turns into compose gestures.
//! Keeping the interaction here lets the selectable list keep text
//! drag-selection over the code (the `DiffLine`).

use concats_diff::LineKind;

// NOTE: `WidgetActionData` is not in makepad_widgets' root re-exports, only in
// the (public) `widget` module — hence the long path.
use crate::makepad_widgets::widget::WidgetActionData;
use crate::{
    frame_theme, makepad_widgets::*, row_bg, row_marker, row_selected_bg, theme::paint, ROW_PAD,
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.FONT

    // The change bar. Deletions are striped, not just red: red against green is
    // the one difference a colour-blind reviewer cannot see. So the added bar
    // is solid and the deleted one a 1px comb over a wash of the same colour —
    // texture that works whatever the hue.
    set_type_default() do #(DrawMark::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: mod.app_theme.color_added
        striped: 0.0
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
    /// CheckBox declare the same field, which is why the tick box and the fold
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

    #[rust]
    kind: Option<LineKind>,
    #[rust]
    old_no: Option<u32>,
    #[rust]
    new_no: Option<u32>,
    #[rust]
    number: String,
    #[rust]
    blob: u32,
    #[rust]
    line: u32,
    #[rust]
    seen: bool,
    /// Whether this line lies inside a stored comment's range — draws the
    /// blue marker the design puts at the card edge, left of the change bar.
    #[rust]
    commented: bool,
    /// Whether this line lies inside the range being composed right now —
    /// blue marker plus a blue-tinted row while selecting.
    #[rust]
    selected: bool,
    #[rust]
    hovered: bool,
    /// The last y emitted during a drag, so a move that does not actually
    /// move does not spam actions.
    #[rust]
    last_y: f64,
}

impl Gutter {
    /// Point this rail at one diff line — its numbers, and its seen/commented/
    /// selected state. (Called by `ReviewList` as it recycles list items.)
    #[allow(clippy::too_many_arguments)]
    pub fn set_row(
        &mut self,
        kind: LineKind,
        old_no: Option<u32>,
        new_no: Option<u32>,
        blob: u32,
        line: u32,
        seen: bool,
        commented: bool,
        selected: bool,
    ) {
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
            if let Some(no) = no {
                use std::fmt::Write;
                let _ = write!(self.number, "{no:>5}");
            } else {
                self.number.push_str("     ");
            }
        }
        self.blob = blob;
        self.line = line;
        self.seen = seen;
        self.commented = commented;
        self.selected = selected;
    }
}

impl Widget for Gutter {
    /// Only this rail reports interactive, so the list hands it the clicks and
    /// keeps text selection for everything to its right.
    fn is_interactive(&self) -> bool {
        true
    }

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
        self.draw_bg.begin(
            cx,
            walk,
            // 6 (comment slot) + 6 (change slot) + 12 of air before the
            // numbers, like the design.
            Layout::flow_right().with_padding(Inset {
                left: 24.0,
                right: 10.0,
                top: ROW_PAD,
                bottom: ROW_PAD,
            }),
        );
        self.draw_text.color = paint(theme.text_faint);
        // One number column, like the design: the new line number, or the
        // old one for deletions.
        self.draw_text
            .draw_walk(cx, Walk::fit(), Align::default(), &self.number);
        self.draw_bg.end(cx);

        // Two 6px marker slots at the card's left edge, like the design: the
        // outer one is the comment/selection bar (the comment strip below the
        // range continues it), the inner one the change bar. Separate slots
        // mean commenting a changed line never displaces its green. Seen and
        // commented share the outer slot and the accent colour and differ only
        // in width: 6pt for a comment, 2pt for merely seen. Same colour on
        // purpose — commenting on a line implies having seen it — so the width
        // tells the two apart. A comment takes the slot outright rather than
        // drawing both.
        let r = self.draw_bg.area().rect(cx);
        let marker = if self.commented || self.selected {
            Some(MARKER_WIDE)
        } else if self.seen {
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
        DrawStep::done()
    }
}
