//! The review pane: one whole review — the caption/toolbar header, the diff
//! picker, the dock (Guide / Sessions / Commits / File Diff tabs in the center,
//! and a left, right and bottom panel) and the status bar — bound to one
//! document. It owns
//! its own chrome actions and dock/gesture routing, so the App drives it
//! through a few public entry points (layout restore, the status-bar buttons,
//! the diff-picker hooks), and a second pane could sit next to it without the
//! App arbitrating widget lookups. It composes the `ReviewList` and terminal
//! widgets and turns the `Gutter`'s drag actions into the inline comment
//! composer.

mod pickers;

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use concats_diff::{CollapsedEnd, LineKind, Loaded, Row, Side, load::Rev};
use concats_review::{comments, interchange, store};
use concats_state::Target;
use makepad_service::Shared;
use pickers::{RangeEnd, RangeRow};

use super::{
    FileBrowserAction, GutterAction, ReviewList, SeenBar,
    review_list::{ReviewItemAction, ReviewListAction},
};
#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::{
    FrameData, WindowScope,
    dock::{
        TabSpec, area_bar, browser_tab_spec, drag_source_tab_id, file_tab_id, holds, model_tab_of,
        open_tab, settings_tab_spec, shown_tabs, stream_area, stream_tab_spec, stream_tabs_in,
        terminal_tab_spec, terminal_tabs,
    },
    file_view::{caret_to_line, caret_to_thread, open_file, read_file_sides},
    keymap::Command,
    load::{hold_minted, resplice_comments, spawn_load},
    makepad_widgets::*,
    review_doc::{
        Compose, Composing, ReviewDoc, Stream, blob_label, card_keys, code_row_near,
        comment_anchor, derive_compose, expand_collapsed, quoted, reveal_removed, seen_progress,
        selected, splice_composer, stream_has_composer, strip_composer, widen_compose,
    },
    service::{self, ReviewCmd, review, review_state},
    terminal,
    terminal_view::TerminalViewAction,
    window::WindowState,
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.PerfGraph
    use mod.widgets.TerminalView
    use mod.widgets.FileBrowser
    use mod.widgets.ReviewList
    use mod.widgets.SeenBar
    use mod.widgets.HeaderRule
    use mod.widgets.FONT
    use mod.widgets.DarkInput
    use mod.widgets.BarButton
    use mod.widgets.C_CARD
    use mod.widgets.C_BORDER
    use mod.widgets.C_CHROME
    use mod.widgets.C_TEXT
    use mod.widgets.C_DIM
    use mod.widgets.C_FAINT
    use mod.widgets.C_ELEMENT_HOVER
    use mod.widgets.C_DRAG

    // One tab of the dock's strip: icon + label, right hairline, bottom
    // hairline that disappears when active (the active tab fuses with the
    // content below) — the old hand-drawn TabButton look, ported onto the
    // dock's Tab so tabs drag, split, and merge for free.
    let ReviewTab = TabFlat {
        closeable: true
        height: Fill
        margin: 0
        padding: Inset{left: 15 right: 16 top: 0 bottom: 1}
        spacing: 5
        icon_walk: Walk{width: 11 height: 11}
        // The quiet ✕ (drawn left of the icon — where the Tab widget puts
        // it); closed views come back via the status-bar buttons.
        close_button +: {
            width: 11 height: 11
            margin: Inset{left: 0 right: 4 top: 0 bottom: 0}
            draw_button +: {
                color: C_FAINT
                color_hover: C_TEXT
                color_active: C_DIM
            }
        }
        draw_text +: {
            text_style: FONT{font_size: 9}
            color: C_DIM
            color_hover: C_DIM
            color_active: C_TEXT
        }
        draw_bg +: {
            // Colors arrive as uniforms from mod.app_theme — module `let`s don't
            // reach shader fns; a theme switch re-bakes them via request_live_edit.
            color_card: uniform(mod.app_theme.color_card)
            color_bg: uniform(mod.app_theme.color_bg)
            color_border: uniform(mod.app_theme.color_border)
            pixel: fn() {
                let bg = mix(self.color_card, self.color_bg, self.active)
                // Divider between tabs only. No bottom edge: the strip hands
                // over to the content's fade, and a hairline across it read as
                // a seam. Active still reads by its fill and its brighter text.
                if self.pos.x > 1.0 - 1.0 / self.rect_size.x {
                    return self.color_border
                }
                return bg
            }
        }
        // The icon tint follows the label: dim at rest, text colour when
        // active. The stock states are restated in full: a state merge replaces
        // the whole `apply`, so adding draw_icon means repeating the rest.
        animator +: {
            active: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward{duration: 0.3}}
                    apply: {
                        close_button: {draw_button: {active: 0.0}}
                        draw_bg: {active: 0.0}
                        draw_text: {active: 0.0}
                        draw_icon: {color: C_DIM}
                    }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: {
                        close_button: {draw_button: {active: 1.0}}
                        draw_bg: {active: 1.0}
                        draw_text: {active: 1.0}
                        draw_icon: {color: C_TEXT}
                    }
                }
            }
        }
    }
    // The `_fill` icon variants exist because the Tab widget renders icons
    // with DrawSvgGlyph, which draws filled paths only — the stroke-style
    // originals (used everywhere else) would vanish. They are generated from
    // the originals by fattening each stroke into filled geometry.
    let GuideTab = ReviewTab {
        draw_icon +: { color: C_DIM svg: crate_resource("self:resources/icons/guide_fill.svg") }
    }
    let SessionsTab = ReviewTab {
        draw_icon +: { color: C_DIM svg: crate_resource("self:resources/icons/sessions_fill.svg") }
    }
    let CommitsTab = ReviewTab {
        draw_icon +: { color: C_DIM svg: crate_resource("self:resources/icons/commits_fill.svg") }
    }
    let FilesTab = ReviewTab {
        draw_icon +: { color: C_DIM svg: crate_resource("self:resources/icons/file_diff_fill.svg") }
    }
    let CommentsTab = ReviewTab {
        draw_icon +: { color: C_DIM svg: crate_resource("self:resources/icons/comment_fill.svg") }
    }
    // One file's whole content, opened from the browser. Its name is set per
    // file, so it carries no fixed label here.
    let FileTab = ReviewTab {
        draw_icon +: { color: C_DIM svg: crate_resource("self:resources/icons/file_fill.svg") }
    }
    // The settings tab; the `{ }` status-bar button opens it beside the
    // streams (no dedicated icon yet).
    let SettingsTab = ReviewTab {}
    // The bottom tab bar is the panel's structural anchor; its default
    // terminal is a normal closable tab, recreated by the `>_` button.
    let TerminalTab = ReviewTab {
        draw_icon +: { color: C_DIM svg: crate_resource("self:resources/icons/terminal_fill.svg") }
    }
    let BrowserTab = ReviewTab {
        draw_icon +: { color: C_DIM svg: crate_resource("self:resources/icons/folder_fill.svg") }
    }

    let ReviewDock = DockFlat {
        padding: 0
        tab_bar +: {
            height: 25
            // The rest of the strip: inactive chrome with the bottom hairline.
            // The design keeps that line and hangs a shadow off it (DropShadow
            // at the top of the list); it only looked like a seam while the
            // content below it was a flat band. Palette inlined — module `let`
            // colors don't reach shader fns.
            draw_bg +: {
                color_border: uniform(mod.app_theme.color_border)
                color_card: uniform(mod.app_theme.color_card)
                pixel: fn() {
                    if self.pos.y > 1.0 - 1.0 / self.rect_size.y {
                        return self.color_border
                    }
                    return self.color_card
                }
            }
            draw_drag +: { color: C_CHROME }
        }
        splitter +: {
            draw_bg +: {
                // Window bg with a centered 1px hairline; the hairline
                // brightens on hover/drag. Colors as uniforms from mod.app_theme
                // — module `let`s don't reach shader fns.
                color_bg: uniform(mod.app_theme.color_bg)
                color_border: uniform(mod.app_theme.color_border)
                color_border_focus: uniform(mod.app_theme.color_border_focus)
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.clear(self.color_bg)
                    if self.is_vertical > 0.5 {
                        sdf.rect(0.0, self.rect_size.y * 0.5 - 0.5, self.rect_size.x, 1.0)
                    } else {
                        sdf.rect(self.rect_size.x * 0.5 - 0.5, 0.0, 1.0, self.rect_size.y)
                    }
                    sdf.fill_keep(mix(self.color_border, self.color_border_focus, max(self.hover, self.drag)))
                    return sdf.result
                }
            }
        }
        round_corner +: {
            pixel: fn() { return vec4(0.0, 0.0, 0.0, 0.0) }
        }
        // The drop highlight: the accent over the center, and while a tab
        // is dragged over a panel, that panel's own hue (see `tint_drop`).
        drag_target_preview +: {
            color: C_DRAG
        }
    }

    // A hairline between chrome groups: 1x16 with 4px of air on each side,
    // as the design draws it in the header and the status bar.
    let Divider = SolidView {
        width: 1 height: 16
        margin: Inset{left: 4 right: 4}
        draw_bg.color: C_BORDER
    }

    // A floating dropdown panel: the card fill in a hairline box, rows
    // stacked down it.
    let Dropdown = RoundedView {
        height: Fit
        flow: Down
        spacing: 2
        padding: 4
        draw_bg.color: C_CARD
        draw_bg.border_radius: 2.0
        draw_bg.border_size: 1.0
        draw_bg.border_color: C_BORDER
    }

    // One row of a picker dropdown: a full-width, left-aligned entry. Every
    // text state is pinned to C_TEXT — the "Open dir…" row opens a blocking
    // modal that can leave it focused-but-not-hovered, where the theme's
    // `color_focus`/`_down` would otherwise render it invisible.
    // One end of the header's range breadcrumb.
    let RangeChip = ButtonFlatter {
        width: Fit height: Fit
        padding: Inset{top: 1 bottom: 1 left: 4 right: 4}
        margin: 0
        text: ""
        draw_bg +: {
            color_hover: C_BORDER
            border_radius: 4.0
        }
        draw_text +: {
            color: C_DIM
            color_hover: C_TEXT
            text_style: FONT{font_size: 8.25}
        }
    }

    let ComboRow = ButtonFlatter {
        visible: false
        width: Fill height: Fit
        align: Align{x: 0.0, y: 0.5}
        padding: Inset{top: 3 bottom: 3 left: 6 right: 6}
        margin: 0
        draw_bg +: { color_hover: C_ELEMENT_HOVER border_radius: 4.0 }
        draw_text +: {
            color: C_TEXT
            color_hover: C_TEXT
            color_down: C_TEXT
            color_focus: C_TEXT
            color_disabled: C_TEXT
            text_style: FONT{font_size: 8.25}
        }
    }

    // One pane = one whole review: header, tab strip, document, status bar.
    mod.widgets.ReviewPane = #(ReviewPane::register_widget(vm)) {
        width: Fill
        height: Fill
        flow: Down

        // The header doubles as the window caption: the native macOS traffic
        // lights float over its left edge (full-size content view), and the
        // App answers WindowDragQuery for this band so it drags the window.
        header := SolidView {
            width: Fill
            // AppKit puts the native buttons 9pt down and 14pt tall, so 32
            // centres them, with the controls below on the same line. With
            // the rule below, the tab strip starts at 33.
            height: 32
            draw_bg.color: C_CHROME
            flow: Right
            spacing: 0
            align: Align{x: 0.0, y: 0.5}
            // 78 clears the native traffic lights until the window reports
            // where they are; `header_inset` takes over from there.
            padding: Inset{left: 78 right: 4}

            // The repo name doubles as the repo picker: click to drop down the
            // recent repos (and "Open dir…" for the native folder dialog).
            // Carved out of the header's drag band by the App (like the chip).
            repo_button := ButtonFlatter {
                width: Fit height: Fit
                padding: Inset{top: 1 bottom: 1 left: 4 right: 4}
                margin: 0
                text: "concats app"
                draw_bg +: {
                    color_hover: C_BORDER
                    border_radius: 4.0
                }
                // Every state pinned to C_TEXT: the click opens a blocking
                // modal that swallows the button's mouse-up, so it can be left
                // focused-but-not-hovered — the default `color_focus`/`_down`
                // would render the name invisible in that stuck state.
                draw_text +: {
                    color: C_TEXT
                    color_hover: C_TEXT
                    color_down: C_TEXT
                    color_focus: C_TEXT
                    color_disabled: C_TEXT
                    text_style: FONT{font_size: 8.25}
                }
            }
            // The range as a breadcrumb, `base / head`: each end is a chip
            // that drops down its own picker. The App's WindowDragQuery
            // handler carves the chips out of the header's drag band so the
            // clicks actually arrive. One view, so a directory without Git
            // can leave the whole range out.
            range := View {
                width: Fit height: Fit
                flow: Right
                align: Align{y: 0.5}
                Divider {}
                base_button := RangeChip {
                    icon_walk: Walk{width: 12 height: Fit}
                    draw_icon +: {
                        svg: crate_resource("self:resources/icons/git_branch.svg")
                        color: C_DIM
                    }
                }
                Label {
                    text: "/"
                    draw_text.color: C_FAINT
                    draw_text.text_style: FONT{font_size: 8.25}
                }
                head_button := RangeChip {}
            }
            // Sits right after the diff chip: click to reload the open repo
            // at its current range. Icon color is the theme's (C_DIM/C_TEXT),
            // so it tracks light/dark.
            load_button := BarButton {
                margin: Inset{left: 4}
                draw_icon.svg: crate_resource("self:resources/icons/refresh.svg")
            }
            View { width: Fill height: Fit }
            // Share lives in the title bar, top right — per the design.
            share_button := BarButton {
                draw_icon +: {
                    // Brighter than the status bar's icons, like the design.
                    svg: crate_resource("self:resources/icons/share.svg")
                    color: C_TEXT
                    color_hover: C_TEXT
                }
            }
        }
        header_rule := HeaderRule {}

        body_overlay := View {
            width: Fill
            height: Fill
            flow: Overlay

            // The dock owns the tab strip and every pane: tabs drag to
            // reorder, drop on an edge to split, drop on a bar to merge. The
            // bottom panel hosts the terminal, the right one the file browser,
            // and the left one starts empty. All three ride the dock's own
            // splitters, so all slide and drag by the same handle.
            dock := ReviewDock {
                width: Fill
                height: Fill

                tab_bar +: {
                    GuideTab := GuideTab{}
                    SessionsTab := SessionsTab{}
                    CommitsTab := CommitsTab{}
                    FilesTab := FilesTab{}
                    CommentsTab := CommentsTab{}
                    FileTab := FileTab{}
                    TerminalTab := TerminalTab{}
                    SettingsTab := SettingsTab{}
                    BrowserTab := BrowserTab{}
                }

                // Four areas: the center, and a left, right and bottom panel.
                // The side panels hang off the two outer splitters so they
                // span the bottom one, like an editor's; each panel's
                // splitter spans the dock along its axis, which is what lets
                // a panel's size be read off its splitter's position. A shut
                // panel is its side at 0.
                root := DockSplitter {
                    axis: SplitterAxis.Horizontal
                    // Open by default, unlike the others: the browser is how
                    // you reach a file, so hiding it hides the way in.
                    align: SplitterAlign.FromB(260.0)
                    a: @inner
                    b: @right_tabs
                }
                inner := DockSplitter {
                    axis: SplitterAxis.Horizontal
                    align: SplitterAlign.FromA(0.0)
                    a: @left_tabs
                    b: @body_split
                }
                body_split := DockSplitter {
                    axis: SplitterAxis.Vertical
                    align: SplitterAlign.FromB(0.0)
                    a: @main_tabs
                    b: @bottom_tabs
                }
                left_tabs := DockTabs {
                    tabs: []
                    selected: 0
                    closable: false
                }
                right_tabs := DockTabs {
                    tabs: [@browser_tab]
                    selected: 0
                    closable: false
                }
                main_tabs := DockTabs {
                    tabs: [@guide_tab @sessions_tab @commits_tab @files_tab @comments_tab]
                    selected: 0
                    closable: false
                }
                bottom_tabs := DockTabs {
                    tabs: [@terminal_tab]
                    selected: 0
                    closable: false
                }

                guide_tab := DockTab { name: "Guide" template: @GuideTab kind: @GuidePane }
                sessions_tab := DockTab { name: "Sessions" template: @SessionsTab kind: @SessionsPane }
                commits_tab := DockTab { name: "Commits" template: @CommitsTab kind: @CommitsPane }
                files_tab := DockTab { name: "File Diff" template: @FilesTab kind: @FilesPane }
                comments_tab := DockTab { name: "Comments" template: @CommentsTab kind: @CommentsPane }
                terminal_tab := DockTab { name: "Terminal" template: @TerminalTab kind: @TerminalPane }
                // Predefined but not in the default tab list — opened on demand
                // by the `{ }` button (mirrors how stream tabs re-open).
                settings_tab := DockTab { name: "Settings" template: @SettingsTab kind: @SettingsPane }
                browser_tab := DockTab { name: "Files" template: @BrowserTab kind: @BrowserPane }

                // One stream-pinned list per tab kind.
                GuidePane := ReviewList { kind: @guide }
                SessionsPane := ReviewList { kind: @sessions }
                CommitsPane := ReviewList { kind: @commits }
                FilesPane := ReviewList { kind: @files }
                CommentsPane := ReviewList { kind: @comments }
                FilePane := ReviewList { kind: @file }
                TerminalPane := TerminalView {}
                // Opened on demand by the `{ }` status-bar button (not in the
                // default tab list) — like the `+`-created terminal sessions.
                SettingsPane := ReviewList { kind: @file }
                BrowserPane := FileBrowser {}
            }

            loading_overlay := View {
                width: Fill
                height: Fill
                visible: false
                align: Align{x: 0.5, y: 0.08}
                padding: Inset{top: 20}
                RoundedView {
                    width: Fit
                    height: Fit
                    padding: Inset{left: 12 right: 12 top: 7 bottom: 7}
                    draw_bg.color: C_CARD
                    draw_bg.border_color: C_BORDER
                    draw_bg.border_size: 1.0
                    draw_bg.border_radius: 2.0
                    Label {
                        text: "Loading review…"
                        draw_text.color: C_DIM
                        draw_text.text_style: FONT{font_size: 9}
                    }
                }
            }

            // The range picker: a floating panel under whichever end's chip
            // opened it. A filter over what that end can be; Enter accepts a
            // typed rev, or a `base...head` range for both ends.
            combo_panel := Dropdown {
                visible: false
                width: 340
                margin: Inset{left: 78 top: 4}

                combo_input := DarkInput {
                    width: Fill
                    draw_text.text_style: FONT{font_size: 8.25}
                }
                combo_row0 := ComboRow {}
                combo_row1 := ComboRow {}
                combo_row2 := ComboRow {}
                combo_row3 := ComboRow {}
                combo_row4 := ComboRow {}
                combo_row5 := ComboRow {}
                combo_row6 := ComboRow {}
                combo_row7 := ComboRow {}
            }

            // The repo picker: a floating panel under the repo name. The recent
            // repos as rows, then "Open dir…" to browse for another one.
            repo_panel := Dropdown {
                visible: false
                width: 300
                margin: Inset{left: 78 top: 4}

                // This repository's checkouts, shown when it has more than one.
                worktree_row0 := ComboRow {}
                worktree_row1 := ComboRow {}
                worktree_row2 := ComboRow {}
                worktree_row3 := ComboRow {}
                worktree_row4 := ComboRow {}
                worktree_row5 := ComboRow {}
                worktree_row6 := ComboRow {}
                worktree_row7 := ComboRow {}
                worktree_rule := SolidView {
                    visible: false
                    width: Fill height: 1
                    margin: Inset{top: 2 bottom: 2}
                    draw_bg.color: C_BORDER
                }
                repo_row0 := ComboRow {}
                repo_row1 := ComboRow {}
                repo_row2 := ComboRow {}
                repo_row3 := ComboRow {}
                repo_row4 := ComboRow {}
                SolidView {
                    width: Fill height: 1
                    margin: Inset{top: 2 bottom: 2}
                    draw_bg.color: C_BORDER
                }
                open_dir_row := ComboRow { visible: true text: "Open dir…" }
            }

            // The share dropdown: a floating panel under the share button.
            // One row per output form — more targets will slot in here.
            share_panel := View {
                visible: false
                width: Fill height: Fit
                align: Align{x: 1.0, y: 0.0}
                padding: Inset{right: 4 top: 2}
                Dropdown {
                    width: 150
                    share_prompt := ComboRow { visible: true text: "Copy as prompt" }
                    share_json := ComboRow { visible: true text: "Copy as JSON" }
                    // WORKTREE reviews only (toggled on open): `git add -p`
                    // driven by the seen ticks.
                    share_stage := ComboRow { visible: false text: "Stage seen hunks" }
                }
            }

            perf_overlay := View {
                width: Fill
                height: Fill
                visible: false
                align: Align{x: 1.0, y: 0.0}
                padding: Inset{top: 8 bottom: 8 left: 8 right: 8}
                perf_graph := PerfGraph {}
            }
        }

        SolidView { width: Fill height: 1 draw_bg.color: C_BORDER }
        status := SolidView {
            width: Fill
            height: 27
            draw_bg.color: C_CHROME
            flow: Right
            spacing: 1
            align: Align{x: 0.0, y: 0.5}
            padding: Inset{left: 4 right: 4}

            // How much of the diff is ticked seen — the review in one bar.
            progress := SeenBar {}
            Divider {}
            // Transient feedback only (a load, a share, a stage): the design's
            // status bar is otherwise text-free.
            status_label := Label {
                width: Fill
                text: ""
                margin: Inset{left: 4}
                draw_text.color: C_FAINT
                draw_text.text_style: FONT{font_size: 8.25}
            }
            // One button per view: reopen (or jump to) its tab. Buttons for
            // streams this range doesn't have are hidden, like the tabs.
            comments_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/comment.svg")
            }
            files_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/file_diff.svg")
            }
            commits_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/commits.svg")
            }
            sessions_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/sessions.svg")
            }
            guide_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/guide.svg")
            }
            // The file browser: reopen it, select it, show its panel.
            browser_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/folder.svg")
            }
            Divider {}
            // The terminal: opens the bottom panel on the shell.
            terminal_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/terminal.svg")
            }
            // … and its `+`: another shell session in a new tab.
            terminal_add_button := BarButton {
                icon_walk: Walk{width: 11 height: Fit}
                draw_icon.svg: crate_resource("self:resources/icons/plus.svg")
            }
            Divider {}
            // The settings editor: a TOML view of the shared config.
            settings_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/settings.svg")
            }
            Divider {}
            // The three panels, each shown or hidden with whatever it holds,
            // empty or not.
            left_panel_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/panel_left.svg")
            }
            bottom_panel_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/panel_bottom.svg")
            }
            right_panel_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/panel_right.svg")
            }
        }
    }
}

/// What a pane asks of the App: run the command its palette picked, in its
/// window. Some commands are the App's to run, such as opening a window.
#[derive(Clone, Debug, Default)]
pub enum ReviewPaneAction {
    Run(Command),
    #[default]
    None,
}

/// One of the pane's three sliding panels. Each is one side of a splitter
/// that spans the dock along its axis, shuts to 0 on that side, and reopens
/// to the size the user last dragged, so all three share the machinery below.
#[derive(Clone, Copy, PartialEq)]
enum Panel {
    Left,
    Right,
    Bottom,
}

impl Panel {
    const ALL: [Panel; 3] = [Panel::Left, Panel::Right, Panel::Bottom];

    /// The dock splitter this panel rides.
    fn splitter(self) -> LiveId {
        match self {
            Panel::Left => id!(inner),
            Panel::Right => id!(root),
            Panel::Bottom => id!(body_split),
        }
    }

    /// The splitter alignment that makes this panel `size` wide or tall: the
    /// left panel is its splitter's `a`, measured from the start, the others
    /// its `b`.
    fn align(self, size: f64) -> SplitterAlign {
        match self {
            Panel::Left => SplitterAlign::FromA(size),
            Panel::Right | Panel::Bottom => SplitterAlign::FromB(size),
        }
    }

    /// The dock item on this panel's side of its splitter: what it shows.
    fn side(self, items: &HashMap<LiveId, DockItem>) -> Option<LiveId> {
        match (self, items.get(&self.splitter())?) {
            (Panel::Left, DockItem::Splitter { a, .. }) => Some(*a),
            (_, DockItem::Splitter { b, .. }) => Some(*b),
            _ => None,
        }
    }
}

/// The area `abs` is over in a dock at `rect` whose left, right and bottom
/// panels are `sizes` big: the bottom panel spans the center column only.
fn area_at(abs: Vec2d, rect: Rect, sizes: [f64; 3]) -> concats_config::Area {
    let [left, right, bottom] = sizes;
    if abs.x < rect.pos.x + left {
        concats_config::Area::Left
    } else if abs.x > rect.pos.x + rect.size.x - right {
        concats_config::Area::Right
    } else if abs.y > rect.pos.y + rect.size.y - bottom {
        concats_config::Area::Bottom
    } else {
        concats_config::Area::Center
    }
}

/// A panel slide in flight: where it started, where it is going, and when it
/// began — set on the first frame, so the easing is wall-clock and not
/// frame-rate bound.
struct Slide {
    panel: Panel,
    from: f64,
    to: f64,
    start: Option<f64>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ReviewPane {
    #[deref]
    view: View,
    /// The window this pane renders. Set by the App the moment the window is
    /// opened, and handed to the rows below through `Scope` — `Root` gives
    /// every window the same scope, so the per-window one starts here.
    #[rust]
    state: Option<std::sync::Arc<WindowState>>,
    #[rust]
    review_git_dir: Option<PathBuf>,
    #[rust]
    review_output: Shared<service::ReviewState>,
    #[rust]
    file_titles_generation: u64,
    #[rust]
    file_titles_rows_rev: u64,
    /// The panel slide in flight, stepped on `NextFrame` with studio's 0.16s
    /// ease-out cubic (`app_backend.rs`). One at a time: the two panels are
    /// toggled by two different buttons.
    #[rust]
    slide: Option<Slide>,
    #[rust]
    slide_next_frame: NextFrame,
    /// The area the drop highlight was last tinted for.
    #[rust]
    drop_area: Option<concats_config::Area>,
    /// The tab last pressed or opened: where the keyboard is when nothing
    /// holds the key focus.
    #[rust]
    last_tab: Option<LiveId>,
    /// The open picker's row the arrow keys have walked to.
    #[rust]
    picker_cursor: Option<usize>,
    /// Sizes to restore each panel to on reopen (0 = never opened yet, so the
    /// default). Written by the App from the persisted layout on load.
    #[rust]
    pub left_restore: f64,
    #[rust]
    pub right_restore: f64,
    #[rust]
    pub bottom_restore: f64,
    /// Stream tabs the user closed: reconciliation must not resurrect them
    /// on the next load — only their status-bar button reopens them. The App
    /// reads and seeds it during dock reconciliation.
    #[rust]
    pub user_closed: HashSet<LiveId>,
    /// Numbering for `+`-created terminal tabs ("Terminal 2", …).
    #[rust]
    next_terminal: usize,
    /// The recent repo paths shown in the repo picker, most-recent first. Held
    /// so a row click maps back to its full path (the rows show basenames).
    #[rust]
    recents: Vec<String>,
    /// Which end of the range the range picker edits while it is open.
    #[rust]
    range_end: RangeEnd,
    /// What the range picker offers for that end: each row's text and the
    /// range it loads. Filled when the picker opens.
    #[rust]
    range_rows: Vec<RangeRow>,
    /// The ranges behind the visible rows, in slot order.
    #[rust]
    range_shown: Vec<Target>,
    /// The palette's rows while the dropdown is the palette: what this
    /// window can run, each with its keys.
    #[rust]
    palette: Option<Vec<(Command, String)>>,
    /// The commands behind the palette's visible rows, in slot order.
    #[rust]
    palette_shown: Vec<Command>,
    /// Where the keyboard was when the palette opened.
    #[rust]
    palette_return: Area,
    /// The commands the palette ran lately, most recent first.
    #[rust]
    recent_commands: Vec<Command>,
    /// The dropdown just opened: focus its input after the next draw.
    #[rust]
    focus_combo: bool,
}

/// The status-bar view buttons, one per stream.
const VIEW_BUTTONS: [(&[LiveId], Stream); 5] = [
    (ids!(guide_button), Stream::Guide),
    (ids!(sessions_button), Stream::Sessions),
    (ids!(commits_button), Stream::Commits),
    (ids!(files_button), Stream::Files),
    (ids!(comments_button), Stream::Comments),
];

impl ReviewPane {
    // NOTE: a handle cached per repo and refilled when the repo changes; the state is only read.
    // coland-ignore: @coland/read_write_operation
    fn review_snapshot(&mut self, git_dir: Option<&Path>) -> Arc<service::ReviewState> {
        if self.review_git_dir.as_deref() != git_dir {
            self.review_output = review_state(git_dir);
            self.review_git_dir = git_dir.map(Path::to_path_buf);
        }
        self.review_output.load()
    }

    /// Take the window this pane renders. Called by the App as the window
    /// opens, before any event reaches the pane.
    pub(crate) fn adopt(&mut self, state: std::sync::Arc<WindowState>) {
        self.state = Some(state);
    }

    /// The window this pane renders. Before the App has adopted it — the first
    /// frames of a run — this answers with a detached empty document, which is
    /// what the pane would draw at that point anyway.
    fn state(&self) -> &std::sync::Arc<WindowState> {
        self.state
            .as_ref()
            .unwrap_or_else(|| crate::window::WindowState::detached())
    }

    /// Open the Settings dock tab — creating it next to the stream tabs if it
    /// isn't open, else selecting it.
    pub fn open_settings_tab(&mut self, cx: &mut Cx) {
        self.state().with(crate::file_view::open_settings);
        let area = crate::theme::active_placement().settings;
        self.open_view(cx, settings_tab_spec(), &stream_tabs_in(area), area);
        self.view.redraw(cx);
    }

    /// Show one file of the head tree in the File tab, replacing whatever it
    /// held — one tab per file, like an editor. Picking a file that is already
    /// open selects its tab instead of stacking a second copy: the tab id
    /// derives from the path, so "already open" is a lookup, not a search.
    ///
    /// The read runs here rather than on the load thread: it is one blob out of
    /// the ODB (or one `read` of a working file) plus a newline scan —
    /// single-digit milliseconds — and it must not bump `generation`, the only
    /// thing a landed background load could signal with.
    ///
    /// `at` is a 1-based line and column to put the caret on and bring into
    /// view; without it the tab opens where it was, at the top when new.
    pub fn open_file_tab(&mut self, cx: &mut Cx, path: &str, at: Option<(u32, u32)>) {
        let (repo, range) = self.state().read(|d| (d.repo.clone(), d.resolved));
        let sides = match read_file_sides(&repo, range, path) {
            Ok(sides) => sides,
            Err(e) => {
                return self
                    .view
                    .label(cx, ids!(status_label))
                    .set_text(cx, &e.to_string());
            }
        };
        let git_dir = self.state().read(|d| d.git_dir.clone());
        let comments = self.review_snapshot(git_dir.as_deref()).comments.clone();
        let minted = self.state().with(|d| open_file(d, path, sides, &comments));
        hold_minted(git_dir.as_deref(), minted);

        let tab_id = file_tab_id(path);
        self.show_file_tab(cx, tab_id.0);
        if let Some((line, column)) = at {
            let row = self
                .state()
                .with(|d| caret_to_line(d, tab_id.0, line, column));
            *self.state().reveal.lock().unwrap() = row.map(|row| (Stream::File(tab_id.0), row));
        }
        self.redraw_streams(cx);
    }

    /// ⌘N: an empty Untitled tab where file tabs open, selected, with the
    /// keyboard in it. Its first save asks where it goes.
    pub fn new_file(&mut self, cx: &mut Cx) {
        let tab = self.state().with(crate::file_view::open_untitled);
        self.show_file_tab(cx, tab);
        *self.state().reveal.lock().unwrap() = Some((Stream::File(tab), 0));
        self.redraw_streams(cx);
    }

    /// The first save of an Untitled tab: ask where in the native Save dialog,
    /// write the buffer there, and make the tab that file's. Only a path in the
    /// checkout, and one no other tab has open, is taken.
    fn save_as(&mut self, cx: &mut Cx, tab: u64) {
        let (root, text) = self.state().read(|d| {
            let text = d
                .file(tab)
                .map(|view| d.blobs[view.head as usize].text.clone());
            (d.root.clone(), text)
        });
        let Some(text) = text else {
            return;
        };
        // NOTE: the native dialog can't be driven headlessly, so the tests
        // answer for it; `-` cancels.
        let picked = match crate::dev_hooks::var("CONCATS_APP_SAVE_AS") {
            Some(answer) => (answer != "-").then(|| root.join(answer)),
            None => rfd::FileDialog::new()
                .set_directory(&root)
                .set_file_name("Untitled")
                .save_file(),
        };
        let Some(picked) = picked else {
            return;
        };
        let relative = |root: &std::path::Path| {
            let path = picked.strip_prefix(root).ok()?.to_str()?.to_owned();
            let file = concats_diff::load::worktree_file(root, &path).ok()?;
            Some((path, file))
        };
        let message = match relative(&root).or_else(|| relative(&root.canonicalize().ok()?)) {
            None => format!("can't save outside {}", root.display()),
            Some((path, _))
                if self
                    .state()
                    .read(|d| d.files_open.iter().any(|f| f.path == path)) =>
            {
                format!("{path} is already open")
            }
            Some((path, file)) => {
                if let Err(error) = std::fs::write(&file, &text) {
                    format!("cannot save {path}: {error}")
                } else {
                    let named = self
                        .state()
                        .with(|d| crate::file_view::name_untitled(d, tab, &path, file));
                    self.view.dock(cx, ids!(dock)).close_tab(cx, LiveId(tab));
                    if let Some(named) = named {
                        self.show_file_tab(cx, named);
                    }
                    // A reload is what puts a new file in the browser.
                    let target = self.state().read(ReviewDoc::target);
                    self.start_load(cx, target, None, None);
                    format!("saved {path}")
                }
            }
        };
        self.view
            .label(cx, ids!(status_label))
            .set_text(cx, &message);
        self.redraw_streams(cx);
    }

    /// Open the File tab `tab`, whose view is in the document, where file tabs
    /// open, select it and make it the one gestures act in.
    fn show_file_tab(&mut self, cx: &mut Cx, tab: u64) {
        let title = self.state().read(|d| {
            d.file(tab)
                .map(|view| crate::file_view::file_tab_title(d, view))
                .unwrap_or_default()
        });
        let spec = TabSpec {
            title: &title,
            ..stream_tab_spec(Stream::File(tab))
        };
        let area = crate::theme::active_placement().file;
        self.open_view(cx, spec, &stream_tabs_in(area), area);
        self.set_gesture_tab(cx, Stream::File(tab));
    }

    /// Open a view: select its tab where it is, or make it in `area`, beside
    /// `siblings`, when it isn't open; then slide open the panel it is in.
    fn open_view(
        &mut self,
        cx: &mut Cx,
        spec: TabSpec<'_>,
        siblings: &[LiveId],
        area: concats_config::Area,
    ) {
        let dock = self.view.dock(cx, ids!(dock));
        open_tab(cx, &dock, spec, siblings, area_bar(area));
        dock.select_tab(cx, spec.id);
        self.last_tab = Some(spec.id);
        self.reveal_tab(cx, spec.id);
    }

    /// With no saved layout to restore, move the views the default layout put
    /// elsewhere to where Settings places them.
    pub(crate) fn place_views(&mut self, cx: &mut Cx) {
        let placement = crate::theme::active_placement();
        let dock = self.view.dock(cx, ids!(dock));
        let views = std::iter::once(Stream::Files)
            .chain(crate::review_doc::STREAMS)
            .map(|stream| (stream_tab_spec(stream), stream_area(&placement, stream)))
            .chain([
                (terminal_tab_spec(), placement.terminal),
                (browser_tab_spec(), placement.browser),
            ]);
        for (spec, area) in views {
            let bar = area_bar(area);
            if dock
                .find_tab_bar_of_tab(spec.id)
                .is_some_and(|(at, _)| at != bar)
            {
                dock.close_tab(cx, spec.id);
                open_tab(cx, &dock, spec, &[], bar);
            }
        }
    }

    /// The File tabs' ids: a File tab is its file's identity, so telling one
    /// from a terminal's tab takes the document's open list.
    fn open_files(&self) -> Vec<u64> {
        self.state()
            .read(|d| d.files_open.iter().map(|f| f.tab).collect())
    }

    /// `tab_id` came to the front, pressed or by a key: gestures and typing
    /// go there now, and a terminal whose shell exited starts a new one.
    fn tab_pressed(&mut self, cx: &mut Cx, tab_id: LiveId) {
        self.last_tab = Some(tab_id);
        self.state().set_terminal_tab(None);
        if let Some(tab) = model_tab_of(tab_id, &self.open_files()) {
            self.set_gesture_tab(cx, tab);
        } else if terminal_tabs(&self.view.dock(cx, ids!(dock))).contains(&tab_id) {
            // (Re)spawn that tab's shell — the recovery path after its
            // previous one exited.
            self.open_terminal(cx, tab_id);
            self.state().set_terminal_tab(Some(tab_id));
        }
    }

    /// Close `tab_id` the way its ✕ does: a File tab drops its stream, a
    /// stream tab stays closed until its button reopens it, but never the
    /// last one, and a terminal's shell ends.
    fn close_tab(&mut self, cx: &mut Cx, tab_id: LiveId) {
        let dock = self.view.dock(cx, ids!(dock));
        let open_files = self.open_files();
        if open_files.contains(&tab_id.0) {
            self.close_file_tab(cx, tab_id);
        } else if model_tab_of(tab_id, &open_files).is_some() {
            // Keep at least one stream tab open — an empty main area would
            // leave nothing to navigate from.
            let remaining = crate::review_doc::STREAMS
                .into_iter()
                .map(|t| stream_tab_spec(t).id)
                .filter(|t| *t != tab_id && dock.find_tab_bar_of_tab(*t).is_some())
                .count();
            if remaining > 0 {
                self.user_closed.insert(tab_id);
                dock.close_tab(cx, tab_id);
            }
        } else if terminal_tabs(&dock).contains(&tab_id) {
            // Closing a terminal tab ends its shell.
            if self.state().terminal_tab() == Some(tab_id) {
                self.state().set_terminal_tab(None);
            }
            terminal::close(terminal::Session {
                window: self.state().id,
                tab: tab_id,
            });
            dock.close_tab(cx, tab_id);
        } else if [settings_tab_spec().id, browser_tab_spec().id].contains(&tab_id) {
            dock.close_tab(cx, tab_id);
        }
    }

    /// A command that opens a panel, a view or a picker, or reloads. A view
    /// opened takes the keyboard, the way VS Code's and Zed's focus commands
    /// do: the terminal takes typing, the browser and a list take find.
    pub(crate) fn view_command(&mut self, cx: &mut Cx, command: Command) {
        let dock = self.view.dock(cx, ids!(dock));
        match command {
            Command::TogglePanel(area) => {
                let panel = match area {
                    concats_config::Area::Left => Panel::Left,
                    concats_config::Area::Right => Panel::Right,
                    concats_config::Area::Bottom => Panel::Bottom,
                    concats_config::Area::Center => return,
                };
                self.toggle_panel(cx, panel);
            }
            Command::OpenTerminal | Command::NewTerminal => {
                if command == Command::OpenTerminal {
                    self.reveal_terminal(cx);
                } else {
                    self.add_terminal_session(cx);
                }
                // With nothing focused, the window's terminal takes the keys.
                cx.set_key_focus(Area::Empty);
            }
            Command::OpenBrowser => {
                let spec = browser_tab_spec();
                let area = crate::theme::active_placement().browser;
                self.open_view(cx, spec, &[], area);
                let tree = dock.item(spec.id).file_tree(cx, ids!(tree)).area();
                cx.set_key_focus(tree);
            }
            Command::OpenStream(stream) => {
                self.open_stream_tab(cx, stream);
                let list = dock
                    .item(stream_tab_spec(stream).id)
                    .portal_list(cx, ids!(list))
                    .area();
                cx.set_key_focus(list);
            }
            Command::OpenRepo => {
                self.repo_open(cx);
                cx.set_key_focus(Area::Empty);
            }
            Command::PickBase => self.combo_open(cx, pickers::RangeEnd::Base),
            Command::PickHead => self.combo_open(cx, pickers::RangeEnd::Head),
            Command::Palette => self.palette_open(cx),
            Command::Reload => self.reload(cx),
            _ => {}
        }
    }

    /// A review command, in the current tab if it is a stream: walking its
    /// hunks, cards and find matches, or acting on its current card.
    pub(crate) fn review_command(&mut self, cx: &mut Cx, command: Command) {
        let Some(tab) = self
            .current_tab(cx)
            .and_then(|tab| model_tab_of(tab, &self.open_files()))
        else {
            return;
        };
        let item = self.view.dock(cx, ids!(dock)).item(stream_tab_spec(tab).id);
        let card = item
            .borrow::<ReviewList>()
            .and_then(|list| list.current_card());
        match (command, card) {
            (Command::ToggleSeen, Some((path, header, _))) => {
                self.toggle_card_seen(cx, tab, &path, header);
            }
            (Command::FoldCard, Some((path, header, _))) => {
                // Whether every changed line is ticked decides whether Collapse
                // seen is what shuts it, as for the header's caret.
                let git_dir = self.state().read(|d| d.git_dir.clone());
                let review = self.review_snapshot(git_dir.as_deref());
                let seen = self.state().read(|d| {
                    card_keys(d.stream(tab), &path, header, &d.blobs)
                        .iter()
                        .all(|key| review.seen.contains(key))
                });
                self.toggle_card_fold(cx, &path, seen);
            }
            (Command::CommentOnSelection, Some((_, _, near))) => self.compose_span(cx, tab, near),
            (Command::ToggleSeen | Command::FoldCard | Command::CommentOnSelection, None) => {}
            _ => {
                if let Some(mut list) = item.borrow_mut::<ReviewList>() {
                    list.review_step(cx, command);
                }
                self.redraw_streams(cx);
            }
        }
    }

    /// The tab the keyboard is in: the one holding the key focus, else the
    /// one last pressed or opened, else the center's.
    pub(crate) fn current_tab(&mut self, cx: &mut Cx) -> Option<LiveId> {
        let dock = self.view.dock(cx, ids!(dock));
        let items = dock.clone_state()?;
        let shown: Vec<LiveId> = items
            .values()
            .filter_map(|item| match item {
                DockItem::Tabs { tabs, selected, .. } => tabs.get(*selected).copied(),
                _ => None,
            })
            .collect();
        let focus = cx.key_focus();
        let focused = (!focus.is_empty())
            .then(|| focus.rect(cx).center())
            .and_then(|at| {
                shown
                    .iter()
                    .copied()
                    .find(|tab| dock.item(*tab).area().rect(cx).contains(at))
            });
        focused
            .or(self.last_tab.filter(|tab| shown.contains(tab)))
            .or_else(|| match items.get(&id!(main_tabs)) {
                Some(DockItem::Tabs { tabs, selected, .. }) => tabs.get(*selected).copied(),
                _ => None,
            })
    }

    /// A tab command, in the current tab's bar.
    pub(crate) fn tab_command(&mut self, cx: &mut Cx, command: Command) {
        let Some(current) = self.current_tab(cx) else {
            return;
        };
        let dock = self.view.dock(cx, ids!(dock));
        let Some((bar, at)) = dock.find_tab_bar_of_tab(current) else {
            return;
        };
        let Some(DockItem::Tabs { tabs, .. }) =
            dock.clone_state().and_then(|mut items| items.remove(&bar))
        else {
            return;
        };
        let saved = |pane: &Self, tab: LiveId| {
            pane.state().read(|d| {
                d.file(tab.0)
                    .is_some_and(|view| !d.blobs[view.head as usize].dirty())
            })
        };
        let select = match command {
            Command::NextTab => tabs.get((at + 1) % tabs.len()),
            Command::PreviousTab => tabs.get((at + tabs.len() - 1) % tabs.len()),
            Command::Tab(n) => tabs.get(n.wrapping_sub(1)),
            _ => None,
        };
        let close: Vec<LiveId> = match command {
            Command::CloseTab => vec![current],
            Command::CloseOtherTabs => tabs.iter().copied().filter(|t| *t != current).collect(),
            Command::CloseSavedTabs => tabs.iter().copied().filter(|t| saved(self, *t)).collect(),
            Command::CloseAllTabs => tabs.clone(),
            _ => Vec::new(),
        };
        if let Some(tab) = select {
            dock.select_tab(cx, *tab);
            self.tab_pressed(cx, *tab);
        }
        for tab in close {
            self.close_tab(cx, tab);
        }
        // What the bar shows now is where the keyboard is.
        if let Some(DockItem::Tabs { tabs, selected, .. }) =
            dock.clone_state().and_then(|mut items| items.remove(&bar))
            && let Some(tab) = tabs.get(selected)
        {
            self.tab_pressed(cx, *tab);
        }
        self.redraw_streams(cx);
    }

    /// Drop a closed file tab's stream. The tab is the file's identity, so a
    /// tab that is gone has no stream to keep — and leaving it would have the
    /// comment and composer passes walking rows nothing renders.
    fn close_file_tab(&mut self, cx: &mut Cx, tab_id: LiveId) {
        self.state().with(|d| {
            if d.composer_tab == Some(Stream::File(tab_id.0)) || d.tab == Stream::File(tab_id.0) {
                drop_composer(self.state(), d);
            }
            d.files_open.retain(|f| f.tab != tab_id.0);
            // The gesture cannot stay pointed at a stream that no longer
            // exists; the diff is where every range starts.
            if d.tab == Stream::File(tab_id.0) {
                d.tab = Stream::Files;
            }
        });
        self.view.dock(cx, ids!(dock)).close_tab(cx, tab_id);
        self.redraw_streams(cx);
    }

    /// A load began (`on`) or ended: show or hide the "Loading review…" card.
    /// Called by the App from the doc's `loading` flag on every load signal,
    /// and directly by `combo_load` so a user-triggered load says so at once
    /// (the transient `loading` flag can otherwise be coalesced away before the
    /// UI sees it). Covers every load: branch switch, repo switch, reload,
    /// worktree refresh, startup.
    pub fn set_loading(&mut self, cx: &mut Cx, on: bool) {
        self.view
            .view(cx, ids!(loading_overlay))
            .set_visible(cx, on);
    }

    pub fn start_load(
        &mut self,
        cx: &mut Cx,
        target: concats_state::Target,
        guide: Option<String>,
        status: Option<&str>,
    ) {
        if self.state().read(|document| document.repo != target.repo) {
            let window = self.state().id;
            let tabs = self
                .view
                .dock(cx, ids!(dock))
                .clone_state()
                .into_iter()
                .flatten()
                .filter_map(|(tab, item)| {
                    matches!(item, DockItem::Tab { kind, .. } if kind == id!(TerminalPane))
                        .then_some(tab)
                })
                .collect::<Vec<_>>();
            for tab in tabs {
                let session = terminal::Session { window, tab };
                if terminal::is_open(session) {
                    terminal::close(session);
                    self.open_terminal_for(cx, tab, &target.repo);
                }
            }
        }
        if let Some(status) = status {
            self.view.label(cx, ids!(status_label)).set_text(cx, status);
        }
        self.set_loading(cx, true);
        spawn_load(self.state(), target, guide);
    }

    /// Point the gesture/composer state at the given stream: pressing a dock
    /// tab or starting a gutter gesture in one of its panes moves it there.
    /// An open composer belongs to the stream it was opened in — leaving
    /// that stream closes it. (Scroll state lives per list instance now, so
    /// there is nothing to reset here.)
    pub fn set_gesture_tab(&mut self, cx: &mut Cx, tab: Stream) {
        let changed = self.state().with(|d| {
            if d.tab == tab {
                return false;
            }
            drop_composer(self.state(), d);
            d.tab = tab;
            true
        });
        if changed {
            self.view.redraw(cx);
        }
    }

    /// The header tick box: every changed (blob, line) key of every hunk in
    /// this file card flips together. Content-addressed, so the mark shows up
    /// in every view that renders those lines.
    fn toggle_card_seen(&mut self, cx: &mut Cx, tab: Stream, path: &str, near: usize) {
        let docs = self.state().snapshot();
        let d = &*docs;
        let keys = card_keys(d.stream(tab), path, near, &d.blobs);
        if keys.is_empty() {
            return;
        }
        let Some(git_dir) = d.git_dir.clone() else {
            return;
        };
        drop(docs);
        // Optimistic: the published state flips now, the sqlite write lands
        // on the service thread.
        service::toggle_seen(&git_dir, keys);
        self.refresh_progress(cx);
        self.redraw_streams(cx);
    }

    // NOTE: Dock::redraw invalidates its own area only, so stream mutations
    // must also invalidate the lists inside its tabs.
    fn redraw_streams(&mut self, cx: &mut Cx) {
        self.view.redraw(cx);
        let dock = self.view.dock(cx, ids!(dock));
        let files = self
            .state()
            .read(|d| d.files_open.iter().map(|f| f.tab).collect::<Vec<_>>());
        for tab in crate::review_doc::STREAMS
            .into_iter()
            .chain(files.into_iter().map(Stream::File))
        {
            dock.item(stream_tab_spec(tab).id).redraw(cx);
        }
    }

    /// Fold a file card shut (or open it again). Keyed by path, so the card
    /// is shut in every stream that renders that file — and the list rebuilds
    /// its entry→row mapping on the next draw. `seen` is whether all of it is
    /// ticked, which decides whether Collapse seen is what shuts it.
    fn toggle_card_fold(&mut self, cx: &mut Cx, path: &str, seen: bool) {
        self.state()
            .with(|d| crate::review_doc::toggle_card(d, path, seen));
        self.redraw_streams(cx);
    }

    /// Show or hide the conversations this range cannot place for a file.
    /// Unlike folding, this changes which comment rows exist, so it resplices
    /// — and announces the new shape, because every row-indexed cache below
    /// is stale the moment a row is inserted mid-stream.
    // NOTE: rows_rev counts row changes; splice_comments bumps it only for the composer's row.
    // coland-ignore: @coland/mixed_abstraction_levels
    fn toggle_card_outdated(&mut self, cx: &mut Cx, path: &str) {
        let git_dir = self.state().read(|d| d.git_dir.clone());
        let comments = self.review_snapshot(git_dir.as_deref()).comments.clone();
        self.state().with(|d| {
            if !d.show_all_comments.remove(path) {
                d.show_all_comments.insert(path.to_string());
            }
            resplice_comments(d, &comments);
            d.rows_rev += 1;
        });
        self.redraw_streams(cx);
    }

    /// Re-tally the status bar's review-progress bar. Called wherever seen
    /// state can move: a tick box here, a landed load, another process's
    /// write picked up by the poll.
    pub fn refresh_progress(&mut self, cx: &mut Cx) {
        let (seen, total) = {
            let docs = self.state().snapshot();
            seen_progress(&docs, &self.review_snapshot(docs.git_dir.as_deref()))
        };
        if let Some(mut bar) = self.view.widget(cx, ids!(progress)).borrow_mut::<SeenBar>() {
            bar.set_progress(cx, seen, total);
        }
    }

    /// Share: the threads on screen in whichever form the dropdown row picked
    /// — the JSON document `concats comments import` reads back, or text to
    /// paste into an agent session. The service renders it the way the CLI and
    /// the pipe export, and the App puts it on the clipboard.
    fn share_comments(
        &mut self,
        cx: &mut Cx,
        render: fn(&Loaded, &[interchange::Entry]) -> String,
    ) {
        self.view.view(cx, ids!(share_panel)).set_visible(cx, false);
        let (target, shown) = self
            .state()
            .read(|d| (d.target(), d.placed_threads.clone()));
        review().send(ReviewCmd::Share {
            window: self.state().id,
            target,
            shown,
            render,
        });
    }

    /// Share = stage: `git add -p` driven by the seen ticks. Every fully seen
    /// hunk of the WORKTREE review goes into the index; everything else —
    /// unticked hunks, files that moved since the load — stays put. The
    /// worktree poll notices the index change and reloads the pane, so the
    /// staged hunks drop out of the unstaged view within a second.
    fn stage_seen_hunks(&mut self, cx: &mut Cx) {
        self.view.view(cx, ids!(share_panel)).set_visible(cx, false);
        let (worktree, workdir, files, git_dir) = self.state().read(|d| {
            let worktree = d.worktree_review();
            (worktree, d.root.clone(), d.stage.clone(), d.git_dir.clone())
        });
        let (true, Some(git_dir)) = (worktree, git_dir) else {
            return;
        };
        // Rewriting the index is git I/O: the service does it and posts the
        // report back for the status bar.
        review().send(ReviewCmd::StageSeen {
            window: self.state().id,
            git_dir,
            workdir,
            files,
        });
        self.view
            .label(cx, ids!(status_label))
            .set_text(cx, "staging seen hunks…");
        self.view.redraw(cx);
    }

    fn delete_comment(&mut self, cx: &mut Cx, id: u64) {
        let Some(git_dir) = self.state().read(|d| d.git_dir.clone()) else {
            return;
        };
        review().send(ReviewCmd::DeleteComment { git_dir, id });
        self.redraw_streams(cx);
    }

    /// Open the composer on a comment's thread. Replying to a reply answers the
    /// thread, GitHub-style — the store normalizes it the same way — so the
    /// row's `parent` is the target whenever it has one.
    // NOTE: sets the compose the way every gesture does; splice_composer drops one it cannot place.
    // coland-ignore: @coland/mixed_abstraction_levels
    fn reply_to_comment(&mut self, cx: &mut Cx, tab: Stream, id: u64, near: usize) {
        // The gesture claims its stream first, like a gutter press does:
        // `d.tab` routes the composer into a stream.
        self.set_gesture_tab(cx, tab);
        {
            let mut docs = self.state().write();
            let d = Arc::make_mut(&mut docs);
            strip_composer(d);
            let Some(root) = d.stream(tab).iter().find_map(|row| match row {
                Row::Comment {
                    id: current,
                    parent,
                    ..
                } if *current == id => Some(parent.unwrap_or(id)),
                _ => None,
            }) else {
                return;
            };
            d.compose = Some(Composing::Reply(root));
            d.compose_anchor = near;
            // An abandoned draft belongs to the comment it was being written
            // on, not to this thread.
            self.state().compose_draft.write().unwrap().clear();
            splice_composer(d);
            d.compose_focus = true;
        }
        self.redraw_streams(cx);
    }

    /// A press on a gutter starts a comment selection, GitHub-style: the
    /// pressed line alone, extended by dragging. If the composer is already
    /// open on the same file, a further click widens its range instead —
    /// including onto the other side of a deleted→added boundary.
    // NOTE: sets the compose the way every gesture does; splice_composer drops one it cannot place.
    // coland-ignore: @coland/mixed_abstraction_levels
    fn compose_start(&mut self, cx: &mut Cx, tab: Stream, item_id: usize, blob: u32, line: u32) {
        // The gesture claims its stream first: `d.tab` routes the composer,
        // and with two streams visible in a split it must follow the pane the
        // drag actually started in (closing a composer left in another one).
        self.set_gesture_tab(cx, tab);
        {
            let mut docs = self.state().write();
            let d = Arc::make_mut(&mut docs);
            let Some(item_id) = code_row_near(d.active(), blob, line, item_id) else {
                return;
            };
            let kind = match d.active().get(item_id) {
                Some(Row::Code { kind, .. }) => *kind,
                _ => return,
            };
            if d.composer_tab == Some(d.tab)
                && let Some(compose) = widen_compose(d, kind, blob, line)
            {
                d.compose = Some(Composing::Lines(compose));
                splice_composer(d);
            } else {
                strip_composer(d);
                let Some(item_id) = code_row_near(d.active(), blob, line, item_id) else {
                    return;
                };
                let side = Some(Side {
                    blob,
                    start: line,
                    end: line,
                });
                let (old, new) = match kind {
                    LineKind::Del => (side, None),
                    _ => (None, side),
                };
                d.compose = Some(Composing::Lines(Compose {
                    old,
                    new,
                    span: None,
                }));
                d.compose_anchor = item_id;
                self.state().compose_draft.write().unwrap().clear();
            }
        }
        self.redraw_streams(cx);
    }

    /// The drag extends the selection from the anchor row across contiguous
    /// code rows — deleted and added lines both, so a range can cross the
    /// del→add boundary of a hunk. Anything that isn't code (a skipped run,
    /// another file's header, a posted comment) ends the walk.
    fn compose_drag(&mut self, cx: &mut Cx, tab: Stream, y: f64) {
        // Which row the pointer is over is a question about pixels, so the list
        // that drew them answers it. It knows the band each row occupies; a row
        // delta cannot, once a wrapped line is taller than its neighbours.
        let Some(target) = self
            .view
            .dock(cx, ids!(dock))
            .item(stream_tab_spec(tab).id)
            .borrow::<ReviewList>()
            .and_then(|l| l.row_at_y(y))
        else {
            return;
        };
        {
            let mut docs = self.state().write();
            let d = Arc::make_mut(&mut docs);
            // A reply has no range to drag; only a line selection grows.
            if stream_has_composer(d.active()) || !matches!(d.compose, Some(Composing::Lines(_))) {
                return;
            }
            let rows = d.active();
            let anchor = d.compose_anchor;
            if !matches!(rows.get(target), Some(Row::Code { .. })) {
                return;
            }
            let (lo, hi) = (anchor.min(target), anchor.max(target));
            if let Some(c) = derive_compose(rows, lo, hi) {
                d.compose = Some(Composing::Lines(c));
            }
        }
        self.redraw_streams(cx);
    }

    /// The chip beside a text selection: comment on the lines it covers, and on
    /// the words themselves.
    ///
    /// The lines stay the anchor — that is what places the thread, exports to a
    /// pull request and survives a reload — and the selection rides along as
    /// the quote, so a comment about one call in a line does not have to say
    /// which by hand.
    // NOTE: sets the compose the way every gesture does; splice_composer drops one it cannot place.
    // coland-ignore: @coland/mixed_abstraction_levels
    fn compose_span(&mut self, cx: &mut Cx, tab: Stream, near: usize) {
        self.set_gesture_tab(cx, tab);
        {
            let mut docs = self.state().write();
            let d = Arc::make_mut(&mut docs);
            let (Some(head), Some(tail)) = (d.caret, d.selection_anchor) else {
                return;
            };
            // The rows the selection covers, folded into the two sides the way
            // a gutter drag folds them: deletions anchor on the old blob, the
            // rest on the new one, and a selection running across a hunk's
            // boundary carries both.
            let Some(compose) = selected(d.active(), Some(head), Some(tail))
                .and_then(|at| derive_compose(d.active(), at.from_row, at.to_row))
            else {
                return;
            };
            d.compose = Some(Composing::Lines(Compose {
                span: Some((head, tail)),
                ..compose
            }));
            d.compose_anchor = near;
            self.state().compose_draft.write().unwrap().clear();
            splice_composer(d);
            d.compose_focus = true;
        }
        self.redraw_streams(cx);
    }

    /// Release: open the inline composer below the selection, in the stream the
    /// drag started in.
    ///
    /// The release names that stream rather than reading whichever one is
    /// current, so the composer opens where the reviewer dragged even if
    /// something moved the gesture in between — `compose_anchor` is a row of
    /// that stream and means nothing in another one.
    fn compose_open(&mut self, cx: &mut Cx, tab: Stream) {
        {
            let mut docs = self.state().write();
            let d = Arc::make_mut(&mut docs);
            d.tab = tab;
            if d.compose.is_none() || stream_has_composer(d.active()) {
                return;
            }
            splice_composer(d);
            d.compose_focus = true;
        }
        self.redraw_streams(cx);
    }

    fn post_comment(&mut self, cx: &mut Cx) {
        {
            let mut docs = self.state().write();
            let d = Arc::make_mut(&mut docs);
            let body = self
                .state()
                .compose_draft
                .read()
                .unwrap()
                .trim()
                .to_string();
            if body.is_empty() {
                return;
            }
            let Some(c) = d.compose else {
                return;
            };
            drop_composer(self.state(), d);
            let Some(git_dir) = d.git_dir.clone() else {
                return;
            };
            // The write, the `git config` read for the author, and the reply
            // all happen off this thread; the row splices in when the service
            // publishes.
            let cmd = match c {
                // A reply takes its thread root's anchor, so there is nothing
                // here to resolve — the store copies it.
                Composing::Reply(parent) => ReviewCmd::ReplyComment {
                    git_dir,
                    parent,
                    body,
                },
                Composing::Lines(c) => {
                    let Some(primary) = comment_anchor(c) else {
                        return;
                    };
                    // What the selection named, read now rather than stored as
                    // a range: the comment says which words of its lines it is
                    // about, and says it in the words themselves.
                    let quote = c.span.and_then(|span| quoted(d, d.tab, span, primary));
                    // A worktree file gets the comment's lines as cursors in
                    // its document, minted here where the buffer is, so the
                    // comment rides every edit from the start.
                    let buffer = &mut d.blobs[primary.blob as usize];
                    let cursors = buffer
                        .editable()
                        .then(|| buffer.cursors_at(primary.start, primary.end))
                        .flatten();
                    let path = blob_label(d, primary.blob);
                    let side_anchor = |s: Side| store::Anchor {
                        blob: d.blobs[s.blob as usize].oid,
                        start: s.start,
                        end: s.end,
                    };
                    ReviewCmd::AddComment {
                        git_dir,
                        comment: store::Comment {
                            body,
                            quote,
                            cursors,
                            ..store::comment(path, side_anchor(primary))
                        },
                    }
                }
            };
            review().send(cmd);
        }
        self.focus_active_list(cx);
        self.redraw_streams(cx);
    }

    fn close_composer(&mut self, cx: &mut Cx) {
        self.state().with(|d| drop_composer(self.state(), d));
        self.focus_active_list(cx);
        self.redraw_streams(cx);
    }

    fn focus_active_list(&mut self, cx: &mut Cx) {
        let tab = self.state().read(|d| d.tab);
        let list = self
            .view
            .dock(cx, ids!(dock))
            .item(stream_tab_spec(tab).id)
            .portal_list(cx, ids!(list));
        cx.set_key_focus(list.area());
    }

    /// Spawn this tab's shell in the loaded repo. A running session is never
    /// restarted — opening is idempotent.
    ///
    /// The shell needs nothing else: the window answers on a pipe in the
    /// checkout's root, so `concats manifest` (or an agent running the
    /// review-guide skill) anywhere in it targets the diff on screen with no
    /// flags, even after the reviewer switches ranges.
    pub fn open_terminal(&mut self, cx: &mut Cx, tab: LiveId) {
        let repo = self.state().read(|d| d.repo.clone());
        self.open_terminal_for(cx, tab, &repo);
    }

    fn open_terminal_for(&mut self, cx: &mut Cx, tab: LiveId, repo: &str) {
        let cwd = if repo.is_empty() { "." } else { repo };
        terminal::open(
            terminal::Session {
                window: self.state().id,
                tab,
            },
            std::path::Path::new(cwd),
        );
        self.view.redraw(cx);
    }

    /// The status-bar `+`: another shell session in a new closable tab,
    /// created next to the other terminals, selected, and revealed.
    fn add_terminal_session(&mut self, cx: &mut Cx) {
        let dock = self.view.dock(cx, ids!(dock));
        let tab_id = dock.unique_id(terminal_tab_spec().id.0);
        let n = self.next_terminal.max(2);
        self.next_terminal = n + 1;
        let spec = TabSpec {
            id: tab_id,
            title: &format!("Terminal {n}"),
            ..terminal_tab_spec()
        };
        self.open_view(
            cx,
            spec,
            &terminal_tabs(&dock),
            crate::theme::active_placement().terminal,
        );
        self.open_terminal(cx, tab_id);
        self.state().set_terminal_tab(Some(tab_id));
    }

    /// A status-bar view button: reopen the stream's tab if it was closed,
    /// and jump to it either way.
    fn open_stream_tab(&mut self, cx: &mut Cx, tab: Stream) {
        let spec = stream_tab_spec(tab);
        self.user_closed.remove(&spec.id);
        let area = stream_area(&crate::theme::active_placement(), tab);
        self.open_view(cx, spec, &stream_tabs_in(area), area);
        self.set_gesture_tab(cx, tab);
        self.view.redraw(cx);
    }

    /// A stream with nothing to show has no tab and no view button, and a tab
    /// the user closed stays closed until its button reopens it. The rest of
    /// the user's layout is kept.
    pub(crate) fn sync_stream_tabs(&mut self, cx: &mut Cx, d: &ReviewDoc) {
        let dock = self.view.dock(cx, ids!(dock));
        let placement = crate::theme::active_placement();
        for (button, stream) in VIEW_BUTTONS {
            let available = stream == Stream::Files || !d.stream(stream).is_empty();
            let spec = stream_tab_spec(stream);
            if available && !self.user_closed.contains(&spec.id) {
                let area = stream_area(&placement, stream);
                open_tab(cx, &dock, spec, &stream_tabs_in(area), area_bar(area));
            } else if dock.find_tab_bar_of_tab(spec.id).is_some() {
                dock.close_tab(cx, spec.id);
            }
            self.view.button(cx, button).set_visible(cx, available);
        }
    }

    /// Open the bottom panel with the default terminal selected and its
    /// shell running. The screenshot path (`CONCATS_APP_TERM=1`) — rides
    /// the same slide animation as the toggle, so the shot also proves the
    /// slide completed.
    pub fn reveal_terminal(&mut self, cx: &mut Cx) {
        let dock = self.view.dock(cx, ids!(dock));
        let spec = terminal_tab_spec();
        self.open_view(
            cx,
            spec,
            &terminal_tabs(&dock),
            crate::theme::active_placement().terminal,
        );
        self.open_terminal(cx, spec.id);
        self.state().set_terminal_tab(Some(spec.id));
    }

    /// A panel's current size, off its splitter's position. Each panel's
    /// splitter spans the dock along its axis, so a `b` side is the dock's
    /// extent past that position and the left panel, an `a` side, is the
    /// position itself.
    fn panel_size(&mut self, cx: &mut Cx, panel: Panel) -> Option<f64> {
        let dock = self.view.dock(cx, ids!(dock));
        let rect = dock.area().rect(cx).size;
        let position = dock.splitter_position(panel.splitter())?;
        let size = match panel {
            Panel::Left => position,
            Panel::Right => rect.x - position,
            Panel::Bottom => rect.y - position,
        };
        Some(size.max(0.0))
    }

    /// Each panel's size, left, right and bottom.
    pub(crate) fn panel_sizes(&mut self, cx: &mut Cx) -> [f64; 3] {
        Panel::ALL.map(|panel| self.panel_size(cx, panel).unwrap_or_default())
    }

    /// Tint the drop highlight by the area under the dragged tab, so a move
    /// into a panel reads apart from a split within one: the center keeps the
    /// accent, each panel has a hue of its own. Only when the area changes:
    /// an apply restyles the dock, and a drag sends an event per pointer move.
    fn tint_drop(&mut self, cx: &mut Cx, abs: Vec2d) {
        let rect = self.view.dock(cx, ids!(dock)).area().rect(cx);
        let area = area_at(abs, rect, self.panel_sizes(cx));
        if self.drop_area == Some(area) {
            return;
        }
        self.drop_area = Some(area);
        let theme = crate::theme::active_theme();
        let hue = match area {
            concats_config::Area::Center => theme.accent,
            concats_config::Area::Left => theme.ansi[5],
            concats_config::Area::Right => theme.ansi[6],
            concats_config::Area::Bottom => theme.ansi[3],
        };
        let color = Vec4f {
            w: 0.25,
            ..crate::theme::paint(hue)
        };
        let mut dock = self.view.dock(cx, ids!(dock));
        script_apply_eval!(cx, dock, {
            drag_target_preview +: { color: #(color) }
        });
    }

    /// `save` marks the dock layout dirty — passed only on the final set of a
    /// slide, so an animation does not write a layout on every frame.
    fn set_panel_size(&mut self, cx: &mut Cx, panel: Panel, size: f64, save: bool) -> bool {
        self.view.dock(cx, ids!(dock)).set_splitter_align(
            cx,
            panel.splitter(),
            panel.align(size.max(0.0)),
            save,
        )
    }

    /// Where a panel's size is kept while it is shut.
    fn restore_slot(&mut self, panel: Panel) -> &mut f64 {
        match panel {
            Panel::Left => &mut self.left_restore,
            Panel::Right => &mut self.right_restore,
            Panel::Bottom => &mut self.bottom_restore,
        }
    }

    /// The size a panel reopens at: wherever the user last dragged it.
    fn restore_size(&mut self, panel: Panel) -> f64 {
        let last = *self.restore_slot(panel);
        let default = if panel == Panel::Bottom { 220.0 } else { 260.0 };
        if last > 1.0 { last } else { default }
    }

    fn start_slide(&mut self, cx: &mut Cx, panel: Panel, to: f64) {
        let from = self.panel_size(cx, panel).unwrap_or(to);
        self.slide = Some(Slide {
            panel,
            from,
            to: to.max(0.0),
            start: None,
        });
        self.slide_next_frame = cx.new_next_frame();
    }

    fn step_slide(&mut self, cx: &mut Cx, time: f64) {
        let Some(slide) = self.slide.as_mut() else {
            return;
        };
        // Studio's panel slide: 0.16s, ease-out cubic.
        let start = *slide.start.get_or_insert(time);
        let progress = ((time - start).max(0.0) / 0.16).min(1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        let size = slide.from + (slide.to - slide.from) * eased;
        let (panel, target, done) = (slide.panel, slide.to, progress >= 1.0);
        if !self.set_panel_size(cx, panel, size, false) {
            self.slide = None;
            return;
        }
        if done {
            self.slide = None;
            // The final set marks the layout dirty — studio persists the
            // panel size out-of-band, we ride the dock's needs_save.
            self.set_panel_size(cx, panel, target, true);
        } else {
            self.slide_next_frame = cx.new_next_frame();
        }
    }

    /// A panel button: slide the panel open with whatever it holds, empty or
    /// not, or shut it. A terminal it opens onto starts its shell, as
    /// pressing that tab does.
    fn toggle_panel(&mut self, cx: &mut Cx, panel: Panel) {
        let Some(current) = self.panel_size(cx, panel) else {
            return;
        };
        if current > 1.0 {
            *self.restore_slot(panel) = current;
            return self.start_slide(cx, panel, 0.0);
        }
        let dock = self.view.dock(cx, ids!(dock));
        let shown = dock
            .clone_state()
            .and_then(|items| Some(shown_tabs(&items, panel.side(&items)?)))
            .unwrap_or_default();
        for tab in shown
            .into_iter()
            .filter(|tab| terminal_tabs(&dock).contains(tab))
        {
            self.open_terminal(cx, tab);
        }
        let to = self.restore_size(panel);
        self.start_slide(cx, panel, to);
    }

    /// Slide open the panel `tab` sits in, if it is in a shut one: a terminal
    /// the user keeps in the center does not need the bottom panel.
    fn reveal_tab(&mut self, cx: &mut Cx, tab: LiveId) {
        let Some(items) = self.view.dock(cx, ids!(dock)).clone_state() else {
            return;
        };
        let Some(panel) = Panel::ALL.into_iter().find(|panel| {
            panel
                .side(&items)
                .is_some_and(|side| holds(&items, side, tab))
        }) else {
            return;
        };
        if self.panel_size(cx, panel).is_none_or(|size| size <= 1.0) {
            let to = self.restore_size(panel);
            self.start_slide(cx, panel, to);
        }
    }

    /// The dock: a tab press routes the gesture/composer stream there; the
    /// drag plumbing hands the event straight back to the dock, which does
    /// all the split/merge/reorder work internally. The terminal view's and
    /// the file browser's actions ride the same widget-action pass.
    fn handle_dock_actions(&mut self, cx: &mut Cx, actions: &[&WidgetAction]) {
        let dock = self.view.dock(cx, ids!(dock));
        for wa in actions {
            match wa.cast() {
                DockAction::TabWasPressed(tab_id) => self.tab_pressed(cx, tab_id),
                DockAction::TabCloseWasPressed(tab_id) => self.close_tab(cx, tab_id),
                DockAction::ShouldTabStartDrag(tab_id) => {
                    dock.tab_start_drag(
                        cx,
                        tab_id,
                        DragItem::FilePath {
                            path: String::new(),
                            internal_id: Some(tab_id),
                        },
                    );
                }
                DockAction::Drag(e) => {
                    if drag_source_tab_id(e.items.as_ref()).is_some() {
                        self.tint_drop(cx, e.abs);
                        dock.accept_drag(cx, e, DragResponse::Move);
                    }
                }
                DockAction::Drop(e) => {
                    if let Some(src) = drag_source_tab_id(e.items.as_ref()) {
                        dock.drop_move(cx, e.abs, src);
                    }
                    // The next drag tints afresh, in whatever theme is on.
                    self.drop_area = None;
                }
                _ => {}
            }
            match wa.cast() {
                TerminalViewAction::OpenFile(path, at) => self.open_file_tab(cx, &path, at),
                TerminalViewAction::None => {}
            }
            match wa.cast() {
                ReviewListAction::SaveAs(tab) => self.save_as(cx, tab),
                ReviewListAction::None => {}
            }
            // The file browser names a path; the pane owns the dock, so
            // turning that into a tab is this side's job.
            match wa.cast() {
                FileBrowserAction::OpenFile(path) => self.open_file_tab(cx, &path, None),
                FileBrowserAction::None => {}
            }
        }
    }

    /// The chrome around the dock: the panel toggles, the per-stream view
    /// buttons, the Share menu, and the settings gear.
    #[expect(
        clippy::cognitive_complexity,
        reason = "Each chrome control has an independent action in this dispatch boundary."
    )]
    fn handle_chrome_buttons(&mut self, cx: &mut Cx, actions: &Actions) {
        // The terminal button opens the panel on the shell; the panel button
        // next to the settings gear is the plain show/hide.
        if self.view.button(cx, ids!(terminal_button)).clicked(actions) {
            self.reveal_terminal(cx);
        }
        for (button, panel) in [
            (ids!(left_panel_button), Panel::Left),
            (ids!(bottom_panel_button), Panel::Bottom),
            (ids!(right_panel_button), Panel::Right),
        ] {
            if self.view.button(cx, button).clicked(actions) {
                self.toggle_panel(cx, panel);
            }
        }
        if self.view.button(cx, ids!(browser_button)).clicked(actions) {
            self.open_view(
                cx,
                browser_tab_spec(),
                &[],
                crate::theme::active_placement().browser,
            );
        }
        if self
            .view
            .button(cx, ids!(terminal_add_button))
            .clicked(actions)
        {
            self.add_terminal_session(cx);
        }
        // The view buttons: reopen (or jump to) each stream's tab.
        for (btn, tab) in VIEW_BUTTONS {
            if self.view.button(cx, btn).clicked(actions) {
                self.open_stream_tab(cx, tab);
            }
        }

        if self.view.button(cx, ids!(share_button)).clicked(actions) {
            let panel = self.view.view(cx, ids!(share_panel));
            let open = panel.visible();
            if !open {
                // Staging only exists where there is a worktree to stage from.
                let worktree = self.state().read(ReviewDoc::worktree_review);
                self.view
                    .button(cx, ids!(share_stage))
                    .set_visible(cx, worktree);
            }
            panel.set_visible(cx, !open);
            self.view.redraw(cx);
        }
        if self.view.button(cx, ids!(share_prompt)).clicked(actions) {
            self.share_comments(cx, |loaded, entries| {
                interchange::render_prompt(&comments::old_rev(loaded.base), entries)
            });
        }
        if self.view.button(cx, ids!(share_json)).clicked(actions) {
            self.share_comments(cx, |loaded, entries| {
                interchange::render(&comments::meta(loaded), entries)
            });
        }
        if self.view.button(cx, ids!(share_stage)).clicked(actions) {
            self.stage_seen_hunks(cx);
        }

        // Settings: a TOML editor over the shared config. The toggle opens it with
        // the current config and a hint listing theme names; Apply parses the
        // TOML, switches theme, persists, and re-themes live — the Rust-side
        // colors (they read active_theme() on draw), the DSL chrome (via
        // request_live_edit re-running script_mod), and the open terminals.
        if self.view.button(cx, ids!(settings_button)).clicked(actions) {
            self.open_settings_tab(cx);
        }
    }

    /// Per-item actions from the virtualized lists — one list per dock tab:
    /// the viewed tick box on a file card, a comment's delete button, the
    /// gutter's comment gestures, and the inline composer's controls.
    #[expect(
        clippy::cognitive_complexity,
        reason = "Row actions are dispatched against the current document in action order."
    )]
    fn handle_item_actions(&mut self, cx: &mut Cx, actions: &[&WidgetAction]) {
        for action in actions {
            let Some(target) = action
                .data
                .as_ref()
                .and_then(|data| data.downcast_ref::<ReviewItemAction>())
            else {
                continue;
            };
            let clicked = action
                .action
                .downcast_ref::<ButtonAction>()
                .is_some_and(|action| matches!(action, ButtonAction::Clicked(_)));
            match target {
                ReviewItemAction::Seen { tab, path, near }
                    if action
                        .action
                        .downcast_ref::<CheckBoxAction>()
                        .is_some_and(|action| matches!(action, CheckBoxAction::Change(_))) =>
                {
                    self.toggle_card_seen(cx, *tab, path, *near);
                }
                ReviewItemAction::Fold { path, seen } if clicked => {
                    self.toggle_card_fold(cx, path, *seen);
                }
                ReviewItemAction::Delete { id } if clicked => {
                    self.delete_comment(cx, *id);
                }
                ReviewItemAction::Reply { tab, id, near } if clicked => {
                    self.reply_to_comment(cx, *tab, *id, *near);
                }
                ReviewItemAction::Quote { tab, near } if clicked => {
                    self.compose_span(cx, *tab, *near);
                }
                ReviewItemAction::Outdated { path } if clicked => {
                    self.toggle_card_outdated(cx, path);
                }
                ReviewItemAction::More { id } if clicked => {
                    self.state().with(|d| {
                        if !d.expanded_comments.remove(id) {
                            d.expanded_comments.insert(*id);
                        }
                    });
                    self.redraw_streams(cx);
                }
                ReviewItemAction::Replies { root } if clicked => {
                    self.state().with(|d| d.expanded_threads.insert(*root));
                    self.redraw_streams(cx);
                }
                ReviewItemAction::OpenFile { path, thread } if clicked => {
                    self.open_file_tab(cx, path, None);
                    // Looked for once the tab is open: its file is where the
                    // thread's content sits now, wherever the range put it.
                    if let Some(root) = *thread {
                        let git_dir = self.state().read(|d| d.git_dir.clone());
                        let review = self.review_snapshot(git_dir.as_deref());
                        let tab = file_tab_id(path).0;
                        let row = self
                            .state()
                            .with(|d| caret_to_thread(d, tab, &review.comments, root));
                        *self.state().reveal.lock().unwrap() =
                            row.map(|row| (Stream::File(tab), row));
                    }
                }
                ReviewItemAction::Gutter { tab, row } => match action.action.downcast_ref() {
                    Some(GutterAction::DragStart { blob, line }) => {
                        self.compose_start(cx, *tab, *row, *blob, *line);
                    }
                    Some(GutterAction::DragTo { y }) => self.compose_drag(cx, *tab, *y),
                    Some(GutterAction::DragEnd) => self.compose_open(cx, *tab),
                    _ => {}
                },
                // A chevron on a collapsed run: the band's action says which end.
                // Only this stream's copy of the run opens: the same file is a
                // card in several streams, and each one is its own reading
                // surface.
                ReviewItemAction::Expand { tab, row } => {
                    if let Some(end) = action.action.downcast_ref::<CollapsedEnd>() {
                        self.state().with(|d| expand_collapsed(d, *tab, *row, *end));
                        self.redraw_streams(cx);
                    }
                }
                // "N lines removed": put them back where they were taken from.
                ReviewItemAction::Reveal { tab, row } if clicked => {
                    self.state().with(|d| reveal_removed(d, *tab, *row));
                    self.redraw_streams(cx);
                }
                ReviewItemAction::Post if clicked => {
                    self.post_comment(cx);
                }
                ReviewItemAction::Cancel if clicked => {
                    self.close_composer(cx);
                }
                // The guard above did not hold: not a click, or not the tick.
                ReviewItemAction::Seen { .. }
                | ReviewItemAction::Fold { .. }
                | ReviewItemAction::Delete { .. }
                | ReviewItemAction::Reply { .. }
                | ReviewItemAction::Quote { .. }
                | ReviewItemAction::Outdated { .. }
                | ReviewItemAction::OpenFile { .. }
                | ReviewItemAction::More { .. }
                | ReviewItemAction::Replies { .. }
                | ReviewItemAction::Reveal { .. }
                | ReviewItemAction::Post
                | ReviewItemAction::Cancel => {}
            }
        }
    }

    fn handle_composer_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions
            .iter()
            .filter_map(|action| action.as_widget_action())
        {
            let Some(
                text @ (TextInputAction::Changed(_)
                | TextInputAction::Returned(..)
                | TextInputAction::Escaped),
            ) = action.action.downcast_ref::<TextInputAction>()
            else {
                continue;
            };
            // The composer's field, matched by uid rather than by the action
            // data every other control in a row carries — see
            // `ReviewList::composer_input`. Only the stream holding the
            // composer row has one, and it is asked per keystroke: once this
            // batch has posted or closed the composer, what its field still
            // reports is stale and must not refill the cleared draft.
            let input = self.state().read(|d| d.composer_tab).and_then(|tab| {
                self.view
                    .dock(cx, ids!(dock))
                    .item(stream_tab_spec(tab).id)
                    .borrow::<ReviewList>()
                    .and_then(|list| list.composer_input)
            });
            if input != Some(action.widget_uid) {
                continue;
            }
            let (TextInputAction::Changed(draft) | TextInputAction::Returned(draft, _)) = text
            else {
                // Escape cancels, like the Cancel button.
                self.close_composer(cx);
                continue;
            };
            // The draft is mirrored on every keystroke because the virtualized
            // list can recycle the field out from under it; without that
            // mirror, posting reads an empty body.
            self.state()
                .compose_draft
                .write()
                .unwrap()
                .clone_from(draft);
            if let TextInputAction::Returned(..) = text {
                self.post_comment(cx);
            }
        }
    }
}

/// The window a pointer event names, for the events that name one.
// NOTE: the pointer half of handle_event's window gate, beside is_keyboard_input.
// coland-ignore: @coland/single_caller_private_function
fn pointer_window(event: &Event) -> Option<WindowId> {
    Some(match event {
        Event::MouseDown(e) => e.window_id,
        Event::MouseMove(e) => e.window_id,
        Event::MouseUp(e) => e.window_id,
        Event::Scroll(e) => e.window_id,
        Event::TouchUpdate(e) => e.window_id,
        _ => return None,
    })
}

/// Events that reach a widget by key focus rather than by position. Mouse and
/// touch carry a `window_id` and makepad's hit test filters on it; these do
/// not, so the window has to.
// NOTE: the keyboard half of handle_event's window gate, beside pointer_window.
// coland-ignore: @coland/single_caller_private_function
fn is_keyboard_input(event: &Event) -> bool {
    matches!(
        event,
        Event::KeyDown(_)
            | Event::KeyUp(_)
            | Event::TextInput(_)
            | Event::TextRangeReplace(_)
            | Event::TextCopy(_)
            | Event::TextCut(_)
            | Event::ImeAction(_)
    )
}

/// Close the composer: no target, no draft, no row, no keyboard.
fn drop_composer(state: &WindowState, d: &mut ReviewDoc) {
    d.compose = None;
    state.compose_draft.write().unwrap().clear();
    strip_composer(d);
    d.compose_focus = false;
}

impl Widget for ReviewPane {
    /// Where a window's scope begins. `Root` hands every window the same one,
    /// so the rows below are told which document they are drawn from here —
    /// the pane is the first widget that belongs to exactly one window.
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let state = self.state().clone();
        // NOTE: Only the list that draws the input can acknowledge focus.
        let focus_composer = state.read(|d| d.composer_tab.filter(|_| d.compose_focus));
        let document = state.snapshot();
        if self.file_titles_generation != document.generation
            || self.file_titles_rows_rev != document.rows_rev
        {
            let dock = self.view.dock(cx, ids!(dock));
            for file in &document.files_open {
                if file.tab == settings_tab_spec().id.0 {
                    continue;
                }
                let title = crate::file_view::file_tab_title(&document, file);
                dock.set_tab_title(cx, LiveId(file.tab), title);
            }
            self.file_titles_generation = document.generation;
            self.file_titles_rows_rev = document.rows_rev;
        }
        // The browser's title names the revision of its tree, oid included:
        // a branch name moves.
        // NOTE: Every draw, because a restored layout brings back the title it
        // was saved with.
        if document.generation > 0 {
            let spec = browser_tab_spec();
            let title = match document.resolved.1 {
                // A directory without Git has no revision to name.
                _ if document.without_git => spec.title.to_string(),
                Rev::Commit(oid) => format!(
                    "{} · {} · {}",
                    spec.title,
                    document.head,
                    &oid.to_string()[..7]
                ),
                _ => format!("{} · {}", spec.title, document.head),
            };
            self.view
                .dock(cx, ids!(dock))
                .set_tab_title(cx, spec.id, title);
        }
        let compose_draft = state.compose_draft.read().unwrap().clone();
        let mut frame = FrameData {
            review: self.review_snapshot(document.git_dir.as_deref()),
            theme: crate::theme::active_theme(),
            focus_composer,
            compose_draft,
            document,
            state,
        };
        let step = self
            .view
            .draw_walk(cx, &mut Scope::with_data(&mut frame), walk);
        // A dropdown opened since the last frame has an input to focus only
        // once it is drawn.
        if std::mem::take(&mut self.focus_combo) {
            self.view
                .text_input(cx, ids!(combo_input))
                .set_key_focus(cx);
        }
        step
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        // A keystroke names no window: makepad routes the keyboard through one
        // global key focus, so `Root` hands every window's widgets every key.
        // The rows below claim keys before that focus gate — the caret needs
        // the arrows, Home and Cmd-A that `PortalList` would otherwise take —
        // so without this a keystroke edits, and a Cmd-A selects, in every
        // window at once. A window that does not have the OS focus drops them.
        if is_keyboard_input(event) && !self.state().is_focused() {
            return;
        }
        // A pointer names its window, but two windows overlap in window-local
        // coordinates, so the same press lands on both — and whichever runs
        // last takes the key focus, which is how a click in one window sent
        // the next keystroke to the other one's shell. Makepad's hit test
        // keeps them apart with the `handled` flag, but a widget that tests
        // raw coordinates never consults it.
        if let Some(window_id) = pointer_window(event)
            && self.state().platform != Some(window_id)
        {
            return;
        }
        // An open picker walks its rows by key before anything focused does.
        if let Event::KeyDown(ke) = event
            && self.picker_key(cx, ke)
        {
            return;
        }
        if let Event::NextFrame(ne) = event
            && self.slide_next_frame.is_event(event).is_some()
        {
            self.step_slide(cx, ne.time);
        }
        // Only which window, not the whole frame: this runs on every mouse
        // move, and rebuilding the rest of `FrameData` there would cost a
        // registry lookup per event for something no handler reads.
        let mut window = WindowScope(self.state().clone());
        let scope = &mut Scope::with_data(&mut window);
        self.view.handle_event(cx, event, scope);
        self.widget_match_event(cx, event, scope);
    }
}

impl WidgetMatchEvent for ReviewPane {
    /// Routed by concern. The blocks are independent — each reads the actions
    /// it recognizes out of the same batch — so the order is presentation, not
    /// protocol, except that a load must be handled before the widgets it
    /// rebuilds.
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, _scope: &mut Scope) {
        // NOTE: Action batches span windows; filter ownership once before dispatch.
        let window = self.state().id;
        let owned: Vec<_> = actions
            .iter()
            .filter_map(|action| action.as_widget_action())
            .filter(|action| {
                cx.widget_tree()
                    .path_to(action.widget_uid)
                    .contains(&window)
            })
            .collect();
        if owned.is_empty() {
            return;
        }
        self.handle_repo_picker(cx, actions);
        self.handle_diff_picker(cx, actions);
        self.handle_dock_actions(cx, &owned);
        self.handle_chrome_buttons(cx, actions);
        self.handle_item_actions(cx, &owned);
        self.handle_composer_actions(cx, actions);
    }
}

#[cfg(test)]
mod tests {
    use concats_config::Area;

    use super::*;

    #[test]
    fn a_point_is_in_the_area_whose_panel_it_is_over() {
        // A 1000×800 dock with a 200-wide left, 250-wide right and 150-tall
        // bottom panel, which spans the center column only.
        let rect = Rect {
            pos: dvec2(0.0, 30.0),
            size: dvec2(1000.0, 800.0),
        };
        let sizes = [200.0, 250.0, 150.0];
        for (x, y, area) in [
            (100.0, 400.0, Area::Left),
            (100.0, 800.0, Area::Left),
            (900.0, 800.0, Area::Right),
            (500.0, 750.0, Area::Bottom),
            (500.0, 400.0, Area::Center),
        ] {
            assert_eq!(area_at(dvec2(x, y), rect, sizes), area, "{x},{y}");
        }
        // Shut panels are nowhere.
        assert_eq!(area_at(dvec2(1.0, 829.0), rect, [0.0; 3]), Area::Center);
    }
}
