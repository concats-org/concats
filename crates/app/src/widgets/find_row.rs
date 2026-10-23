//! Find, inline in a tab bar: the query with its match toggles, previous and
//! next, the tally, a replacement where the view can replace, and close. The
//! card views, the file browser and the terminal each host one, and read and
//! show its toggles through the functions here.

#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::{
    find::{Find, Options},
    makepad_widgets::*,
};

script_mod! {
    use mod.prelude.widgets.*

    // A match toggle while it is off: a bar icon, small enough to sit in the
    // field.
    mod.widgets.FindToggle = mod.widgets.BarButton {
        width: 16 height: 16
        icon_walk: Walk{width: 11 height: Fit}
    }

    // The same toggle while it is on, in the accent "Show seen" uses: an on
    // control is a second button that takes the first one's place.
    mod.widgets.FindToggleOn = mod.widgets.FindToggle {
        visible: false
        draw_icon +: {
            color: mod.app_theme.color_accent
            color_hover: mod.app_theme.color_accent
        }
    }

    mod.widgets.FindRow = View {
        width: Fit height: Fit
        flow: Right
        align: Align{y: 0.5}
        spacing: 4
        // The query and its toggles share one field. The box draws the
        // field and the input only its hover and focus border. The toggles
        // come last: a view offers events to its last child first, so a
        // click on one is theirs, not the input's.
        find_field := RoundedView {
            width: 220 height: Fit
            flow: Overlay
            align: Align{x: 1.0 y: 0.5}
            draw_bg +: {
                color: mod.app_theme.color_bg
                border_size: 1.0
                border_radius: 4.0
                border_color: mod.app_theme.color_border
            }
            find_input := mod.widgets.DarkInput {
                width: Fill
                is_multiline: false
                submit_on_enter: true
                empty_text: "Find"
                padding: Inset{top: 2 bottom: 2 left: 6 right: 54}
                draw_bg +: {
                    color: #0000
                    color_hover: #0000
                    color_focus: #0000
                    color_down: #0000
                    color_empty: #0000
                    color_disabled: #0000
                    border_color: #0000
                    border_color_down: #0000
                    border_color_empty: #0000
                    border_color_disabled: #0000
                }
            }
            View {
                width: Fit height: Fit
                margin: Inset{right: 2}
                find_case := mod.widgets.FindToggle {
                    draw_icon +: { svg: crate_resource("self:resources/icons/case_sensitive.svg") }
                }
                find_case_on := mod.widgets.FindToggleOn {
                    draw_icon +: { svg: crate_resource("self:resources/icons/case_sensitive.svg") }
                }
                find_word := mod.widgets.FindToggle {
                    draw_icon +: { svg: crate_resource("self:resources/icons/whole_word.svg") }
                }
                find_word_on := mod.widgets.FindToggleOn {
                    draw_icon +: { svg: crate_resource("self:resources/icons/whole_word.svg") }
                }
                find_regex := mod.widgets.FindToggle {
                    draw_icon +: { svg: crate_resource("self:resources/icons/regex.svg") }
                }
                find_regex_on := mod.widgets.FindToggleOn {
                    draw_icon +: { svg: crate_resource("self:resources/icons/regex.svg") }
                }
            }
        }
        find_prev := mod.widgets.BarButton {
            icon_walk: Walk{width: 6 height: Fit}
            draw_icon.svg: crate_resource("self:resources/icons/chevron_left.svg")
        }
        find_next := mod.widgets.BarButton {
            icon_walk: Walk{width: 6 height: Fit}
            draw_icon.svg: crate_resource("self:resources/icons/chevron_right.svg")
        }
        find_count := Label {
            width: Fit height: Fit
            margin: Inset{left: 2 right: 6}
            draw_text.color: mod.app_theme.color_faint
            draw_text.text_style: mod.widgets.FONT{font_size: 8.25}
        }
        // In the tally's place while a regex query does not parse.
        find_invalid := Label {
            visible: false
            width: Fit height: Fit
            margin: Inset{left: 2 right: 6}
            text: "Invalid regex"
            draw_text.color: mod.app_theme.color_deleted
            draw_text.text_style: mod.widgets.FONT{font_size: 8.25}
        }
        // A text input ignores `visible`, so the box around it is what hides.
        replace_box := View {
            visible: false
            width: Fit height: Fit
            replace_input := mod.widgets.DarkInput {
                width: 180
                is_multiline: false
                submit_on_enter: true
                empty_text: "Replace"
            }
        }
        find_close := mod.widgets.BarButton {
            icon_walk: Walk{width: 9 height: Fit}
            draw_icon.svg: crate_resource("self:resources/icons/x.svg")
        }
    }
}

/// `find` as the bar's `actions` leave it, when they retyped the query or
/// flipped a toggle. A flip hands the keyboard back to the query.
pub fn changed(bar: &View, cx: &mut Cx, actions: &Actions, find: &Find) -> Option<Find> {
    let flipped = |off: &[LiveId], on: &[LiveId]| {
        bar.button(cx, off).clicked(actions) || bar.button(cx, on).clicked(actions)
    };
    let was = find.options();
    let options = Options {
        case: was.case != flipped(ids!(find_case), ids!(find_case_on)),
        word: was.word != flipped(ids!(find_word), ids!(find_word_on)),
        regex: was.regex != flipped(ids!(find_regex), ids!(find_regex_on)),
    };
    let input = bar.text_input(cx, ids!(find_input));
    if options != was {
        input.set_key_focus(cx);
    }
    let query = input.changed(actions);
    (query.is_some() || options != was)
        .then(|| Find::new(query.unwrap_or_else(|| find.query().to_owned()), options))
}

/// Show each toggle as `options` sets it, and, while the query cannot be
/// searched for, say so where the tally was.
pub fn show(bar: &View, cx: &mut Cx, options: Options, invalid: bool) {
    for (off, on, set) in [
        (ids!(find_case), ids!(find_case_on), options.case),
        (ids!(find_word), ids!(find_word_on), options.word),
        (ids!(find_regex), ids!(find_regex_on), options.regex),
    ] {
        bar.button(cx, off).set_visible(cx, !set);
        bar.button(cx, on).set_visible(cx, set);
    }
    bar.label(cx, ids!(find_count)).set_visible(cx, !invalid);
    bar.label(cx, ids!(find_invalid)).set_visible(cx, invalid);
}
