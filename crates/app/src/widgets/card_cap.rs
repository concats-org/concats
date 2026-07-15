//! The rounded end of a file card: a 6px strip drawn over the card's top or
//! bottom edge, painting the corner cut-outs in the page colour and the
//! rounded border+fill inside them.
//!
//! Why a widget and not a rounded `View` background: a `show_bg` view with an
//! SDF shader paints only about half its height in this makepad build (the
//! same defect that used to leave a band of window background between a file
//! header and its first code row), while a widget drawing through `draw_walk`
//! is correct. The strip is an overlay, so it costs the card no layout height.
//!
//! Why two shaders rather than one with a direction flag: only a shader's own
//! type default reaches the GPU here. A per-widget (`draw_cap +: {…}`) or
//! per-instance override leaves the default in place, and the Rust field still
//! reads back as set — so the mismatch is invisible from the CPU. The flag was
//! silently ignored, drawing every card's bottom as a second top cap: square
//! corners, and the card's fill hanging below its own border as a light lip
//! that read as a drop shadow.

use crate::{frame_theme, makepad_widgets::*, theme::paint};

script_mod! {
    use mod.prelude.widgets.*

    // Each box runs past the far edge of the strip, so only the near end is
    // rounded and the end that joins the card stays square. The fill is inset
    // by the border at that near end, and only there.
    set_type_default() do #(DrawCardCapTop::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: mod.app_theme.color_card
        border_color: mod.app_theme.color_border
        page_color: mod.app_theme.color_bg
        // `--radius-xs` in the design (Code node, 703:943) — the same token the
        // tick box uses, and the one radius the whole UI shares. The pinned
        // header carries it the whole way up, so it never changes shape.
        radius: 2.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.clear(self.page_color)
            // Twice the strip's height, so its half-height boundary sits at the
            // far edge and every row of the strip is evaluated against the
            // rounded end. `box_y` picks its radius with `step(h/2, y)`, and
            // the half using radius 0 reports distance 0 for every point inside
            // it, so `fill` draws nothing there and the `clear` shows through
            // as a page-coloured line across the card. At radius 6 the old `h =
            // rect + radius` hid this; at 2 the boundary moved into view.
            sdf.box_y(0.0, 0.0, self.rect_size.x, self.rect_size.y * 2.0, self.radius, 0.0)
            sdf.fill(self.border_color)
            sdf.box_y(
                1.0, 1.0,
                self.rect_size.x - 2.0, self.rect_size.y * 2.0,
                self.radius - 1.0, 0.0
            )
            sdf.fill(self.color)
            return sdf.result
        }
    }

    set_type_default() do #(DrawCardCapBottom::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: mod.app_theme.color_card
        border_color: mod.app_theme.color_border
        page_color: mod.app_theme.color_bg
        radius: 2.0
        // NOTE: the commas matter. The DSL also separates arguments by newline,
        // so a line opening with `-` continues the previous one: `0.0` /
        // `-self.rect_size.y` parsed as the single expression `0.0 -
        // self.rect_size.y`, leaving box_y five arguments. That is a shader
        // compile error, and a shader that fails to compile draws nothing at
        // all: every card's bottom corners stayed square while the top ones
        // rounded, with no sign of it from the CPU side.
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.clear(self.page_color)
            sdf.box_y(
                0.0, 0.0 - self.rect_size.y,
                self.rect_size.x, self.rect_size.y * 2.0,
                0.0, self.radius
            )
            sdf.fill(self.border_color)
            sdf.box_y(
                1.0, 0.0 - self.rect_size.y - 1.0,
                self.rect_size.x - 2.0, self.rect_size.y * 2.0,
                0.0, self.radius - 1.0
            )
            sdf.fill(self.color)
            return sdf.result
        }
    }

    mod.widgets.CardCap = #(CardCapTop::register_widget(vm)) {
        width: Fill height: 6
        draw_cap +: {}
    }

    mod.widgets.CardCapBottom = #(CardCapBottom::register_widget(vm)) {
        width: Fill height: 6
        draw_cap +: {}
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawCardCapTop {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    border_color: Vec4f,
    #[live]
    page_color: Vec4f,
    #[live]
    radius: f32,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawCardCapBottom {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    border_color: Vec4f,
    #[live]
    page_color: Vec4f,
    #[live]
    radius: f32,
}

#[derive(Script, ScriptHook, Widget)]
pub struct CardCapTop {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_cap: DrawCardCapTop,
}

#[derive(Script, ScriptHook, Widget)]
pub struct CardCapBottom {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_cap: DrawCardCapBottom,
}

impl Widget for CardCapTop {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(t) = frame_theme(scope) else {
            return DrawStep::done();
        };
        self.draw_cap.color = paint(t.surface);
        self.draw_cap.border_color = paint(t.border);
        self.draw_cap.page_color = paint(t.background);
        self.draw_cap.draw_walk(cx, walk);
        DrawStep::done()
    }
}

impl Widget for CardCapBottom {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(t) = frame_theme(scope) else {
            return DrawStep::done();
        };
        self.draw_cap.color = paint(t.surface);
        self.draw_cap.border_color = paint(t.border);
        self.draw_cap.page_color = paint(t.background);
        self.draw_cap.draw_walk(cx, walk);
        DrawStep::done()
    }
}
