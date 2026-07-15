//! The status bar's review-progress track: how much of the diff has been ticked
//! seen. The whole review in one 80x8 bar: `seen / total` over the range's
//! changed lines (the same `(blob oid, line)` keys the tick boxes write), so it
//! moves with every card you tick and survives reloads.

use crate::{frame_theme, makepad_widgets::*, theme::paint};

script_mod! {
    use mod.prelude.widgets.*

    set_type_default() do #(DrawSeenBar::script_shader(vm)) {
        ..mod.draw.DrawQuad
        progress: 0.0
        color: mod.app_theme.color_bg
        border_color: mod.app_theme.color_border
        fill_color: mod.app_theme.color_dim
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 2.0)
            sdf.fill_keep(self.color)
            sdf.stroke(self.border_color, 1.0)
            // The fill rides inside the border, and rounds like it.
            let w = floor((self.rect_size.x - 2.0) * self.progress)
            if w > 0.5 {
                sdf.box(1.0, 1.0, w, self.rect_size.y - 2.0, 1.5)
                sdf.fill(self.fill_color)
            }
            return sdf.result
        }
    }

    mod.widgets.SeenBar = #(SeenBar::register_widget(vm)) {
        width: 80 height: 8
        // The design's 84x20 hit box around an 80x8 track: 2px of air on each
        // side, the status bar's own padding supplies the rest.
        margin: Inset{left: 2 right: 2}
        draw_bar +: {}
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawSeenBar {
    #[deref]
    draw_super: DrawQuad,
    /// The share of the range's changed lines that are ticked seen, 0..1.
    #[live]
    progress: f32,
    #[live]
    color: Vec4f,
    #[live]
    border_color: Vec4f,
    #[live]
    fill_color: Vec4f,
}

#[derive(Script, ScriptHook, Widget)]
pub struct SeenBar {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bar: DrawSeenBar,

    #[rust]
    seen: usize,
    #[rust]
    total: usize,
}

impl SeenBar {
    /// Point the bar at the current tally. Nothing to review (a rename-only
    /// range) leaves it empty rather than full — nothing was reviewed.
    pub fn set_progress(&mut self, cx: &mut Cx, seen: usize, total: usize) {
        if (self.seen, self.total) == (seen, total) {
            return;
        }
        self.seen = seen;
        self.total = total;
        self.draw_bar.redraw(cx);
    }
}

impl Widget for SeenBar {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(t) = frame_theme(scope) else {
            return DrawStep::done();
        };
        self.draw_bar.color = paint(t.background);
        self.draw_bar.border_color = paint(t.border);
        self.draw_bar.fill_color = paint(t.text_muted);
        self.draw_bar.progress = if self.total == 0 {
            0.0
        } else {
            self.seen as f32 / self.total as f32
        };
        self.draw_bar.draw_walk(cx, walk);
        DrawStep::done()
    }
}
