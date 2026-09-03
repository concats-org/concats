//! The review pane: one whole review — the caption/toolbar header, the diff
//! picker, the dock (Guide / Sessions / Commits / File Diff tabs plus the
//! bottom terminal panel) and the status bar — bound to one document. It owns
//! its own chrome actions and dock/gesture routing, so the App drives it
//! through a few public entry points (layout restore, the status-bar buttons,
//! the diff-picker hooks), and a second pane could sit next to it without the
//! App arbitrating widget lookups. It composes the `ReviewList` and terminal
//! widgets and turns the `Gutter`'s drag actions into the inline comment
//! composer.

use std::{collections::HashSet, sync::Arc};

use concats_diff::{CollapsedEnd, LineKind, Row, Side};
use concats_review::{interchange, store};

use super::{review_list::ReviewItemAction, FileBrowserAction, GutterAction, ReviewList, SeenBar};
use crate::{
    dock::{
        create_stream_tab, drag_source_tab_id, file_tab_id, is_terminal_dock_tab, model_tab_of,
        stream_tab_spec,
    },
    file_view::{open_file, read_file_sides},
    load::{resplice_comments, spawn_load},
    makepad_widgets::*,
    review_doc::{
        blob_label, card_keys, comment_anchor, derive_compose, expand_collapsed, reveal_removed,
        seen_progress, splice_composer, stream_has_composer, strip_composer, Compose, Composing,
        Tab,
    },
    service::{self, review, review_state, ReviewCmd},
    terminal,
    terminal_view::DesktopTerminalViewAction,
    window::WindowState,
    FrameData, WindowScope,
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.PerfGraph
    use mod.widgets.DesktopTerminalView
    use mod.widgets.FileBrowser
    use mod.widgets.ReviewList
    use mod.widgets.SeenBar
    use mod.widgets.FONT
    use mod.widgets.DarkInput
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
    // The settings tab; the `{ }` status-bar button opens it in the bottom
    // panel (no dedicated icon yet).
    let SettingsTab = ReviewTab {}
    // The permanent terminal tab is the panel's structural anchor (the `>_`
    // toggle needs the bottom splitter to survive) — not closable. Extra
    // sessions from the `+` button are.
    let TerminalTab = ReviewTab {
        closeable: false
        draw_icon +: { color: C_DIM svg: crate_resource("self:resources/icons/terminal_fill.svg") }
    }
    let TerminalCloseTab = TerminalTab { closeable: true }

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

    // One chrome icon button: 20x20 around a 12px glyph, dim until hovered.
    let BarButton = ButtonFlatter {
        width: 20 height: 20
        padding: 0
        margin: 0
        text: ""
        icon_walk: Walk{width: 12 height: Fit}
        draw_icon +: {
            color: C_DIM
            color_hover: C_TEXT
        }
    }

    // One row of a picker dropdown: a full-width, left-aligned entry. Every
    // text state is pinned to C_TEXT — the "Open dir…" row opens a blocking
    // modal that can leave it focused-but-not-hovered, where the theme's
    // `color_focus`/`_down` would otherwise render it invisible.
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
            // 26 + the hairline below = the design's 27pt header band; the tab
            // strip then starts at 27 and the content at 52, as in the frames.
            height: 26
            draw_bg.color: C_CHROME
            flow: Right
            spacing: 0
            align: Align{x: 0.0, y: 0.5}
            // 78 clears the native traffic lights (the design's 60 assumes
            // 10px dots; macOS draws bigger ones).
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
            Divider {}
            // The diff picker chip: click to open the autocomplete dropdown.
            // The App's WindowDragQuery handler carves this rect out of the
            // header's drag band so the click actually arrives.
            range_button := ButtonFlatter {
                width: Fit height: Fit
                padding: Inset{top: 1 bottom: 1 left: 4 right: 4}
                margin: 0
                text: "select a diff"
                icon_walk: Walk{width: 12 height: Fit}
                draw_icon +: {
                    svg: crate_resource("self:resources/icons/git_branch.svg")
                    color: C_DIM
                }
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
            // Sits right after the diff chip: click to reload the open repo
            // at its current range. Icon color is the theme's (C_DIM/C_TEXT),
            // so it tracks light/dark.
            load_button := ButtonFlatter {
                width: 20 height: 20
                padding: 0
                margin: Inset{left: 4}
                text: ""
                icon_walk: Walk{width: 12 height: Fit}
                draw_icon +: {
                    svg: crate_resource("self:resources/icons/refresh.svg")
                    color: C_DIM
                    color_hover: C_TEXT
                }
            }
            View { width: Fill height: Fit }
            // Share lives in the title bar, top right — per the design.
            share_button := ButtonFlatter {
                width: 20 height: 20
                padding: 0
                margin: 0
                text: ""
                icon_walk: Walk{width: 12 height: Fit}
                draw_icon +: {
                    // Brighter than the status bar's icons, like the design.
                    svg: crate_resource("self:resources/icons/share.svg")
                    color: C_TEXT
                    color_hover: C_TEXT
                }
            }
        }
        SolidView { width: Fill height: 1 draw_bg.color: C_BORDER }

        body_overlay := View {
            width: Fill
            height: Fill
            flow: Overlay

            // The dock owns the tab strip and every pane: tabs drag to
            // reorder, drop on an edge to split, drop on a bar to merge. The
            // bottom panel hosts the terminal and the right one the file
            // browser, each collapsed until toggled — both on the dock's own
            // splitters, so both slide and both drag by the same handle.
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
                    TerminalCloseTab := TerminalCloseTab{}
                    SettingsTab := SettingsTab{}
                }

                // The browser hangs off the outermost splitter so it spans the
                // terminal too, like an editor's side panel. Both panels
                // collapse to FromB(0).
                root := DockSplitter {
                    axis: SplitterAxis.Horizontal
                    // Open by default, unlike the terminal: the browser is how
                    // you reach a file, so hiding it hides the way in.
                    align: SplitterAlign.FromB(260.0)
                    a: @body_split
                    b: @sidebar_tabs
                }
                body_split := DockSplitter {
                    axis: SplitterAxis.Vertical
                    align: SplitterAlign.FromB(0.0)
                    a: @main_tabs
                    b: @bottom_tabs
                }
                // No tab strip: the browser carries its own header (the
                // rev chip), and a one-tab bar over it would just be a
                // second title.
                sidebar_tabs := DockTabs {
                    tabs: [@sidebar_tab]
                    selected: 0
                    closable: false
                    hide_tab_bar: true
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
                sidebar_tab := DockTab { name: "Files" template: @FilesTab kind: @SidebarPane }

                // One stream-pinned list per tab kind.
                GuidePane := ReviewList { kind: @review }
                SessionsPane := ReviewList { kind: @sessions }
                CommitsPane := ReviewList { kind: @commits }
                FilesPane := ReviewList { kind: @files }
                CommentsPane := ReviewList { kind: @comments }
                FilePane := ReviewList { kind: @file }
                TerminalPane := DesktopTerminalView {}
                // Opened on demand by the `{ }` status-bar button (not in the
                // default tab list) — like the `+`-created terminal sessions.
                SettingsPane := ReviewList { kind: @file }
                SidebarPane := FileBrowser {}
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

            // The diff picker: a floating panel right under the header chip.
            // A filter input over the repo's refs; Enter accepts a typed ref
            // or an explicit `base...head` range.
            combo_panel := RoundedView {
                visible: false
                width: 340 height: Fit
                margin: Inset{left: 78 top: 4}
                flow: Down
                spacing: 2
                padding: 4
                draw_bg.color: C_CARD
                draw_bg.border_radius: 2.0
                draw_bg.border_size: 1.0
                draw_bg.border_color: C_BORDER

                combo_input := DarkInput {
                    width: Fill
                    empty_text: "ref to diff against HEAD, or base...head"
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
            repo_panel := RoundedView {
                visible: false
                width: 300 height: Fit
                margin: Inset{left: 78 top: 4}
                flow: Down
                spacing: 2
                padding: 4
                draw_bg.color: C_CARD
                draw_bg.border_radius: 2.0
                draw_bg.border_size: 1.0
                draw_bg.border_color: C_BORDER

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
                RoundedView {
                    width: 150 height: Fit
                    flow: Down
                    spacing: 2
                    padding: 4
                    draw_bg.color: C_CARD
                    draw_bg.border_radius: 2.0
                    draw_bg.border_size: 1.0
                    draw_bg.border_color: C_BORDER
                    share_prompt := ComboRow { visible: true text: "Copy as prompt" }
                    share_md := ComboRow { visible: true text: "Copy as markdown" }
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
            // The settings editor: a JSON view of the app config.
            settings_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/settings.svg")
            }
            Divider {}
            // The bottom panel itself, whatever it currently holds.
            panel_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/panel_bottom.svg")
            }
            // The file browser on the right.
            sidebar_button := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/panel_right.svg")
            }
        }
    }
}

/// One of the pane's two sliding panels. Both are their splitter's `b`, both
/// collapse to `FromB(0)`, and both reopen to the size the user last dragged —
/// so they behave identically and share the machinery below.
#[derive(Clone, Copy, PartialEq)]
enum Panel {
    Bottom,
    Sidebar,
}

impl Panel {
    /// The dock splitter this panel rides. The sidebar is on the outermost one
    /// so it spans the bottom panel too.
    fn splitter(self) -> LiveId {
        match self {
            Panel::Bottom => id!(body_split),
            Panel::Sidebar => id!(root),
        }
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
    /// The panel slide in flight, stepped on NextFrame with studio's 0.16s
    /// ease-out cubic (app_backend.rs). One at a time: the two panels are
    /// toggled by two different buttons.
    #[rust]
    slide: Option<Slide>,
    #[rust]
    slide_next_frame: NextFrame,
    /// Sizes to restore each panel to on reopen (0 = never opened yet, so the
    /// default). Written by the App from the persisted layout on load.
    #[rust]
    pub bottom_restore: f64,
    #[rust]
    pub sidebar_restore: f64,
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
}

/// The fixed row slots of the diff-picker dropdown. A handful is plenty:
/// the filter narrows the list as you type.
macro_rules! combo_rows {
    () => {
        [
            ids!(combo_row0),
            ids!(combo_row1),
            ids!(combo_row2),
            ids!(combo_row3),
            ids!(combo_row4),
            ids!(combo_row5),
            ids!(combo_row6),
            ids!(combo_row7),
        ]
    };
}

/// The recent-repo row slots of the repo picker (capped to match recents::MAX).
macro_rules! repo_rows {
    () => {
        [
            ids!(repo_row0),
            ids!(repo_row1),
            ids!(repo_row2),
            ids!(repo_row3),
            ids!(repo_row4),
        ]
    };
}

impl ReviewPane {
    /// Take the window this pane renders. Called by the App as the window
    /// opens, before any event reaches the pane.
    pub(crate) fn adopt(&mut self, state: std::sync::Arc<WindowState>) {
        self.state = Some(state);
    }

    /// The window this pane renders. Before the App has adopted it — the first
    /// frames of a run — this answers with a detached empty document, which is
    /// what the pane would draw at that point anyway.
    fn state(&self) -> &std::sync::Arc<WindowState> {
        static DETACHED: std::sync::OnceLock<std::sync::Arc<WindowState>> =
            std::sync::OnceLock::new();
        match self.state.as_ref() {
            Some(state) => state,
            None => DETACHED.get_or_init(|| WindowState::new(LiveId(0))),
        }
    }

    /// Open the Settings dock tab — creating it next to the stream tabs if it
    /// isn't open, else selecting it. Shared by the `{ }` toolbar button and the
    /// `CONCATS_APP_SETTINGS` screenshot hook.
    pub fn open_settings_tab(&mut self, cx: &mut Cx) {
        self.state().with(crate::file_view::open_settings);
        let dock = self.view.dock(cx, ids!(dock));
        if dock.find_tab_bar_of_tab(id!(settings_tab)).is_none() {
            // Open next to the stream tabs (the bar they live in), else the main
            // tab area. The reconciliation only manages the four streams, so a
            // settings tab here is left alone.
            let bar = [
                id!(guide_tab),
                id!(sessions_tab),
                id!(commits_tab),
                id!(files_tab),
            ]
            .into_iter()
            .find_map(|t| dock.find_tab_bar_of_tab(t).map(|(b, _)| b))
            .unwrap_or(id!(main_tabs));
            dock.create_tab(
                cx,
                bar,
                id!(settings_tab),
                id!(SettingsPane),
                "Settings".into(),
                id!(SettingsTab),
                None,
            );
        }
        dock.select_tab(cx, id!(settings_tab));
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
    pub fn open_file_tab(&mut self, cx: &mut Cx, path: String) {
        let (repo, range) = self
            .state()
            .read(|d| (d.repo.clone(), (d.merge_base_oid, d.head_oid)));
        let sides = match read_file_sides(&repo, range, &path) {
            Ok(sides) => sides,
            Err(e) => {
                return self
                    .view
                    .label(cx, ids!(status_label))
                    .set_text(cx, &e.to_string());
            }
        };
        let git_dir = self.state().read(|d| d.git_dir.clone());
        let comments = review_state(git_dir.as_deref()).load().comments.clone();
        self.state().with(|d| open_file(d, &path, sides, &comments));

        let tab_id = file_tab_id(&path);
        let dock = self.view.dock(cx, ids!(dock));
        if dock.find_tab_bar_of_tab(tab_id).is_none() {
            // Beside the stream tabs, like the settings tab — the reconcile
            // only manages the four streams, so these are left alone.
            let bar = [
                id!(guide_tab),
                id!(sessions_tab),
                id!(commits_tab),
                id!(files_tab),
            ]
            .into_iter()
            .find_map(|t| dock.find_tab_bar_of_tab(t).map(|(b, _)| b))
            .unwrap_or(id!(main_tabs));
            let name = self
                .state()
                .read(|d| crate::file_view::file_tab_title(d, &path));
            dock.create_tab(cx, bar, tab_id, id!(FilePane), name, id!(FileTab), None);
        }
        dock.select_tab(cx, tab_id);
        self.set_gesture_tab(cx, Tab::File(tab_id.0));
        self.redraw_streams(cx);
    }

    /// Drop a closed file tab's stream. The tab is the file's identity, so a
    /// tab that is gone has no stream to keep — and leaving it would have the
    /// comment and composer passes walking rows nothing renders.
    fn close_file_tab(&mut self, cx: &mut Cx, tab_id: LiveId) {
        self.state().with(|d| {
            d.files_open.retain(|f| f.tab != tab_id.0);
            // The gesture cannot stay pointed at a stream that no longer
            // exists; the diff is where every range starts.
            if d.tab == Tab::File(tab_id.0) {
                d.tab = Tab::Files;
            }
        });
        self.view.dock(cx, ids!(dock)).close_tab(cx, tab_id);
        self.redraw_streams(cx);
    }

    /// Open the diff picker under the header chip: empty filter, refs listed,
    /// keyboard in the input.
    pub fn combo_open(&mut self, cx: &mut Cx) {
        self.repo_close(cx);
        self.view.view(cx, ids!(combo_panel)).set_visible(cx, true);
        let input = self.view.text_input(cx, ids!(combo_input));
        input.set_text(cx, "");
        input.set_key_focus(cx);
        self.combo_filter(cx, "");
        self.view.redraw(cx);
    }

    fn combo_close(&mut self, cx: &mut Cx) {
        self.view.view(cx, ids!(combo_panel)).set_visible(cx, false);
        self.view.redraw(cx);
    }

    /// Open the repo picker under the repo name: the recent repos as rows (most
    /// recent first, shown by basename), plus the always-present "Open dir…".
    pub fn repo_open(&mut self, cx: &mut Cx) {
        self.combo_close(cx);
        review().send(ReviewCmd::LoadRecents);
        self.show_recents(cx);
        self.view.view(cx, ids!(repo_panel)).set_visible(cx, true);
        self.view.redraw(cx);
    }

    pub fn set_recents(&mut self, cx: &mut Cx, recents: Vec<String>) {
        self.recents = recents;
        self.show_recents(cx);
        self.view.redraw(cx);
    }

    fn show_recents(&self, cx: &mut Cx) {
        for (i, ids) in repo_rows!().into_iter().enumerate() {
            let row = self.view.button(cx, ids);
            match self.recents.get(i) {
                Some(path) => {
                    let name = std::path::Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.clone());
                    row.set_text(cx, &name);
                    row.set_visible(cx, true);
                }
                None => row.set_visible(cx, false),
            }
        }
    }

    fn repo_close(&mut self, cx: &mut Cx) {
        self.view.view(cx, ids!(repo_panel)).set_visible(cx, false);
        self.view.redraw(cx);
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

    /// Fill the row slots with the refs matching the query.
    fn combo_filter(&mut self, cx: &mut Cx, query: &str) {
        let query = query.trim().to_lowercase();
        let rows = combo_rows!();
        let matches: Vec<String> = self.state().read(|d| {
            d.refs
                .iter()
                .filter(|r| query.is_empty() || r.to_lowercase().contains(&query))
                .take(rows.len())
                .cloned()
                .collect()
        });
        for (i, ids) in rows.into_iter().enumerate() {
            let row = self.view.button(cx, ids);
            match matches.get(i) {
                Some(name) => {
                    row.set_text(cx, name);
                    row.set_visible(cx, true);
                }
                None => row.set_visible(cx, false),
            }
        }
        self.view.redraw(cx);
    }

    /// Load `base…head` of `repo`, and close the diff picker.
    fn combo_load(&mut self, cx: &mut Cx, repo: String, base: String, head: String) {
        self.view
            .label(cx, ids!(status_label))
            .set_text(cx, "loading…");
        self.combo_close(cx);
        self.set_loading(cx, true);
        spawn_load(
            self.state(),
            concats_state::Target { repo, base, head },
            None,
        );
    }

    /// The range to (re)load with: whatever is open, else a sane default for a
    /// repo the app hasn't diffed yet.
    fn current_range(&self) -> (String, String) {
        self.state().read(|d| {
            if d.base.is_empty() {
                ("HEAD~5".into(), "HEAD".into())
            } else {
                (d.base.clone(), d.head.clone())
            }
        })
    }

    /// Open a native folder dialog under the repo name and load whatever repo
    /// the user picks, keeping the open range. Blocks on the modal panel —
    /// fine, we are on the main thread and there is nothing else to draw.
    fn pick_repo(&mut self, cx: &mut Cx) {
        let mut dialog = rfd::FileDialog::new().set_title("Choose a repository");
        // Start the browser next to the current repo, not the cwd.
        let current = self.state().read(|d| d.repo.clone());
        if let Some(parent) = std::path::Path::new(&current).parent() {
            if !parent.as_os_str().is_empty() {
                dialog = dialog.set_directory(parent);
            }
        }
        let picked = dialog.pick_folder();
        // The modal swallowed the row's mouse-up/hover-out; snap it back so the
        // "Open dir…" row doesn't reopen stuck in its pressed style.
        self.view.button(cx, ids!(open_dir_row)).reset_hover(cx);
        let Some(path) = picked else {
            return; // cancelled
        };
        let (base, head) = self.current_range();
        self.combo_load(cx, path.to_string_lossy().into_owned(), base, head);
    }

    /// Point the gesture/composer state at the given stream: pressing a dock
    /// tab or starting a gutter gesture in one of its panes moves it there.
    /// An open composer belongs to the stream it was opened in — leaving
    /// that stream closes it. (Scroll state lives per list instance now, so
    /// there is nothing to reset here.)
    pub fn set_gesture_tab(&mut self, cx: &mut Cx, tab: Tab) {
        let changed = self.state().with(|d| {
            if d.tab == tab {
                return false;
            }
            d.compose = None;
            d.compose_draft.clear();
            strip_composer(d);
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
    fn toggle_card_seen(&mut self, cx: &mut Cx, tab: Tab, item_id: usize) {
        let docs = self.state().snapshot();
        let d = &*docs;
        let keys = card_keys(d.stream(tab), item_id, &d.blobs);
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

    /// Repaint every stream's list. `Dock::redraw` only invalidates the dock's
    /// own area, never the widgets inside its tabs, so `self.view.redraw` stops
    /// short of the lists: a fold, a tick, or anything the comment gestures
    /// change would sit unpainted until something else — a scroll, a hover —
    /// invalidated the list itself. That is what made posting and cancelling a
    /// comment look like a hang, and a drag show no selection until you moved
    /// the pointer somewhere unrelated. Every mutation of a stream must come
    /// through here.
    fn redraw_streams(&mut self, cx: &mut Cx) {
        self.view.redraw(cx);
        let dock = self.view.dock(cx, ids!(dock));
        let files = self
            .state()
            .read(|d| d.files_open.iter().map(|f| f.tab).collect::<Vec<_>>());
        for tab in [
            Tab::Guide,
            Tab::Sessions,
            Tab::Commits,
            Tab::Files,
            Tab::Comments,
        ]
        .into_iter()
        .chain(files.into_iter().map(Tab::File))
        {
            dock.item(stream_tab_spec(tab).0).redraw(cx);
        }
    }

    /// Fold a file card shut (or open it again). Keyed by path, so the card
    /// is shut in every stream that renders that file — and the list rebuilds
    /// its entry→row mapping on the next draw.
    fn toggle_card_fold(&mut self, cx: &mut Cx, tab: Tab, item_id: usize) {
        self.state().with(|d| {
            let Some(Row::FileHeader { path, .. }) = d.stream(tab).get(item_id).cloned() else {
                return;
            };
            if !d.folded.remove(&path) {
                d.folded.insert(path);
            }
        });
        self.redraw_streams(cx);
    }

    /// Show or hide the conversations this range cannot place for a file.
    /// Unlike folding, this changes which comment rows exist, so it resplices
    /// — and announces the new shape, because every row-indexed cache below
    /// is stale the moment a row is inserted mid-stream.
    fn toggle_card_outdated(&mut self, cx: &mut Cx, tab: Tab, item_id: usize) {
        let git_dir = self.state().read(|d| d.git_dir.clone());
        let comments = review_state(git_dir.as_deref()).load().comments.clone();
        self.state().with(|d| {
            let Some(Row::FileHeader { path, .. }) = d.stream(tab).get(item_id).cloned() else {
                return;
            };
            if !d.show_all_comments.remove(&path) {
                d.show_all_comments.insert(path);
            }
            resplice_comments(d, &comments);
            d.rows_rev += 1;
        });
        self.redraw_streams(cx);
    }

    /// Reveal part of a collapsed run of unchanged lines. Only this stream's
    /// copy of the run opens: the same file is a card in several streams, and
    /// each one is its own reading surface.
    fn expand_run(&mut self, cx: &mut Cx, tab: Tab, item_id: usize, end: CollapsedEnd) {
        self.state()
            .with(|d| expand_collapsed(d, tab, item_id, end));
        self.redraw_streams(cx);
    }

    /// Re-tally the status bar's review-progress bar. Called wherever seen
    /// state can move: a tick box here, a landed load, another process's
    /// write picked up by the poll.
    pub fn refresh_progress(&mut self, cx: &mut Cx) {
        let (seen, total) = {
            let docs = self.state().snapshot();
            seen_progress(&docs, &review_state(docs.git_dir.as_deref()).load())
        };
        if let Some(mut bar) = self.view.widget(cx, ids!(progress)).borrow_mut::<SeenBar>() {
            bar.set_progress(cx, seen, total);
        }
    }

    /// Share: the review comments as one interchange document, in whichever
    /// form the dropdown row picked (canonical markdown, or the bot-style
    /// prompt) — pasteable anywhere, re-importable with `concats comments
    /// import`. Lands on the clipboard and closes the dropdown.
    fn share_comments(
        &mut self,
        cx: &mut Cx,
        render: fn(&interchange::Meta, &[interchange::Entry]) -> String,
    ) {
        self.view.view(cx, ids!(share_panel)).set_visible(cx, false);
        let (md, n) = {
            let docs = self.state().snapshot();
            let d = &*docs;
            let st = review_state(d.git_dir.as_deref()).load();
            let (old, new) = interchange::blob_sides(d.files_rows.iter(), &d.blobs);
            let mut entries = interchange::entries_from(&st.comments, &old, &new);
            entries.sort_by(|x, y| (&x.path, x.start, x.id).cmp(&(&y.path, y.start, y.id)));
            let meta = interchange::Meta {
                repo: Some(d.repo.clone()).filter(|r| !r.is_empty()),
                base: d
                    .merge_base_oid
                    .map(|o| o.to_string())
                    .or_else(|| Some(d.base.clone())),
                head: d
                    .head_oid
                    .map(|o| o.to_string())
                    .or_else(|| Some(d.head.clone())),
            };
            (render(&meta, &entries), st.comments.len())
        };
        let status = if n == 0 {
            "no comments to share yet — click a line number to leave one".to_string()
        } else {
            cx.copy_to_clipboard(&md);
            format!("{n} comment(s) copied to the clipboard")
        };
        self.view
            .label(cx, ids!(status_label))
            .set_text(cx, &status);
        self.view.redraw(cx);
    }

    /// Share = stage: `git add -p` driven by the seen ticks. Every fully seen
    /// hunk of the WORKTREE review goes into the index; everything else —
    /// unticked hunks, files that moved since the load — stays put. The
    /// worktree poll notices the index change and reloads the pane, so the
    /// staged hunks drop out of the unstaged view within a second.
    fn stage_seen_hunks(&mut self, cx: &mut Cx) {
        self.view.view(cx, ids!(share_panel)).set_visible(cx, false);
        let (workdir, files, git_dir) = self
            .state()
            .read(|d| (d.workdir.clone(), d.stage.clone(), d.git_dir.clone()));
        let (Some(workdir), Some(git_dir)) = (workdir, git_dir) else {
            return;
        };
        // Rewriting the index is git I/O: the service does it and posts the
        // report back for the status bar.
        review().send(ReviewCmd::StageSeen {
            git_dir,
            workdir,
            files,
        });
        self.view
            .label(cx, ids!(status_label))
            .set_text(cx, "staging seen hunks…");
        self.view.redraw(cx);
    }

    fn delete_comment_at(&mut self, cx: &mut Cx, tab: Tab, item_id: usize) {
        let docs = self.state().snapshot();
        let d = &*docs;
        let Some(Row::Comment { id, .. }) = d.stream(tab).get(item_id).cloned() else {
            return;
        };
        let Some(git_dir) = d.git_dir.clone() else {
            return;
        };
        drop(docs);
        review().send(ReviewCmd::DeleteComment { git_dir, id });
        self.redraw_streams(cx);
    }

    /// Open the composer on a comment's thread. Replying to a reply answers the
    /// thread, GitHub-style — the store normalizes it the same way — so the
    /// row's `parent` is the target whenever it has one.
    fn reply_to_comment_at(&mut self, cx: &mut Cx, tab: Tab, item_id: usize) {
        // The gesture claims its stream first, like a gutter press does:
        // `d.tab` routes the composer into a stream.
        self.set_gesture_tab(cx, tab);
        {
            let mut docs = self.state().write();
            let d = Arc::make_mut(&mut docs);
            let Some(Row::Comment { id, parent, .. }) = d.stream(tab).get(item_id) else {
                return;
            };
            let root = parent.unwrap_or(*id);
            strip_composer(d);
            d.compose = Some(Composing::Reply(root));
            d.compose_anchor = item_id;
            // An abandoned draft belongs to the comment it was being written
            // on, not to this thread.
            d.compose_draft.clear();
            splice_composer(d);
            d.compose_focus = true;
        }
        self.redraw_streams(cx);
    }

    /// A press on a gutter starts a comment selection, GitHub-style: the
    /// pressed line alone, extended by dragging. If the composer is already
    /// open on the same file, a further click widens its range instead —
    /// including onto the other side of a deleted→added boundary.
    fn compose_start(&mut self, cx: &mut Cx, tab: Tab, item_id: usize, blob: u32, line: u32) {
        // The gesture claims its stream first: `d.tab` routes the composer,
        // and with two streams visible in a split it must follow the pane the
        // drag actually started in (closing a composer left in another one).
        self.set_gesture_tab(cx, tab);
        {
            let mut docs = self.state().write();
            let d = Arc::make_mut(&mut docs);
            let kind = match d.active().get(item_id) {
                Some(Row::Code { kind, .. }) => *kind,
                _ => return,
            };
            let open = stream_has_composer(d.active());
            let lines = match d.compose {
                Some(Composing::Lines(c)) => Some(c),
                // A gutter press while a reply is open starts a fresh line
                // comment; the else branch below replaces the target outright.
                Some(Composing::Reply(_)) | None => None,
            };
            let widened = open
                && lines.is_some_and(|mut c| {
                    // The other side first (copies), then the side to grow.
                    let other = match kind {
                        LineKind::Del => c.new,
                        _ => c.old,
                    };
                    let side = match kind {
                        LineKind::Del => &mut c.old,
                        _ => &mut c.new,
                    };
                    let grown = match side {
                        Some(s) if s.blob == blob => {
                            s.start = s.start.min(line);
                            s.end = s.end.max(line);
                            true
                        }
                        None => {
                            // A new side opens only within the same file.
                            let same_file =
                                other.is_some_and(|o| blob_label(d, o.blob) == blob_label(d, blob));
                            if same_file {
                                *side = Some(Side {
                                    blob,
                                    start: line,
                                    end: line,
                                });
                            }
                            same_file
                        }
                        _ => false,
                    };
                    if grown {
                        d.compose = Some(Composing::Lines(c));
                    }
                    grown
                });
            if widened {
                splice_composer(d);
            } else {
                strip_composer(d);
                let side = Some(Side {
                    blob,
                    start: line,
                    end: line,
                });
                d.compose = Some(Composing::Lines(match kind {
                    LineKind::Del => Compose {
                        old: side,
                        new: None,
                    },
                    _ => Compose {
                        old: None,
                        new: side,
                    },
                }));
                d.compose_anchor = item_id;
                d.compose_draft.clear();
            }
        }
        self.redraw_streams(cx);
    }

    /// The drag extends the selection from the anchor row across contiguous
    /// code rows — deleted and added lines both, so a range can cross the
    /// del→add boundary of a hunk. Anything that isn't code (a skipped run,
    /// another file's header, a posted comment) ends the walk.
    fn compose_drag(&mut self, cx: &mut Cx, tab: Tab, y: f64) {
        // Which row the pointer is over is a question about pixels, so the list
        // that drew them answers it. It knows the band each row occupies; a row
        // delta cannot, once a wrapped line is taller than its neighbours.
        let Some(target) = self
            .list_of(cx, tab)
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

    /// The list rendering one stream, by its dock tab.
    fn list_of(&mut self, cx: &mut Cx, tab: Tab) -> WidgetRef {
        let tab_id = match tab {
            Tab::File(t) => LiveId(t),
            other => stream_tab_spec(other).0,
        };
        self.view.dock(cx, ids!(dock)).item(tab_id)
    }

    /// Release: open the inline composer below the selection.
    fn compose_open(&mut self, cx: &mut Cx) {
        {
            let mut docs = self.state().write();
            let d = Arc::make_mut(&mut docs);
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
            let body = d.compose_draft.trim().to_string();
            if body.is_empty() {
                return;
            }
            let Some(c) = d.compose.take() else {
                return;
            };
            d.compose_draft.clear();
            strip_composer(d);
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
                        path,
                        anchor: side_anchor(primary),
                        body,
                        cursors,
                    }
                }
            };
            review().send(cmd);
        }
        self.redraw_streams(cx);
    }

    fn close_composer(&mut self, cx: &mut Cx) {
        self.state().with(|d| {
            d.compose = None;
            d.compose_draft.clear();
            strip_composer(d);
        });
        self.redraw_streams(cx);
    }

    /// Spawn this tab's shell in the loaded repo. A running session is never
    /// restarted — opening is idempotent.
    ///
    /// The shell gets CONCATS_APP_WINDOW — this window's identity — and the CLI
    /// resolves the window's current range through it (the app republishes on
    /// every load), so `concats manifest` (or an agent running the review-guide
    /// skill) in this terminal targets the diff on screen with no flags, even
    /// after the reviewer switches ranges. The spawn-time
    /// CONCATS_APP_REPO/BASE/HEAD values ride along as the fallback for when
    /// the app has exited.
    pub fn open_terminal(&mut self, cx: &mut Cx, tab: LiveId) {
        let (repo, base, head) = self
            .state()
            .read(|d| (d.repo.clone(), d.base.clone(), d.head.clone()));
        let cwd = if repo.is_empty() {
            ".".to_string()
        } else {
            repo
        };
        // Absolute, so the env stays right even after the user cd's away.
        let cwd = std::fs::canonicalize(&cwd)
            .map(|p| p.display().to_string())
            .unwrap_or(cwd);
        let mut env: Vec<(&str, &str)> = vec![
            ("CONCATS_APP_WINDOW", self.state().key.as_str()),
            ("CONCATS_APP_REPO", cwd.as_str()),
        ];
        if !base.is_empty() {
            env.push(("CONCATS_APP_BASE", base.as_str()));
        }
        if !head.is_empty() {
            env.push(("CONCATS_APP_HEAD", head.as_str()));
        }
        terminal::open(
            terminal::Session {
                window: self.state().id,
                tab,
            },
            std::path::Path::new(&cwd),
            &env,
        );
        self.view.redraw(cx);
    }

    /// The status-bar `+`: another shell session in a new closable tab,
    /// created next to the other terminals, selected, and revealed.
    fn add_terminal_session(&mut self, cx: &mut Cx) {
        let dock = self.view.dock(cx, ids!(dock));
        // Wherever the user keeps their terminals — the permanent tab's bar.
        let bar = dock
            .find_tab_bar_of_tab(id!(terminal_tab))
            .map(|(bar, _)| bar)
            .unwrap_or(id!(bottom_tabs));
        let tab_id = dock.unique_id(id!(terminal_tab).0);
        let n = self.next_terminal.max(2);
        self.next_terminal = n + 1;
        dock.create_and_select_tab(
            cx,
            bar,
            tab_id,
            id!(TerminalPane),
            format!("Terminal {n}"),
            id!(TerminalCloseTab),
            None,
        );
        self.open_terminal(cx, tab_id);
        self.reveal_bottom_panel(cx);
    }

    /// A status-bar view button: reopen the stream's tab if it was closed,
    /// and jump to it either way.
    fn open_stream_tab(&mut self, cx: &mut Cx, tab: Tab) {
        let (tab_id, ..) = stream_tab_spec(tab);
        self.user_closed.remove(&tab_id);
        let dock = self.view.dock(cx, ids!(dock));
        create_stream_tab(cx, &dock, tab);
        dock.select_tab(cx, tab_id);
        self.set_gesture_tab(cx, tab);
        self.view.redraw(cx);
    }

    /// Open the bottom panel with the permanent terminal selected and its
    /// shell running. The screenshot path (CONCATS_APP_TERM=1) — rides
    /// the same slide animation as the toggle, so the shot also proves the
    /// slide completed.
    pub fn reveal_terminal(&mut self, cx: &mut Cx) {
        self.open_terminal(cx, id!(terminal_tab));
        self.view
            .dock(cx, ids!(dock))
            .select_tab(cx, id!(terminal_tab));
        self.reveal_bottom_panel(cx);
    }

    /// A panel's current size: the dock's extent along its axis minus its
    /// splitter position — both panels are their splitter's `b`, so both are
    /// measured the same way.
    fn panel_size(&mut self, cx: &mut Cx, panel: Panel) -> Option<f64> {
        let dock = self.view.dock(cx, ids!(dock));
        let rect = dock.area().rect(cx).size;
        let extent = match panel {
            Panel::Bottom => rect.y,
            Panel::Sidebar => rect.x,
        };
        let position = dock.splitter_position(panel.splitter())?;
        Some((extent.max(0.0) - position).max(0.0))
    }

    /// `save` marks the dock layout dirty — passed only on the final set of a
    /// slide, so an animation does not write a layout on every frame.
    fn set_panel_size(&mut self, cx: &mut Cx, panel: Panel, size: f64, save: bool) -> bool {
        self.view.dock(cx, ids!(dock)).set_splitter_align(
            cx,
            panel.splitter(),
            SplitterAlign::FromB(size.max(0.0)),
            save,
        )
    }

    /// The size a panel reopens at: wherever the user last dragged it.
    fn restore_size(&self, panel: Panel) -> f64 {
        let (last, default) = match panel {
            Panel::Bottom => (self.bottom_restore, 220.0),
            Panel::Sidebar => (self.sidebar_restore, 260.0),
        };
        if last > 1.0 {
            last
        } else {
            default
        }
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

    /// The status-bar `▐` toggle: slide the file browser out, or remember its
    /// width and slide it away. Same mechanism as the bottom panel, so it has
    /// the same feel and the same draggable handle.
    fn toggle_sidebar(&mut self, cx: &mut Cx) {
        let Some(current) = self.panel_size(cx, Panel::Sidebar) else {
            return;
        };
        if current <= 1.0 {
            let restore = self.restore_size(Panel::Sidebar);
            self.start_slide(cx, Panel::Sidebar, restore);
        } else {
            self.sidebar_restore = current;
            self.start_slide(cx, Panel::Sidebar, 0.0);
        }
    }

    /// The status-bar `>_` toggle: slide the panel open, or remember its
    /// height and slide it shut. Running sessions are left exactly as they
    /// are — only the very first open (no sessions at all) spawns a shell.
    fn toggle_bottom_panel(&mut self, cx: &mut Cx) {
        let Some(current) = self.panel_size(cx, Panel::Bottom) else {
            return;
        };
        if current <= 1.0 {
            if terminal::count(self.state().id) == 0 {
                self.open_terminal(cx, id!(terminal_tab));
                self.view
                    .dock(cx, ids!(dock))
                    .select_tab(cx, id!(terminal_tab));
            }
            let restore = self.restore_size(Panel::Bottom);
            self.start_slide(cx, Panel::Bottom, restore);
        } else {
            self.bottom_restore = current;
            self.start_slide(cx, Panel::Bottom, 0.0);
        }
    }

    /// Slide the panel open only if it is collapsed (terminal tab pressed).
    fn reveal_bottom_panel(&mut self, cx: &mut Cx) {
        let Some(current) = self.panel_size(cx, Panel::Bottom) else {
            return;
        };
        if current > 1.0 {
            return;
        }
        let restore = self.restore_size(Panel::Bottom);
        self.start_slide(cx, Panel::Bottom, restore);
    }

    /// The header Load button and the repo picker: the name toggles it, a
    /// recent row loads that repo, and "Open dir…" browses for another. Loads
    /// keep the current range.
    fn handle_repo_picker(&mut self, cx: &mut Cx, actions: &Actions) {
        // The header Load button reloads the open repo at its current range.
        if self.view.button(cx, ids!(load_button)).clicked(actions) {
            let (base, head) = self.current_range();
            let repo = self.state().read(|d| d.repo.clone());
            self.combo_load(cx, repo, base, head);
        }

        if self.view.button(cx, ids!(repo_button)).clicked(actions) {
            if self.view.view(cx, ids!(repo_panel)).visible() {
                self.repo_close(cx);
            } else {
                self.repo_open(cx);
            }
        }
        for (i, ids) in repo_rows!().into_iter().enumerate() {
            if self.view.button(cx, ids).clicked(actions) {
                if let Some(repo) = self.recents.get(i).cloned() {
                    let (base, head) = self.current_range();
                    self.repo_close(cx);
                    self.combo_load(cx, repo, base, head);
                }
            }
        }
        if self.view.button(cx, ids!(open_dir_row)).clicked(actions) {
            self.repo_close(cx);
            self.pick_repo(cx);
        }
    }

    /// The diff picker: chip toggles, typing filters, Enter accepts a typed
    /// ref (or an explicit base...head), a row click picks that ref as the
    /// base — both against HEAD.
    fn handle_diff_picker(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.view.button(cx, ids!(range_button)).clicked(actions) {
            if self.view.view(cx, ids!(combo_panel)).visible() {
                self.combo_close(cx);
            } else {
                self.combo_open(cx);
            }
        }
        if let Some(query) = self.view.text_input(cx, ids!(combo_input)).changed(actions) {
            self.combo_filter(cx, &query);
        }
        if let Some((text, _)) = self
            .view
            .text_input(cx, ids!(combo_input))
            .returned(actions)
        {
            let text = text.trim().to_string();
            if !text.is_empty() {
                let (base, head) = match text.split_once("...").or_else(|| text.split_once("..")) {
                    Some((b, h)) => (b.trim().to_string(), h.trim().to_string()),
                    None => (text, "HEAD".to_string()),
                };
                let repo = self.state().read(|d| d.repo.clone());
                self.combo_load(cx, repo, base, head);
            } else {
                self.combo_close(cx);
            }
        }
        if self.view.text_input(cx, ids!(combo_input)).escaped(actions) {
            self.combo_close(cx);
        }
        for ids in combo_rows!() {
            if self.view.button(cx, ids).clicked(actions) {
                let text = self.view.button(cx, ids).text();
                if !text.is_empty() {
                    // A row is usually a base ref (diffed against HEAD), but
                    // the worktree presets are full `base...head` ranges.
                    let (base, head) = match text.split_once("...") {
                        Some((b, h)) => (b.to_string(), h.to_string()),
                        None => (text, "HEAD".into()),
                    };
                    let repo = self.state().read(|d| d.repo.clone());
                    self.combo_load(cx, repo, base, head);
                }
            }
        }
    }

    /// The dock: a tab press routes the gesture/composer stream there; the
    /// drag plumbing hands the event straight back to the dock, which does
    /// all the split/merge/reorder work internally. The terminal view's and
    /// the file browser's actions ride the same widget-action pass.
    fn handle_dock_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let dock = self.view.dock(cx, ids!(dock));
        // A File tab is its file's identity, so telling one from a terminal's
        // tab takes the document's open list.
        let open_files = self
            .state()
            .read(|d| d.files_open.iter().map(|f| f.tab).collect::<Vec<_>>());
        for action in actions {
            let Some(wa) = action.as_widget_action() else {
                continue;
            };
            match wa.cast() {
                DockAction::TabWasPressed(tab_id) => {
                    if let Some(tab) = model_tab_of(tab_id, &open_files) {
                        self.set_gesture_tab(cx, tab);
                    } else if is_terminal_dock_tab(&dock, tab_id) {
                        // (Re)spawn that tab's shell — the recovery path
                        // after its previous one exited.
                        self.open_terminal(cx, tab_id);
                    }
                }
                DockAction::TabCloseWasPressed(tab_id) => {
                    if open_files.contains(&tab_id.0) {
                        self.close_file_tab(cx, tab_id);
                    } else if model_tab_of(tab_id, &open_files).is_some() {
                        // Keep at least one stream tab open — an empty main
                        // area would leave nothing to navigate from.
                        let remaining = [
                            Tab::Guide,
                            Tab::Sessions,
                            Tab::Commits,
                            Tab::Files,
                            Tab::Comments,
                        ]
                        .into_iter()
                        .map(|t| stream_tab_spec(t).0)
                        .filter(|t| *t != tab_id && dock.find_tab_bar_of_tab(*t).is_some())
                        .count();
                        if remaining > 0 {
                            self.user_closed.insert(tab_id);
                            dock.close_tab(cx, tab_id);
                        }
                    } else if tab_id != id!(terminal_tab) && is_terminal_dock_tab(&dock, tab_id) {
                        // Closing a `+`-created terminal tab ends its shell.
                        terminal::close(terminal::Session {
                            window: self.state().id,
                            tab: tab_id,
                        });
                        dock.close_tab(cx, tab_id);
                    } else if tab_id == id!(settings_tab) {
                        dock.close_tab(cx, tab_id);
                    }
                }
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
                        dock.accept_drag(cx, e, DragResponse::Move);
                    }
                }
                DockAction::Drop(e) => {
                    if let Some(src) = drag_source_tab_id(e.items.as_ref()) {
                        dock.drop_move(cx, e.abs, src);
                    }
                }
                _ => {}
            }
            // The terminal view's whole contract: encoded input bytes out,
            // viewport geometry in (fires on every draw whose size/scroll
            // changed — the resize path). The `path` names the session's tab.
            match wa.cast() {
                DesktopTerminalViewAction::Input { path, data } => {
                    if let Some(session) = terminal::tab_from_path(&path) {
                        terminal::input(session, data);
                    }
                }
                DesktopTerminalViewAction::RequestViewport {
                    path,
                    cols,
                    rows,
                    pty_rows,
                    top_row,
                } => {
                    if let Some(session) = terminal::tab_from_path(&path) {
                        if terminal::request_viewport(session, cols, rows, pty_rows, top_row) {
                            dock.item(session.tab).redraw(cx);
                        }
                    }
                }
                DesktopTerminalViewAction::None => {}
            }
            // The file browser names a path; the pane owns the dock, so
            // turning that into a tab is this side's job.
            match wa.cast() {
                FileBrowserAction::OpenFile(path) => self.open_file_tab(cx, path),
                FileBrowserAction::None => {}
            }
        }
    }

    /// The chrome around the dock: the panel toggles, the per-stream view
    /// buttons, the Share menu, and the settings gear.
    fn handle_chrome_buttons(&mut self, cx: &mut Cx, actions: &Actions) {
        // The terminal button opens the panel on the shell; the panel button
        // next to the settings gear is the plain show/hide.
        if self.view.button(cx, ids!(terminal_button)).clicked(actions) {
            self.reveal_terminal(cx);
        }
        if self.view.button(cx, ids!(panel_button)).clicked(actions) {
            self.toggle_bottom_panel(cx);
        }
        if self.view.button(cx, ids!(sidebar_button)).clicked(actions) {
            self.toggle_sidebar(cx);
        }
        if self
            .view
            .button(cx, ids!(terminal_add_button))
            .clicked(actions)
        {
            self.add_terminal_session(cx);
        }
        // The view buttons: reopen (or jump to) each stream's tab.
        for (btn, tab) in [
            (ids!(guide_button), Tab::Guide),
            (ids!(sessions_button), Tab::Sessions),
            (ids!(commits_button), Tab::Commits),
            (ids!(files_button), Tab::Files),
            (ids!(comments_button), Tab::Comments),
        ] {
            if self.view.button(cx, btn).clicked(actions) {
                self.open_stream_tab(cx, tab);
            }
        }

        if self.view.button(cx, ids!(share_button)).clicked(actions) {
            let panel = self.view.view(cx, ids!(share_panel));
            let open = panel.visible();
            if !open {
                // Staging only exists where there is a worktree to stage from.
                let worktree = self.state().read(|d| d.workdir.is_some());
                self.view
                    .button(cx, ids!(share_stage))
                    .set_visible(cx, worktree);
            }
            panel.set_visible(cx, !open);
            self.view.redraw(cx);
        }
        if self.view.button(cx, ids!(share_prompt)).clicked(actions) {
            self.share_comments(cx, |_, entries| interchange::render_prompt(entries));
        }
        if self.view.button(cx, ids!(share_md)).clicked(actions) {
            self.share_comments(cx, interchange::render);
        }
        if self.view.button(cx, ids!(share_stage)).clicked(actions) {
            self.stage_seen_hunks(cx);
        }

        // Settings: a JSON editor over the app config. The toggle opens it with
        // the current config and a hint listing theme names; Apply parses the
        // JSON, switches theme, persists, and re-themes live — the Rust-side
        // colors (they read active_theme() on draw), the DSL chrome (via
        // request_live_edit re-running script_mod), and the open terminals.
        if self.view.button(cx, ids!(settings_button)).clicked(actions) {
            self.open_settings_tab(cx);
        }
    }

    /// Per-item actions from the virtualized lists — one list per dock tab:
    /// the viewed tick box on a file card, a comment's delete button, the
    /// gutter's comment gestures, and the inline composer's controls.
    fn handle_item_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            let Some(action) = action.as_widget_action() else {
                continue;
            };
            let Some(target) = action
                .data
                .as_ref()
                .and_then(|data| data.downcast_ref::<ReviewItemAction>())
            else {
                continue;
            };
            match target {
                ReviewItemAction::Seen { tab, row }
                    if action
                        .action
                        .downcast_ref::<CheckBoxAction>()
                        .is_some_and(|action| matches!(action, CheckBoxAction::Change(_))) =>
                {
                    self.toggle_card_seen(cx, *tab, *row);
                }
                ReviewItemAction::Fold { tab, row }
                    if action
                        .action
                        .downcast_ref::<ButtonAction>()
                        .is_some_and(|action| matches!(action, ButtonAction::Clicked(_))) =>
                {
                    self.toggle_card_fold(cx, *tab, *row);
                }
                ReviewItemAction::Delete { tab, row }
                    if action
                        .action
                        .downcast_ref::<ButtonAction>()
                        .is_some_and(|action| matches!(action, ButtonAction::Clicked(_))) =>
                {
                    self.delete_comment_at(cx, *tab, *row);
                }
                ReviewItemAction::Reply { tab, row }
                    if action
                        .action
                        .downcast_ref::<ButtonAction>()
                        .is_some_and(|action| matches!(action, ButtonAction::Clicked(_))) =>
                {
                    self.reply_to_comment_at(cx, *tab, *row);
                }
                ReviewItemAction::Outdated { tab, row }
                    if action
                        .action
                        .downcast_ref::<ButtonAction>()
                        .is_some_and(|action| matches!(action, ButtonAction::Clicked(_))) =>
                {
                    self.toggle_card_outdated(cx, *tab, *row);
                }
                ReviewItemAction::Gutter { tab, row } => match action.action.downcast_ref() {
                    Some(GutterAction::DragStart { blob, line }) => {
                        self.compose_start(cx, *tab, *row, *blob, *line);
                    }
                    Some(GutterAction::DragTo { y }) => self.compose_drag(cx, *tab, *y),
                    Some(GutterAction::DragEnd) => self.compose_open(cx),
                    _ => {}
                },
                // A chevron on a collapsed run: the band's action says which end.
                ReviewItemAction::Expand { tab, row } => {
                    if let Some(end) = action.action.downcast_ref::<CollapsedEnd>() {
                        self.expand_run(cx, *tab, *row, *end);
                    }
                }
                // "N lines removed": put them back where they were taken from.
                ReviewItemAction::Reveal { tab, row } => {
                    self.state().with(|d| reveal_removed(d, *tab, *row));
                    self.redraw_streams(cx);
                }
                ReviewItemAction::Post
                    if action
                        .action
                        .downcast_ref::<ButtonAction>()
                        .is_some_and(|action| matches!(action, ButtonAction::Clicked(_))) =>
                {
                    self.post_comment(cx);
                }
                ReviewItemAction::Cancel
                    if action
                        .action
                        .downcast_ref::<ButtonAction>()
                        .is_some_and(|action| matches!(action, ButtonAction::Clicked(_))) =>
                {
                    self.close_composer(cx);
                }
                _ => {}
            }
        }
    }

    /// Every stream tab, the four fixed ones and one per open file: the
    /// pinned header's controls, and where the composer's keystrokes are
    /// mirrored into the draft — a tab missing here posts an empty comment.
    fn handle_sticky_and_composer(&mut self, cx: &mut Cx, actions: &Actions) {
        let dock = self.view.dock(cx, ids!(dock));
        // A File tab's title carries its revision and its unsaved state, so it
        // is refreshed wherever the document might have moved — the tab is the
        // only chrome the editor has.
        for (tab, title) in self.state().read(|d| {
            d.files_open
                .iter()
                .filter(|f| f.tab != crate::dock::settings_tab_id().0)
                .map(|f| (LiveId(f.tab), crate::file_view::file_tab_title(d, &f.path)))
                .collect::<Vec<_>>()
        }) {
            dock.set_tab_title(cx, tab, title);
        }
        let open_files = self
            .state()
            .read(|d| d.files_open.iter().map(|f| f.tab).collect::<Vec<_>>());
        for (tab_id, tab) in [
            (id!(guide_tab), Tab::Guide),
            (id!(sessions_tab), Tab::Sessions),
            (id!(commits_tab), Tab::Commits),
            (id!(files_tab), Tab::Files),
            (id!(comments_tab), Tab::Comments),
        ]
        .into_iter()
        .chain(open_files.iter().map(|f| (LiveId(*f), Tab::File(*f))))
        {
            let content = dock.item(tab_id);
            if content.is_empty() {
                continue;
            }
            // The sticky (pinned) header's tick box: same toggle as the card
            // header it mirrors — the list records which card that is.
            if content
                .check_box(cx, ids!(st_seen))
                .changed(actions)
                .is_some()
            {
                let idx = content.borrow::<ReviewList>().and_then(|r| r.sticky_idx);
                if let Some(idx) = idx {
                    self.toggle_card_seen(cx, tab, idx);
                }
            }
            // … and its caret: folds the card it mirrors, which scrolls the
            // list back to that header (the rows under it are gone).
            // `ButtonRef::clicked` casts the first action for the uid, so a
            // press and its click landing in one batch read as a press and the
            // caret does nothing. Match the click wherever it sits in the
            // batch.
            let fold_uid = content.widget(cx, ids!(st_fold)).widget_uid();
            let fold_clicked = actions.iter().any(|action| {
                action.as_widget_action().is_some_and(|action| {
                    action.widget_uid == fold_uid
                        && matches!(
                            action.action.downcast_ref::<ButtonAction>(),
                            Some(ButtonAction::Clicked(_))
                        )
                })
            });
            if fold_clicked {
                let idx = content.borrow::<ReviewList>().and_then(|r| r.sticky_idx);
                if let Some(idx) = idx {
                    self.toggle_card_fold(cx, tab, idx);
                }
            }
            // … and its outdated-conversations toggle, matched the same way and
            // for the same reason.
            let outdated_uid = content.widget(cx, ids!(st_outdated)).widget_uid();
            let outdated_clicked = actions.iter().any(|action| {
                action.as_widget_action().is_some_and(|action| {
                    action.widget_uid == outdated_uid
                        && matches!(
                            action.action.downcast_ref::<ButtonAction>(),
                            Some(ButtonAction::Clicked(_))
                        )
                })
            });
            if outdated_clicked {
                let idx = content.borrow::<ReviewList>().and_then(|r| r.sticky_idx);
                if let Some(idx) = idx {
                    self.toggle_card_outdated(cx, tab, idx);
                }
            }
            // The composer's field, matched by uid rather than by the action
            // data every other control in a row carries — see
            // `ReviewList::composer_input`. The draft is mirrored on every
            // keystroke because the virtualized list can recycle the field out
            // from under it; without that mirror, posting reads an empty body.
            let input_uid = content
                .borrow::<ReviewList>()
                .and_then(|list| list.composer_input);
            let keystrokes = actions
                .iter()
                .filter_map(|action| action.as_widget_action())
                .filter(|action| Some(action.widget_uid) == input_uid)
                .filter_map(|action| action.action.downcast_ref::<TextInputAction>());
            for action in keystrokes {
                match action {
                    TextInputAction::Changed(draft) => {
                        self.state().with(|d| d.compose_draft = draft.clone());
                    }
                    TextInputAction::Returned(draft, _) => {
                        self.state().with(|d| d.compose_draft = draft.clone());
                        self.post_comment(cx);
                    }
                    _ => {}
                }
            }
        }
    }
}

impl Widget for ReviewPane {
    /// Where a window's scope begins. `Root` hands every window the same one,
    /// so the rows below are told which document they are drawn from here —
    /// the pane is the first widget that belongs to exactly one window.
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let state = self.state().clone();
        // The composer asks for focus by flagging the document; the draw that
        // honours it is the one that clears it. Read before write, so the
        // common frame never takes the write lock and never deep-clones the
        // document behind `Arc::make_mut`.
        let focus_composer = state.read(|d| d.compose_focus.then_some(d.tab));
        if focus_composer.is_some() {
            state.with(|d| d.compose_focus = false);
        }
        let document = state.snapshot();
        let mut frame = FrameData {
            review: review_state(document.git_dir.as_deref()).load(),
            theme: crate::theme::active_theme(),
            focus_composer,
            document,
            state,
        };
        self.view
            .draw_walk(cx, &mut Scope::with_data(&mut frame), walk)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if let Event::NextFrame(ne) = event {
            if self.slide_next_frame.is_event(event).is_some() {
                self.step_slide(cx, ne.time);
            }
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
        self.handle_repo_picker(cx, actions);
        self.handle_diff_picker(cx, actions);
        self.handle_dock_actions(cx, actions);
        self.handle_chrome_buttons(cx, actions);
        self.handle_item_actions(cx, actions);
        self.handle_sticky_and_composer(cx, actions);
    }
}
