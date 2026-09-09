//! Shared style tokens for the widget library: the reusable text styles and
//! small chrome templates the widgets and the app window compose from. They
//! register into `mod.widgets.*` (Makepad's shared-component namespace, the
//! same one the widgets themselves use), so any module reaches them via `use
//! mod.widgets.FONT` etc. Colours come from the theme (`mod.app_theme.*`, built
//! in Rust — see theme.rs); this is the template layer over those values.
//!
//! Registered first in `widgets::script_mod`, so these exist before the widgets
//! and the app layout that embed them.

#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    // The app palette: short DSL names for the theme colors (mod.app_theme,
    // built in Rust — see theme.rs). One place, `use`d by the app window and
    // every widget; a theme switch re-bakes the values via request_live_edit.
    // Shaders can't read these `let`-style tokens, so shader code takes the
    // color as a `uniform(mod.app_theme.color_*)` instead.
    mod.widgets.C_BG = mod.app_theme.color_bg          // window + content + active tab
    mod.widgets.C_CARD = mod.app_theme.color_card      // file cards, inactive chrome
    mod.widgets.C_BORDER = mod.app_theme.color_border  // every hairline and card border
    mod.widgets.C_BORDER_HOVER = mod.app_theme.color_border_hover
    mod.widgets.C_BORDER_FOCUS = mod.app_theme.color_border_focus
    mod.widgets.C_CHROME = mod.app_theme.color_chrome  // header + status bar
    mod.widgets.C_TEXT = mod.app_theme.color_text
    mod.widgets.C_DIM = mod.app_theme.color_dim
    mod.widgets.C_FAINT = mod.app_theme.color_faint    // line numbers
    mod.widgets.C_YELLOW = mod.app_theme.color_yellow
    mod.widgets.C_ACCENT = mod.app_theme.color_accent  // comment markers, selection, +
    mod.widgets.C_ON_ACCENT = mod.app_theme.color_on_accent
    mod.widgets.C_ADDED = mod.app_theme.color_added    // seen tick, add marker
    mod.widgets.C_DELETED = mod.app_theme.color_deleted // "N lines removed"
    mod.widgets.C_CHECKBOX_BG = mod.app_theme.color_checkbox_bg
    mod.widgets.C_CHECKBOX_HOVER = mod.app_theme.color_checkbox_hover
    mod.widgets.C_ELEMENT_HOVER = mod.app_theme.color_element_hover
    mod.widgets.C_DRAG = mod.app_theme.color_drag

    // JetBrains Mono everywhere — chrome and prose alike, not just code. The
    // variable font ships with makepad-widgets; bold is the wght axis, not a
    // second file. Family + size come from mod.app_font (built from the
    // settings; theme.rs), so both re-bake on request_live_edit.
    mod.widgets.FONT = TextStyle{
        font_family: FontFamily{
            first := FontMember{res: mod.app_font.first asc: 0.0 desc: 0.0}
            second := FontMember{res: mod.app_font.second asc: 0.0 desc: 0.0}
            third := FontMember{res: mod.app_font.third asc: 0.0 desc: 0.0}
            fourth := FontMember{res: mod.app_font.fourth asc: 0.0 desc: 0.0}
            mono := FontMember{res: mod.app_font.mono asc: 0.0 desc: 0.0}
            cjk := FontMember{res: mod.app_font.cjk asc: 0.0 desc: 0.0}
            emoji := FontMember{res: mod.app_font.emoji asc: 0.0 desc: 0.0}
        }
        font_size: mod.app_font.size
        line_spacing: 1.4
    }
    // Bold stays a single face: weight is a per-member property, so a bold
    // chain would be a second family. Nothing bold has wanted a fallback glyph
    // yet.
    mod.widgets.FONT_BOLD = TextStyle{
        font_family: FontFamily{
            latin := FontMember{res: mod.app_font.first asc: 0.0 desc: 0.0 weight: 700.0}
        }
        font_size: mod.app_font.size
        line_spacing: 1.4
    }

    // A TextInput recolored into the chrome: content-dark well, hairline
    // border, mono text.
    mod.widgets.DarkInput = TextInputFlat {
        height: Fit
        margin: 0
        padding: Inset{top: 2 bottom: 2 left: 6 right: 6}
        draw_bg +: {
            border_radius: 4.0
            border_size: 1.0
            color: mod.app_theme.color_bg
            color_hover: mod.app_theme.color_bg
            color_focus: mod.app_theme.color_bg
            color_down: mod.app_theme.color_bg
            color_empty: mod.app_theme.color_bg
            color_disabled: mod.app_theme.color_bg
            border_color: mod.app_theme.color_border
            border_color_hover: mod.app_theme.color_border_hover
            border_color_focus: mod.app_theme.color_border_focus
            border_color_down: mod.app_theme.color_border
            border_color_empty: mod.app_theme.color_border
            border_color_disabled: mod.app_theme.color_border
        }
        draw_text +: {
            color: mod.app_theme.color_text
        }
    }

    mod.widgets.FindBar = SolidView {
        width: Fill height: Fit
        draw_bg.color: mod.app_theme.color_bg
        flow: Down
        padding: 8
        spacing: 4
        find_input := mod.widgets.DarkInput {
            width: Fill height: 26
            is_multiline: false
            submit_on_enter: true
            empty_text: "Find"
        }
        find_count := Label {
            width: Fill height: Fit
            draw_text.color: mod.app_theme.color_faint
            draw_text.text_style: mod.widgets.FONT{font_size: 8.25}
        }
    }

    // The per-file "viewed" tick box, shared by the card header and its
    // pinned (sticky) copy.
    mod.widgets.SeenBox = CheckBox {
        width: 12 height: 12
        text: ""
        padding: 0
        margin: 0
        label_walk: Walk{width: 0 height: 0}
        draw_bg +: {
            size: 12.0
            // A 12px square with a 2px radius, like the design — 4 rounds it
            // into a circle at this size.
            border_radius: 2.0
            color: mod.app_theme.color_checkbox_bg
            color_hover: mod.app_theme.color_checkbox_hover
            color_down: mod.app_theme.color_checkbox_bg
            color_active: mod.app_theme.color_checkbox_bg
            color_focus: mod.app_theme.color_checkbox_bg
            color_disabled: mod.app_theme.color_checkbox_bg
            border_color: mod.app_theme.color_border
            border_color_hover: mod.app_theme.color_border_hover
            border_color_down: mod.app_theme.color_border
            border_color_active: mod.app_theme.color_border
            border_color_focus: mod.app_theme.color_border
            border_color_disabled: mod.app_theme.color_border
            // A blue tick, not a green one: the design's check glyph is
            // #6a77ec, the same family as the accent the seen and comment
            // markers use. Ticking a card and commenting on a line make the
            // same claim, "I have read this", so they share a hue and differ
            // only in weight.
            mark_color_active: mod.app_theme.color_accent
            mark_color_active_hover: mod.app_theme.color_accent
            mark_color_focus: #x0000
        }
    }
}
