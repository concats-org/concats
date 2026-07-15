//! The shadows that separate a floating surface from the content under it: the
//! tab strip casting down onto the page, and the pinned file header casting
//! both down onto the rows travelling beneath it and up into the gap above it.
//!
//! Both profiles are measured, not guessed. The design exports its tab-strip
//! shadow as a gradient bitmap whose alpha runs linearly from 0.96 at the strip
//! to 0.03 sixteen points below it, in `#21242b`. The header's is a CSS box
//! shadow, `0px 0px 8px 4px #21242b`: no Y offset, so it wraps the header and
//! its upper half mixes with the strip's. That mixing is wanted — it makes the
//! gap dark enough that rows passing through stop reading as a flicker.
//!
//! Strength is a field on the widget, copied into the shader at draw time. A
//! per-instance value written straight onto the shader (`draw_shadow +: {…}`)
//! does not reach the GPU in this makepad build (see `card_cap.rs`), but a
//! widget `#[live]` does, and the theme colours already round-trip that way.
//! `fade` is the same trick from Rust: the pinned header ramps it from 0 to 1
//! over the last few points before it pins, so the shadow arrives continuously
//! instead of snapping on.

use crate::{frame_theme, makepad_widgets::*, theme::paint};

script_mod! {
    use mod.prelude.widgets.*

    set_type_default() do #(DrawDropShadow::script_shader(vm)) {
        ..mod.draw.DrawQuad
        shadow_color: mod.app_theme.color_shadow
        strength: 0.96
        pixel: fn() {
            // Linear, like the design's own gradient. A squared falloff put
            // almost all the darkening in the first pixel or two and left the
            // rest so thin that content read straight through it, which made
            // the strip look like it sat under the page.
            let a = self.strength * (1.0 - self.pos.y)
            // Premultiplied: makepad blends src + dst*(1-a), so an unscaled
            // colour gets added over the rows instead of darkening them.
            return vec4(self.shadow_color.xyz * a, a)
        }
    }

    // The same shadow cast upward — `t` counted from the far edge instead.
    set_type_default() do #(DrawShadowUp::script_shader(vm)) {
        ..mod.draw.DrawQuad
        shadow_color: mod.app_theme.color_shadow
        strength: 0.84
        pixel: fn() {
            let a = self.strength * self.pos.y
            return vec4(self.shadow_color.xyz * a, a)
        }
    }

    // 12pt, matching the box shadow's reach (4 spread + 8 blur).
    mod.widgets.ShadowUp = #(ShadowUp::register_widget(vm)) {
        width: Fill height: 12
        strength: 0.84
        draw_shadow +: {}
    }

    mod.widgets.DropShadow = #(DropShadow::register_widget(vm)) {
        width: Fill height: 16
        strength: 0.96
        draw_shadow +: {}
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawDropShadow {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    shadow_color: Vec4f,
    #[live]
    strength: f32,
}

#[derive(Script, ScriptHook, Widget)]
pub struct DropShadow {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_shadow: DrawDropShadow,
    /// Peak opacity at the casting edge.
    #[live]
    strength: f32,
    /// Scales `strength`, so a caster can ramp its shadow in rather than
    /// snapping it on. 1.0 unless something sets it.
    #[rust(1.0)]
    pub fade: f32,
}

impl Widget for DropShadow {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(t) = frame_theme(scope) else {
            return DrawStep::done();
        };
        self.draw_shadow.shadow_color = paint(t.shadow);
        self.draw_shadow.strength = self.strength * self.fade;
        self.draw_shadow.draw_walk(cx, walk);
        DrawStep::done()
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawShadowUp {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    shadow_color: Vec4f,
    #[live]
    strength: f32,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ShadowUp {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_shadow: DrawShadowUp,
    #[live]
    strength: f32,
    #[rust(1.0)]
    pub fade: f32,
}

impl Widget for ShadowUp {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(t) = frame_theme(scope) else {
            return DrawStep::done();
        };
        self.draw_shadow.shadow_color = paint(t.shadow);
        self.draw_shadow.strength = self.strength * self.fade;
        self.draw_shadow.draw_walk(cx, walk);
        DrawStep::done()
    }
}
