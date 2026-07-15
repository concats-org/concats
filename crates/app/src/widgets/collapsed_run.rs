//! The collapsed-run indicator: what a stretch of skipped unchanged lines
//! leaves behind in a file card, and the control that reveals it.
//!
//! The design (`07-collapsed-run-seen`, the `Line` variants at `735:2802` /
//! `735:2258` / `735:2814`) puts the expander on each edge of a cut rather than
//! one control on the run: a 16pt band with a 12pt lucide arrow that points
//! into the hidden lines and grows the code it sits against. A cut in the
//! middle of a file shows two of them, stacked to 32pt — a down arrow closing
//! the block above, an up arrow opening the block below. A cut at the card's
//! head or tail shows only the one band that has code to grow.
//!
//! The band also explains where a comment selection stops: a drag walks code
//! rows, and the lines here are not rows until they are revealed.

use concats_diff::CollapsedEnd;

use crate::{
    frame_theme,
    makepad_widgets::{widget::WidgetActionData, *},
    mix4,
    theme::paint,
};

script_mod! {
    use mod.prelude.widgets.*

    set_type_default() do #(DrawSkip::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: mod.app_theme.color_card
        hover_color: mod.app_theme.color_element_hover
        rule_color: mod.app_theme.color_faint
        rule_y: 0.0
        hover_y: 0.0
        hover_on: 0.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.clear(self.color)
            // The band under the pointer, lit. The click target is the band's
            // whole width, far easier to hit than the 12pt arrow, so it has to
            // show which band it is somewhere the pointer actually is. Alpha
            // carries the flag; an unhovered band fills with nothing.
            sdf.rect(0.0, self.hover_y, self.rect_size.x, 16.0)
            sdf.fill(vec4(self.hover_color.xyz, self.hover_on))
            // The seam, at the point where the file's continuity breaks. The
            // design draws it at 4% opacity: the arrow says the code is cut,
            // not this line.
            sdf.rect(0.0, self.rule_y, self.rect_size.x, 1.0)
            sdf.fill(self.rule_color)
            return sdf.result
        }
    }

    mod.widgets.CollapsedRun = #(CollapsedRun::register_widget(vm)) {
        width: Fill height: 16
        draw_bg +: {}
        draw_down.svg: crate_resource("self:resources/icons/arrow_down.svg")
        draw_up.svg: crate_resource("self:resources/icons/arrow_up.svg")
    }
}

/// One expander band, and the vertical rhythm of the whole indicator: 2pt of
/// padding around a 12pt icon, like the design's `py-[--radius-xs]`.
const BAND: f64 = 16.0;
/// The arrow's path box — not its icon box, and not its inked extent. `DrawSvg`
/// scales an svg by the extent of its geometry rather than by its viewBox, and
/// that extent excludes the stroke: ask for the design's 12pt icon box and the
/// 7pt glyph inside it gets blown up to 12. At 7 the stroke stays 1pt and the
/// ink lands on the design's 8pt square, 8 in from the card's left edge and 4
/// down from the band's top, which is the `lucide/arrow-*` box inset by its
/// 20.83%.
const ICON: f64 = 7.0;
const ICON_X: f64 = 8.5;
const ICON_Y: f64 = 4.5;

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawSkip {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    hover_color: Vec4f,
    #[live]
    rule_color: Vec4f,
    #[live]
    rule_y: f32,
    #[live]
    hover_y: f32,
    #[live]
    hover_on: f32,
}

#[derive(Script, ScriptHook, Widget)]
pub struct CollapsedRun {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    /// Carries the `ReviewItemAction::Expand` the list stamps on this row —
    /// without it `cx.widget_action` would emit `data: None` and the pane would
    /// have no way to tell which run was clicked. See `Gutter`.
    #[action_data]
    #[rust]
    action_data: WidgetActionData,
    #[redraw]
    #[live]
    draw_bg: DrawSkip,
    #[live]
    draw_down: DrawSvg,
    #[live]
    draw_up: DrawSvg,
    /// Which ends of the run have code to grow, and so get a band. A run at the
    /// card's head has no code above it; one at its tail none below.
    #[rust(true)]
    head: bool,
    #[rust(true)]
    tail: bool,
    /// Which band the pointer is over, if any.
    #[rust]
    hover: Option<CollapsedEnd>,
}

impl CollapsedRun {
    /// Tell the band which ends of its run are reachable: `head` when there is
    /// code above (its first hidden lines can join it), `tail` when there is code
    /// below. (Called by `ReviewList` as it recycles list items.)
    pub fn set_ends(&mut self, head: bool, tail: bool) {
        self.head = head;
        self.tail = tail;
    }

    /// The band at `y` within the indicator, top first. Which end that names
    /// depends on how many bands there are: the down arrow reveals the run's
    /// head (it grows the code above), the up arrow its tail.
    fn end_at(&self, rect: Rect, y: f64) -> Option<CollapsedEnd> {
        match (self.head, self.tail) {
            // Two bands: the down arrow on top, the up arrow beneath it.
            (true, true) if y >= rect.pos.y + BAND => Some(CollapsedEnd::Tail),
            (true, _) => Some(CollapsedEnd::Head),
            (false, true) => Some(CollapsedEnd::Tail),
            (false, false) => None,
        }
    }

    /// Top of a band within the indicator: the down arrow is always first.
    fn band_y(&self, end: CollapsedEnd) -> f64 {
        match end {
            CollapsedEnd::Head => 0.0,
            CollapsedEnd::Tail if self.head => BAND,
            CollapsedEnd::Tail => 0.0,
        }
    }
}

impl Widget for CollapsedRun {
    /// Like the `Gutter`: the band claims clicks from the selectable list, which
    /// costs no text selection because there is no text in it.
    fn is_interactive(&self) -> bool {
        true
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                cx.set_cursor(MouseCursor::Hand);
                let over = self.end_at(fe.rect, fe.abs.y);
                if self.hover != over {
                    self.hover = over;
                    self.draw_bg.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                self.hover = None;
                self.draw_bg.redraw(cx);
            }
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                if let Some(end) = self.end_at(fe.rect, fe.abs.y) {
                    cx.widget_action_with_data(&self.action_data, self.uid, end);
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(t) = frame_theme(scope) else {
            return DrawStep::done();
        };
        let bands = f64::from(u8::from(self.head) + u8::from(self.tail));
        // `rgba(0,0,0,0.04)` over the card, like the design's variant.
        self.draw_bg.color = mix4(paint(t.surface), vec4(0.0, 0.0, 0.0, 1.0), 0.04);
        self.draw_bg.hover_color = paint(t.element_hover);
        self.draw_bg.rule_color = Vec4f {
            w: 0.04,
            ..paint(t.text_faint)
        };
        // The seam sits where the file actually breaks: between the two bands of
        // a mid-file cut, under the band of a cut at the card's head, and over
        // the band of one at its tail.
        self.draw_bg.rule_y = if self.head && !self.tail {
            0.0
        } else {
            (BAND - 1.0) as f32
        };
        self.draw_bg.hover_y = self.hover.map(|end| self.band_y(end)).unwrap_or(0.0) as f32;
        self.draw_bg.hover_on = f32::from(self.hover.is_some());
        self.draw_bg.draw_walk(
            cx,
            Walk {
                height: Size::Fixed(BAND * bands),
                ..walk
            },
        );

        let r = self.draw_bg.area().rect(cx);
        let (faint, lit) = (paint(t.text_faint), paint(t.text));
        let mut arrow = |icon: &mut DrawSvg, y: f64, hovered: bool| {
            icon.color = if hovered { lit } else { faint };
            icon.draw_abs(
                cx,
                Rect {
                    pos: dvec2(r.pos.x + ICON_X, r.pos.y + y + ICON_Y),
                    size: dvec2(ICON, ICON),
                },
            );
        };
        let up_y = self.band_y(CollapsedEnd::Tail);
        if self.head {
            arrow(
                &mut self.draw_down,
                0.0,
                self.hover == Some(CollapsedEnd::Head),
            );
        }
        if self.tail {
            arrow(
                &mut self.draw_up,
                up_y,
                self.hover == Some(CollapsedEnd::Tail),
            );
        }
        DrawStep::done()
    }
}
