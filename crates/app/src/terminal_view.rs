//! The terminal renderer: one widget per dock tab, drawing the grid its
//! session owns.
//!
//! Nothing is copied out to draw. The widget locks the term in the draw path
//! and walks `renderable_content()`; the thread parsing the shell holds the
//! same lock while it writes, and [`FairMutex`](alacritty_terminal::sync)
//! hands it back rather than letting a flood of output starve the frame.
//!
//! The scrollback belongs to the term as `display_offset`, so the makepad
//! scroll bar is an indicator and an input device, never the source of truth:
//! the draw path puts it where the display is, and a drag turns back into a
//! `Scroll`.
//!
#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "Cell geometry crosses f64 pixels, f32 glyph positions and integer cells and lines; each conversion is rounded or floored, clamped, and bounded by the grid or its 100000-line history."
)]

use std::collections::HashMap;

use alacritty_terminal::{
    Term,
    grid::{Dimensions, Scroll},
    index::{Boundary, Column, Direction, Point, Side},
    selection::{Selection, SelectionType},
    term::{
        TermMode,
        cell::Flags,
        point_to_viewport,
        search::{Match, RegexSearch},
        viewport_to_point,
    },
    vte::ansi::{CursorShape, NamedColor},
};

#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::{
    find::{self, Find},
    makepad_widgets::{text::geom::Point as TextPoint, *},
    terminal::{Proxy, Session, SharedTerm, Size, blink::Blink, colors, keys, mouse},
    widgets::find_row,
    window::WindowState,
};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    set_type_default() do #(DrawTerminalCellBg::script_shader(vm)) {
        ..mod.draw.DrawQuad
        draw_call_group: @cell_bg
        color: mod.app_theme.color_cell_bg
        pixel: fn() {
            return vec4(self.color.rgb * self.color.a, self.color.a)
        }
    }

    set_type_default() do #(DrawTerminalCursor::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: mod.app_theme.color_cursor
        focus: 0.0
        border_width: 1.0
        pixel: fn() {
            if self.focus > 0.5 {
                return vec4(self.color.rgb * self.color.a, self.color.a)
            }
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let inset = self.border_width * 0.5
            let color = self.color
            sdf.box(
                inset
                inset
                self.rect_size.x - self.border_width
                self.rect_size.y - self.border_width
                0.5
            )
            sdf.stroke(color, self.border_width)
            return sdf.result
        }
    }

    mod.widgets.TerminalViewBase = #(TerminalView::register_widget(vm))

    // The terminal's text style: the app's configurable font chain from
    // `mod.app_font`, at the terminal's own size (`terminal_font_size`), so
    // the cell grid stays put when the app font size changes. (This module's
    // script scope can't see the `FONT` `let` in `main.rs`.) Braille is drawn,
    // not shaped — `fills`.
    //
    // No emoji member, unlike `FONT`: the symbols an agent draws its status
    // with (⏺ ✳ ✔ ⏸) are in Noto Color Emoji, which paints them in its own
    // colours, wider than a cell. Past the CJK face the system fallback takes
    // over, which draws them as text in the cell's colour and still finds the
    // system's emoji for real emoji. The CJK face stays because what CoreText
    // picks for CJK does not draw through makepad.
    let TERM_FONT = TextStyle{
        font_family: FontFamily{
            first := FontMember{res: mod.app_font.first asc: 0.0 desc: 0.0}
            second := FontMember{res: mod.app_font.second asc: 0.0 desc: 0.0}
            third := FontMember{res: mod.app_font.third asc: 0.0 desc: 0.0}
            fourth := FontMember{res: mod.app_font.fourth asc: 0.0 desc: 0.0}
            mono := FontMember{res: mod.app_font.mono asc: 0.0 desc: 0.0}
            cjk := FontMember{res: mod.app_font.cjk asc: 0.0 desc: 0.0}
        }
        font_size: mod.app_font.terminal_size
        // A terminal cell is the glyph box, with no leading added: box-drawing
        // characters are cut to fill that box exactly, so any extra spacing
        // opens gaps between the rows of a frame an application draws, and the
        // grid reads as stretched. JetBrains Mono is 1.32 em tall over a 0.6 em
        // advance, so this lands at the 2.2:1 cell a terminal expects.
        line_spacing: 1.0
    }

    // The same chain at a bold weight, for the cells a program makes bold. A
    // variable face takes it on its `wght` axis; a fixed one stays regular.
    let TERM_FONT_BOLD = TextStyle{
        font_family: FontFamily{
            first := FontMember{res: mod.app_font.first asc: 0.0 desc: 0.0 weight: 700.0}
            second := FontMember{res: mod.app_font.second asc: 0.0 desc: 0.0 weight: 700.0}
            third := FontMember{res: mod.app_font.third asc: 0.0 desc: 0.0 weight: 700.0}
            fourth := FontMember{res: mod.app_font.fourth asc: 0.0 desc: 0.0 weight: 700.0}
            mono := FontMember{res: mod.app_font.mono asc: 0.0 desc: 0.0 weight: 700.0}
            cjk := FontMember{res: mod.app_font.cjk asc: 0.0 desc: 0.0 weight: 700.0}
        }
        font_size: mod.app_font.terminal_size
        line_spacing: 1.0
    }

    // The italic face ahead of the same chain, for the cells a program makes
    // italic; whatever it lacks falls through to the upright faces.
    let TERM_FONT_ITALIC = TextStyle{
        font_family: FontFamily{
            italic := FontMember{res: mod.app_font.italic asc: 0.0 desc: 0.0}
            first := FontMember{res: mod.app_font.first asc: 0.0 desc: 0.0}
            second := FontMember{res: mod.app_font.second asc: 0.0 desc: 0.0}
            third := FontMember{res: mod.app_font.third asc: 0.0 desc: 0.0}
            fourth := FontMember{res: mod.app_font.fourth asc: 0.0 desc: 0.0}
            mono := FontMember{res: mod.app_font.mono asc: 0.0 desc: 0.0}
            cjk := FontMember{res: mod.app_font.cjk asc: 0.0 desc: 0.0}
        }
        font_size: mod.app_font.terminal_size
        line_spacing: 1.0
    }

    let TERM_FONT_BOLD_ITALIC = TextStyle{
        font_family: FontFamily{
            italic := FontMember{res: mod.app_font.italic asc: 0.0 desc: 0.0 weight: 700.0}
            first := FontMember{res: mod.app_font.first asc: 0.0 desc: 0.0 weight: 700.0}
            second := FontMember{res: mod.app_font.second asc: 0.0 desc: 0.0 weight: 700.0}
            third := FontMember{res: mod.app_font.third asc: 0.0 desc: 0.0 weight: 700.0}
            fourth := FontMember{res: mod.app_font.fourth asc: 0.0 desc: 0.0 weight: 700.0}
            mono := FontMember{res: mod.app_font.mono asc: 0.0 desc: 0.0 weight: 700.0}
            cjk := FontMember{res: mod.app_font.cjk asc: 0.0 desc: 0.0 weight: 700.0}
        }
        font_size: mod.app_font.terminal_size
        line_spacing: 1.0
    }

    mod.widgets.TerminalView = set_type_default() do mod.widgets.TerminalViewBase {
        width: Fill
        height: Fill
        // The comment-blue at low alpha — the app's selection tint.
        selection_color_focus: mod.app_theme.color_sel_focus
        selection_color_unfocus: mod.app_theme.color_sel_unfocus
        scroll_bars: mod.widgets.ScrollBars {
            show_scroll_x: false
            show_scroll_y: true
        }
        draw_bg +: {
            color: uniform(mod.app_theme.color_bg)
            pixel: fn() {
                return self.color
            }
        }
        draw_text +: {
            draw_call_group: @text
            text_style: TERM_FONT
        }
        draw_bold +: {
            text_style: TERM_FONT_BOLD
        }
        draw_italic +: {
            text_style: TERM_FONT_ITALIC
        }
        draw_bold_italic +: {
            text_style: TERM_FONT_BOLD_ITALIC
        }
        find_bar: mod.widgets.TabBar {
            find_row := mod.widgets.FindRow {
                find_field +: { find_input +: { empty_text: "Find in terminal" } }
            }
        }
        link_tooltip: mod.widgets.Tooltip {
            content +: {
                padding: 8
                draw_bg +: {
                    color: mod.app_theme.color_card
                    border_color: mod.app_theme.color_border
                }
                tooltip_label +: {
                    width: 300
                    draw_text +: {text_style: TERM_FONT color: mod.app_theme.color_text}
                }
            }
        }
        draw_cell_bg +: {}
        draw_cursor +: {}
    }
}

#[derive(Clone, Debug, Default)]
pub enum TerminalViewAction {
    OpenFile(String),
    #[default]
    None,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawTerminalCellBg {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawTerminalCursor {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    focus: f32,
    #[live]
    border_width: f32,
}

/// How thick an underline or strikeout is drawn, in logical pixels.
const RULE_HEIGHT: f64 = 1.0;
/// How wide a beam cursor is drawn.
const BEAM_WIDTH: f64 = 2.0;

/// Where the scroll bar sits when the display is `offset` lines back from the
/// bottom of the grid.
///
/// This and [`display_offset_for`] are inverses, and between them the only
/// description of how the bar and the scrollback relate. Writing that mapping
/// out twice is how it came to disagree with itself.
fn scroll_pos_for(offset: usize, max_scroll: f64, cell_height: f64) -> f64 {
    (max_scroll - offset as f64 * cell_height).max(0.0)
}

/// How far back the display is for a bar sitting at `scroll_y`.
fn display_offset_for(scroll_y: f64, max_scroll: f64, cell_height: f64) -> usize {
    ((max_scroll - scroll_y) / cell_height).round().max(0.0) as usize
}

#[derive(Script, Widget)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Find focus, the alt screen, a selection and forwarded text are independent."
)]
pub struct TerminalView {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    scroll_bars: ScrollBars,
    #[live]
    link_tooltip: Tooltip,
    #[live]
    #[find]
    find_bar: View,
    #[rust]
    find: Option<Find>,
    #[rust]
    focus_find: bool,
    #[rust]
    find_regex: Option<RegexSearch>,
    #[rust]
    find_match: Option<Match>,
    #[layout]
    layout: Layout,
    #[redraw]
    #[live]
    draw_bg: DrawQuad,
    #[live]
    draw_text: DrawText,
    /// Lay out the bold and italic cells only: their glyphs draw through
    /// `draw_text` with the rest.
    #[live]
    draw_bold: DrawText,
    #[live]
    draw_italic: DrawText,
    #[live]
    draw_bold_italic: DrawText,
    #[live]
    draw_cursor: DrawTerminalCursor,
    #[live]
    draw_cell_bg: DrawTerminalCellBg,
    #[live(6.0)]
    pad_x: f64,
    #[live(4.0)]
    pad_y: f64,
    #[live]
    selection_color_focus: Vec4f,
    #[live]
    selection_color_unfocus: Vec4f,
    #[rust]
    viewport_rect: Rect,
    #[rust]
    unscrolled_rect: Rect,
    #[rust]
    cell_width: f64,
    #[rust]
    cell_height: f64,
    #[rust]
    cell_offset_y: f64,
    /// The device pixel scale the cell metrics were snapped to.
    #[rust(1.0)]
    dpi: f64,
    #[rust]
    session: Option<Session>,
    #[rust]
    alternate_screen: bool,
    /// Whether the platform's text for the pending key press already went
    /// out, so the press must not type its character again.
    #[rust]
    text_forwarded: bool,
    /// The modifiers the last key event reported. Text arrives without them,
    /// and whether Option or Ctrl was down decides what the text means.
    #[rust]
    held_keys: KeyModifiers,
    #[rust]
    blink: Blink,
    /// Shaped cells by text and face (bold, italic).
    #[rust]
    glyph_cache: HashMap<(String, Flags), PreparedTextRun>,
    /// The font size, font scale and DPI factor, as bits, that the cell and
    /// the glyph cache were measured at.
    #[rust]
    metrics_for: Option<(u32, u32, u64)>,
    /// Whether the pointer is drawing a selection, and which button opened it,
    /// so motion can be reported to an application that asked for it.
    #[rust]
    selecting: bool,
    #[rust]
    held: Option<MouseButton>,
    /// Wheel deltas arrive in pixels and a line is the smallest thing a
    /// terminal can scroll, so the remainder is kept for the next event.
    #[rust]
    scroll_accum: f64,
    /// Where composed text would appear: under the cursor while there is one,
    /// at the first cell otherwise. It is an anchor for the IME panel, not a
    /// statement about whether this terminal takes text — an application that
    /// hides the cursor (`hx`, `vim`, anything on the alternate screen) still
    /// has to be typed into.
    #[rust]
    ime_pos: Vec2d,
}

fn next_terminal_match<T>(
    term: &Term<T>,
    regex: &mut RegexSearch,
    previous: Option<&Match>,
    direction: Direction,
) -> Option<Match> {
    let origin = previous.map_or_else(
        || viewport_to_point(term.grid().display_offset(), Point::new(0, Column(0))),
        |found| match direction {
            Direction::Right => {
                found
                    .end()
                    .grid_clamp(term, Boundary::Grid)
                    .add(term, Boundary::None, 1)
            }
            Direction::Left => {
                found
                    .start()
                    .grid_clamp(term, Boundary::Grid)
                    .sub(term, Boundary::None, 1)
            }
        },
    );
    term.search_next(regex, origin, direction, direction.opposite(), None)
}

/// Typing pulls the view back to the prompt, the way every terminal does.
fn scroll_to_bottom(session: Session) {
    if let Some(shared) = crate::terminal::term(session) {
        shared.lock().scroll_display(Scroll::Bottom);
    }
}

impl ScriptHook for TerminalView {
    fn on_after_apply(
        &mut self,
        _vm: &mut ScriptVm,
        _apply: &Apply,
        _scope: &mut Scope,
        _value: ScriptValue,
    ) {
        // NOTE: live edits can replace the font family without changing its size or DPI.
        self.metrics_for = None;
    }
}

impl TerminalView {
    fn seek_match(&mut self, cx: &mut Cx, direction: Direction) {
        let Some(shared) = self.session.and_then(crate::terminal::term) else {
            return;
        };
        let Some(regex) = &mut self.find_regex else {
            return;
        };
        let mut term = shared.lock();
        self.find_match = next_terminal_match(&term, regex, self.find_match.as_ref(), direction);
        term.selection = None;
        if let Some(found) = &self.find_match {
            let mut selection = Selection::new(SelectionType::Simple, *found.start(), Side::Left);
            selection.update(*found.end(), Side::Right);
            term.selection = Some(selection);
            term.scroll_to_point(*found.start());
        }
        drop(term);
        self.find_bar.label(cx, ids!(find_count)).set_text(
            cx,
            if self.find_match.is_some() {
                "Match"
            } else {
                "No matches"
            },
        );
        self.redraw(cx);
    }

    /// Which terminal session this widget instance shows: the nearest
    /// enclosing dock tab with a live session (the widget tree path contains
    /// the dock item ids).
    ///
    /// `window` comes from the scope the pane put there. The tab ids are the
    /// same in every window, so without it this resolves to whichever window
    /// opened that tab first.
    fn session_for_widget(&mut self, cx: &Cx, window: LiveId) -> Option<Session> {
        let session = self
            .session
            .filter(|session| session.window == window)
            .or_else(|| {
                cx.widget_tree()
                    .path_to(self.widget_uid())
                    .iter()
                    .rev()
                    .map(|node| Session { window, tab: *node })
                    .find(|session| crate::terminal::is_open(*session))
            });
        if session != self.session {
            self.alternate_screen = false;
            self.session = session;
        }
        session
    }

    // NOTE: draw_walk sets ime_pos from the fresh metrics right after this, every frame.
    // coland-ignore: @coland/derived_state_drift
    fn refresh_cell_metrics(&mut self, cx: &mut Cx2d) {
        let font_scale = self.draw_text.font_scale;
        let metrics_for = (
            self.draw_text.text_style.font_size.to_bits(),
            font_scale.to_bits(),
            cx.current_dpi_factor().to_bits(),
        );
        if self.metrics_for == Some(metrics_for) {
            return;
        }
        let layout = self
            .draw_text
            .layout(cx, 0.0, 0.0, None, false, Align::default(), "M");
        let glyph = layout
            .rows
            .first()
            .and_then(|row| row.glyphs.first())
            .expect("the bundled terminal font contains M");
        let glyph_height = (glyph.ascender_in_lpxs() - glyph.descender_in_lpxs()) * font_scale;
        // NOTE: down, not to the nearest pixel: box and block glyphs still come
        // from the font at its own size, and a cell a fraction smaller than
        // one overlaps its neighbour where a larger cell would show a seam.
        let dpi = cx.current_dpi_factor();
        self.dpi = dpi;
        let line = glyph_height * self.draw_text.text_style.line_spacing;
        self.cell_width = snap(f64::from(glyph.advance_in_lpxs() * font_scale), dpi);
        self.cell_height = snap(f64::from(line), dpi);
        self.cell_offset_y = snap((self.cell_height - f64::from(glyph_height)) * 0.5, dpi).max(0.0);
        self.glyph_cache.clear();
        self.metrics_for = Some(metrics_for);
    }

    /// The widget's geometry in cells, for the term and the PTY.
    fn size(&self) -> Size {
        let (cell_width, cell_height) = (self.cell_width, self.cell_height);
        Size {
            columns: ((self.viewport_rect.size.x - self.pad_x * 2.0) / cell_width)
                .floor()
                .max(1.0) as u16,
            screen_lines: ((self.viewport_rect.size.y - self.pad_y * 2.0) / cell_height)
                .floor()
                .max(1.0) as u16,
            cell_width: cell_width.round().max(1.0) as u16,
            cell_height: cell_height.round().max(1.0) as u16,
        }
    }

    /// How far the scroll bar can travel over a grid of `total_lines`: the
    /// content it stands for, less the part already on screen.
    fn max_scroll(&self, total_lines: usize) -> f64 {
        let (_, cell_height) = (self.cell_width, self.cell_height);
        let content_height =
            (total_lines as f64 * cell_height + self.pad_y * 2.0).max(self.viewport_rect.size.y);
        content_height - self.viewport_rect.size.y
    }

    /// Where the top left cell is drawn.
    fn origin(&self) -> Vec2d {
        dvec2(
            snap(self.unscrolled_rect.pos.x + self.pad_x, self.dpi),
            snap(self.unscrolled_rect.pos.y + self.pad_y, self.dpi),
        )
    }

    fn cache_terminal_glyphs(&mut self, cx: &mut Cx2d, key: &(String, Flags)) {
        if self.glyph_cache.contains_key(key) {
            return;
        }
        let layout = match (key.1.contains(Flags::BOLD), key.1.contains(Flags::ITALIC)) {
            (false, false) => &mut self.draw_text,
            (true, false) => &mut self.draw_bold,
            (false, true) => &mut self.draw_italic,
            (true, true) => &mut self.draw_bold_italic,
        };
        let Some(run) = layout.prepare_single_line_run(cx, &key.0) else {
            return;
        };
        self.glyph_cache.insert(key.clone(), run);
    }

    /// Walk the visible grid: a background where a cell asked for one, the
    /// glyph, whatever rules its flags call for, and the cursor on top.
    #[expect(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "One locked grid snapshot is painted in background, cursor, and glyph order."
    )]
    fn draw_grid(&mut self, cx: &mut Cx2d, term: &Term<Proxy>, focused: bool) {
        let theme = crate::theme::active_theme();
        let content = term.renderable_content();
        let (display_offset, cursor, selection) =
            (content.display_offset, content.cursor, content.selection);
        let slot = |named: NamedColor| {
            crate::theme::paint(colors::slot(named as usize, content.colors, &theme))
        };
        let panel = slot(NamedColor::Background);
        let (cell_width, cell_height) = (self.cell_width, self.cell_height);
        let origin = self.origin();

        self.draw_cell_bg.new_draw_call(cx);
        // The terminal's own background, which a program may have set (OSC
        // 11), under every cell; one on the default background shows it.
        self.draw_cell_bg.color = panel;
        self.draw_cell_bg.draw_abs(cx, self.viewport_rect);
        self.draw_cursor.new_draw_call(cx);
        // What OSC 12 set, else the theme's.
        self.draw_cursor.color = slot(NamedColor::Cursor);
        // A program may ask for a blinking cursor; it blinks while the
        // terminal has the keyboard.
        let shown = self
            .blink
            .shown(cx, focused && term.cursor_style().blinking);
        if shown {
            self.draw_cursor_at(cx, cursor.shape, cursor.point, display_offset, focused);
        }
        self.draw_text.new_draw_call(cx);
        self.draw_text.begin_many_instances(cx);

        // NOTE: one buffer for every cell's cache key, so a lookup allocates
        // nothing.
        let mut key = (String::new(), Flags::empty());
        for indexed in content.display_iter {
            let flags = indexed.cell.flags;
            // The second half of a wide character is not drawn: the glyph in
            // the cell before it already covers this column.
            if flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let Some(viewport) = point_to_viewport(display_offset, indexed.point) else {
                continue;
            };
            let x = origin.x + viewport.column.0 as f64 * cell_width;
            let y = origin.y + viewport.line as f64 * cell_height;
            let width = if flags.contains(Flags::WIDE_CHAR) {
                cell_width * 2.0
            } else {
                cell_width
            };

            let mut fg = colors::foreground(indexed.cell.fg, flags, content.colors, &theme);
            let mut bg = colors::background(indexed.cell.bg, content.colors, &theme);
            if flags.contains(Flags::INVERSE) {
                let ink = fg;
                fg = bg.unwrap_or_else(|| {
                    colors::foreground(indexed.cell.bg, Flags::empty(), content.colors, &theme)
                });
                bg = Some(ink);
            }
            if selection
                .is_some_and(|range| range.contains_cell(&indexed, cursor.point, cursor.shape))
            {
                bg = Some(if focused {
                    self.selection_color_focus
                } else {
                    self.selection_color_unfocus
                });
            }

            if shown
                && focused
                && cursor.shape == CursorShape::Block
                && indexed.point == cursor.point
            {
                fg = panel;
            }
            if let Some(color) = bg {
                self.draw_cell_bg.color = color;
                self.draw_cell_bg.draw_abs(
                    cx,
                    Rect {
                        pos: dvec2(x, y),
                        size: dvec2(width, cell_height),
                    },
                );
            }

            // A tab lands in the grid as U+0009 (`Term::put_tab` writes the
            // character it advanced over), and no control character has a
            // glyph: those cells are spacing, and shaping them draws .notdef.
            let drawn = (!flags.contains(Flags::HIDDEN))
                .then(|| fills(indexed.cell.c, dvec2(cell_width, cell_height), self.dpi))
                .flatten();
            if let Some(fills) = drawn {
                for (rect, ink) in fills {
                    self.draw_cell_bg.color = Vec4f {
                        w: fg.w * ink,
                        ..fg
                    };
                    self.draw_cell_bg.draw_abs(
                        cx,
                        Rect {
                            pos: rect.pos + dvec2(x, y),
                            size: rect.size,
                        },
                    );
                }
            } else if (indexed.cell.c != ' ' || indexed.cell.zerowidth().is_some())
                && !indexed.cell.c.is_control()
                && !flags.contains(Flags::HIDDEN)
            {
                key.0.clear();
                key.0.push(indexed.cell.c);
                if let Some(marks) = indexed.cell.zerowidth() {
                    key.0.extend(marks);
                }
                key.1 = flags & Flags::BOLD_ITALIC;
                self.cache_terminal_glyphs(cx, &key);
                if let Some(run) = self.glyph_cache.get(&key) {
                    let baseline_y = snap(
                        y + self.cell_offset_y + f64::from(run.ascender_in_lpxs),
                        self.dpi,
                    );
                    for glyph in &run.glyphs {
                        self.draw_text.draw_rasterized_glyph_abs(
                            cx,
                            TextPoint::new(
                                (x + f64::from(glyph.pen_x_in_lpxs + glyph.offset_x_in_lpxs))
                                    as f32,
                                baseline_y as f32,
                            ),
                            glyph.font_size_in_lpxs,
                            glyph.rasterized,
                            fg,
                        );
                    }
                }
            }

            if flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT) {
                let rule = indexed.cell.underline_color().map_or(fg, |color| {
                    colors::foreground(color, flags, content.colors, &theme)
                });
                self.draw_cell_bg.color = rule;
                // A wide character underlines both of its cells.
                let columns = if flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                };
                for column in 0..columns {
                    let left = x + f64::from(column) * cell_width;
                    for rect in underline_fills(flags, dvec2(cell_width, cell_height), self.dpi) {
                        self.draw_cell_bg.draw_abs(
                            cx,
                            Rect {
                                pos: rect.pos + dvec2(left, y),
                                size: rect.size,
                            },
                        );
                    }
                }
                if flags.contains(Flags::STRIKEOUT) {
                    self.draw_rule(cx, x, y + cell_height * 0.5, width);
                }
            }
        }
        self.draw_text.end_many_instances(cx);
    }

    fn draw_rule(&mut self, cx: &mut Cx2d, x: f64, y: f64, width: f64) {
        self.draw_cell_bg.draw_abs(
            cx,
            Rect {
                pos: dvec2(x, y),
                size: dvec2(width, RULE_HEIGHT),
            },
        );
    }

    fn draw_cursor_at(
        &mut self,
        cx: &mut Cx2d,
        shape: CursorShape,
        point: Point,
        display_offset: usize,
        focused: bool,
    ) {
        if shape == CursorShape::Hidden {
            return;
        }
        let Some(viewport) = point_to_viewport(display_offset, point) else {
            return;
        };
        let (cell_width, cell_height) = (self.cell_width, self.cell_height);
        let origin = self.origin();
        let x = origin.x + viewport.column.0 as f64 * cell_width;
        let y = origin.y + viewport.line as f64 * cell_height;

        if y < self.viewport_rect.pos.y
            || y + cell_height > self.viewport_rect.pos.y + self.viewport_rect.size.y
        {
            return;
        }
        let (pos, size) = match shape {
            CursorShape::Beam => (dvec2(x, y), dvec2(BEAM_WIDTH, cell_height)),
            CursorShape::Underline => (
                dvec2(x, y + cell_height - RULE_HEIGHT * 2.0),
                dvec2(cell_width, RULE_HEIGHT * 2.0),
            ),
            _ => (dvec2(x, y), dvec2(cell_width, cell_height)),
        };

        // The IME panel opens under the cursor, wherever it is.
        self.ime_pos = dvec2(
            x - self.unscrolled_rect.pos.x,
            y - self.unscrolled_rect.pos.y + cell_height,
        );
        self.draw_cursor.focus = if focused && shape != CursorShape::HollowBlock {
            1.0
        } else {
            0.0
        };
        self.draw_cursor.draw_abs(cx, Rect { pos, size });
    }

    /// The cell under the pointer, and which half of it — the side decides
    /// whether a selection takes the character or stops before it.
    fn point_at(&self, abs: Vec2d, term: &Term<Proxy>) -> (Point, Side) {
        let (cell_width, cell_height) = (self.cell_width, self.cell_height);
        let origin = self.origin();
        let x = (abs.x - origin.x).max(0.0);
        let y = (abs.y - origin.y).max(0.0);
        let column = ((x / cell_width).floor() as usize).min(term.columns().saturating_sub(1));
        let line = ((y / cell_height).floor() as usize).min(term.screen_lines().saturating_sub(1));
        let side = if x - (column as f64 * cell_width) > cell_width / 2.0 {
            Side::Right
        } else {
            Side::Left
        };
        (
            viewport_to_point(
                term.grid().display_offset(),
                Point::new(line, Column(column)),
            ),
            side,
        )
    }

    /// Where the hyperlink under the pointer leads, unless the application is
    /// tracking the pointer.
    fn link_at(
        &self,
        shared: &SharedTerm,
        abs: Vec2d,
        modifiers: KeyModifiers,
        state: Option<&WindowState>,
    ) -> Option<crate::links::Target> {
        let uri = {
            let term = shared.lock();
            let (point, _) = self.point_at(abs, &term);
            (!mouse::wants_pointer(modifiers, *term.mode()))
                .then(|| {
                    term.grid()[point]
                        .hyperlink()
                        .map(|link| link.uri().to_owned())
                })
                .flatten()
        };
        uri.as_deref().and_then(|uri| {
            state?.read(|d| {
                crate::links::target(
                    uri,
                    d.workdir
                        .as_deref()
                        .unwrap_or_else(|| std::path::Path::new(&d.repo)),
                )
            })
        })
    }

    /// One key press for the session, whichever way it reached this view.
    ///
    /// Two paths deliver it: `Hit::KeyDown` while this terminal holds the
    /// keyboard, and `Event::KeyDown` while nothing does and the window's
    /// selected terminal takes what is unclaimed. They have to encode it the
    /// same way, or a key works in one and vanishes in the other — which is
    /// what `hx` and `vim` ran into, since [`keys::encode`] answers `None` for
    /// a key that produces text and only one of the two paths had the
    /// character to fall back on.
    ///
    /// The fallback is for when the platform sends no text of its own. When it
    /// does, that text has already gone out as [`Self::text_typed`] and the
    /// character would type the key twice.
    fn key_press(&mut self, cx: &mut Cx, session: Session, e: &KeyEvent, mode: TermMode) {
        // NOTE: Cmd+V's paste arrives as text. Ctrl+V is the program's own key.
        if matches!(e.key_code, KeyCode::KeyV) && e.modifiers.logo {
            return;
        }
        let typed = std::mem::take(&mut self.text_forwarded);
        // A Cmd chord the app did not claim is not a character; Option kept
        // as Alt puts ESC in front of the key's own one.
        let character = || {
            let c = e.key_code.to_char(e.modifiers.shift)?;
            keys::encode_text(&c.to_string(), e.modifiers, mode)
        };
        let Some(bytes) = keys::encode(e, true, mode)
            .or_else(|| (!typed && !e.modifiers.logo).then(character).flatten())
        else {
            return;
        };
        cx.set_key_focus(self.scroll_bars.area());
        crate::terminal::input(session, bytes);
        scroll_to_bottom(session);
        self.draw_bg.redraw(cx);
    }

    /// One key release. Only the kitty protocol asks to hear about these.
    fn key_release(session: Session, e: &KeyEvent, mode: TermMode) {
        if let Some(bytes) = keys::encode(e, false, mode) {
            crate::terminal::input(session, bytes);
        }
    }

    /// Text the platform produced — typing, a dead key, an IME commit, a paste
    /// — for the session. Marks the key press it belongs to as already sent.
    fn text_typed(&mut self, cx: &mut Cx, session: Session, e: &TextInputEvent, mode: TermMode) {
        let Some(text) = keys::typed(e, self.held_keys) else {
            return;
        };
        cx.set_key_focus(self.scroll_bars.area());
        if e.was_paste {
            self.text_forwarded = true;
            Self::paste(session, text, mode.contains(TermMode::BRACKETED_PASTE));
        } else if let Some(bytes) = keys::encode_text(text, KeyModifiers::default(), mode) {
            self.text_forwarded = true;
            crate::terminal::input(session, bytes);
        }
        scroll_to_bottom(session);
        self.draw_bg.redraw(cx);
    }

    fn paste(session: Session, text: &str, bracketed: bool) {
        if text.is_empty() {
            return;
        }
        crate::terminal::input(session, paste_bytes(text, bracketed));
    }

    fn dropped_text_payload(items: &[DragItem]) -> Option<String> {
        let items: Vec<_> = items.iter().filter(|item| from_outside(item)).collect();
        if items.is_empty() {
            return None;
        }
        let only_paths = items
            .iter()
            .all(|item| matches!(item, DragItem::FilePath { .. }));
        let parts: Vec<String> = items
            .iter()
            .map(|item| match item {
                DragItem::String { value, .. } => value.clone(),
                // Quoted, so the shell reads the path as one word.
                DragItem::FilePath { path, .. } => format!("'{}'", path.replace('\'', "'\\''")),
            })
            .collect();
        Some(if only_paths {
            format!("{} ", parts.join(" "))
        } else {
            parts.join("\n")
        })
    }

    fn handle_drop(&mut self, cx: &mut Cx, session: Session, hit: DragHit, mode: TermMode) -> bool {
        match hit {
            DragHit::Drag(drag) => {
                if !drag.items.iter().any(from_outside) {
                    return false;
                }
                *drag.response.lock().unwrap() = DragResponse::Copy;
                true
            }
            DragHit::Drop(drop) => {
                let Some(payload) = Self::dropped_text_payload(drop.items.as_ref()) else {
                    return false;
                };
                Self::paste(session, &payload, mode.contains(TermMode::BRACKETED_PASTE));
                self.draw_bg.redraw(cx);
                true
            }
            _ => false,
        }
    }
}

// The ranges the terminal draws itself instead of shaping from a font:
// Braille, block elements and box lines. These glyphs have to meet at the
// edges of their cells, and a font's, a little smaller than the cell and
// antialiased, leaves seams between them. Ghostty and Alacritty draw them for
// the same reason. A new range is a new arm of `fills`.

/// How a box line's arms reach out of a cell, as up, right, down and left: 0
/// none, 1 light, 2 heavy, 3 double. Empty for a dashed line, which
/// [`dash_fills`] draws.
#[rustfmt::skip]
const LINES: [&str; 76] = [
    "0101", "0202", "1010", "2020", "", "", "", "", "", "", "", "",
    "0110", "0210", "0120", "0220", "0011", "0012", "0021", "0022",
    "1100", "1200", "2100", "2200", "1001", "1002", "2001", "2002",
    "1110", "1210", "2110", "1120", "2120", "2210", "1220", "2220",
    "1011", "1012", "2011", "1021", "2021", "2012", "1022", "2022",
    "0111", "0112", "0211", "0212", "0121", "0122", "0221", "0222",
    "1101", "1102", "1201", "1202", "2101", "2102", "2201", "2202",
    "1111", "1112", "1211", "1212", "2111", "1121", "2121", "2112",
    "2211", "1122", "1221", "2212", "1222", "2122", "2221", "2222",
];

/// The double lines, U+2550 to U+256C, in the same notation as [`LINES`].
#[rustfmt::skip]
const DOUBLES: [&str; 29] = [
    "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", "1300", "3100",
    "3300", "1003", "3001", "3003", "1310", "3130", "3330", "1013", "3031", "3033",
    "0313", "0131", "0333", "1303", "3101", "3303", "1313", "3131", "3333",
];

/// The half lines, U+2574 to U+257F, in the same notation as [`LINES`].
const HALVES: [&str; 12] = [
    "0001", "1000", "0100", "0010", "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010",
];

/// The filled rectangles of `c` in a cell of `size` at `dpi`, each relative to
/// the cell's corner and with the share of the ink it takes (a shade takes
/// less than all of it). `None` for a character the font draws.
// NOTE: the one table of drawn ranges; each range's painter is a callee.
// coland-ignore: @coland/high_call_degree
fn fills(c: char, size: Vec2d, dpi: f64) -> Option<Vec<(Rect, f32)>> {
    // NOTE: worked in whole device pixels, so a run of cells meets edge to edge.
    let px = dvec2((size.x * dpi).round(), (size.y * dpi).round());
    let device = match c {
        '\u{2800}'..='\u{28ff}' => return Some(braille_fills(c, size)),
        '\u{2580}'..='\u{259f}' => block_fills(c, px),
        '\u{2504}'..='\u{250b}' | '\u{254c}'..='\u{254f}' => dash_fills(c, px),
        '\u{2500}'..='\u{254b}' => line_fills(LINES[c as usize - 0x2500], px)?,
        '\u{2550}'..='\u{256c}' => line_fills(DOUBLES[c as usize - 0x2550], px)?,
        '\u{2574}'..='\u{257f}' => line_fills(HALVES[c as usize - 0x2574], px)?,
        '\u{256d}'..='\u{2570}' => arc_fills(c, px),
        '\u{2571}'..='\u{2573}' => diagonal_fills(c, px),
        '\u{e0b0}'..='\u{e0b7}' => powerline_fills(c, px),
        '\u{e0b8}' | '\u{e0ba}' | '\u{e0bc}' | '\u{e0be}' => triangle_fills(c, px),
        // NOTE: the slanted powerline separators are the box diagonals.
        '\u{e0b9}' | '\u{e0bf}' => diagonal_fills('╲', px),
        '\u{e0bb}' | '\u{e0bd}' => diagonal_fills('╱', px),
        _ => return None,
    };
    let to_cell = |(rect, ink): (Rect, f32)| {
        let rect = Rect {
            pos: rect.pos / dpi,
            size: rect.size / dpi,
        };
        (rect, ink)
    };
    Some(device.into_iter().map(to_cell).collect())
}

/// Block elements in a cell of `px` device pixels: eighths of the cell, shades
/// and quadrants.
fn block_fills(c: char, px: Vec2d) -> Vec<(Rect, f32)> {
    /// The quadrants of U+2596 to U+259F: upper left 1, upper right 2, lower
    /// left 4, lower right 8.
    const QUADRANTS: [u8; 10] = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14];
    let (w, h) = (px.x, px.y);
    let eighths = |n: u32, of: f64| (of * f64::from(n) / 8.0).round();
    let n = c as u32 - 0x2580;
    match n {
        0x0 => vec![fill(0.0, 0.0, w, eighths(4, h))],
        0x1..=0x8 => vec![fill(0.0, h - eighths(n, h), w, h)],
        0x9..=0xf => vec![fill(0.0, 0.0, eighths(16 - n, w), h)],
        0x10 => vec![fill(eighths(4, w), 0.0, w, h)],
        0x11..=0x13 => {
            let ink = match n {
                0x11 => 0.25,
                0x12 => 0.5,
                _ => 0.75,
            };
            vec![(fill(0.0, 0.0, w, h).0, ink)]
        }
        0x14 => vec![fill(0.0, 0.0, w, eighths(1, h))],
        0x15 => vec![fill(w - eighths(1, w), 0.0, w, h)],
        _ => {
            let (mx, my) = (eighths(4, w), eighths(4, h));
            let quadrants = QUADRANTS[n as usize - 0x16];
            [
                (1, (0.0, 0.0, mx, my)),
                (2, (mx, 0.0, w, my)),
                (4, (0.0, my, mx, h)),
                (8, (mx, my, w, h)),
            ]
            .into_iter()
            .filter(|(bit, _)| quadrants & bit != 0)
            .map(|(_, (x0, y0, x1, y1))| fill(x0, y0, x1, y1))
            .collect()
        }
    }
}

/// A box line, given by its [`LINES`] or [`HALVES`] entry, in a cell of `px`
/// device pixels: each arm a stroke from its edge to the centre and on across
/// the other direction's stroke, so a corner or a tee closes without a notch.
/// A light stroke is an eighth of the cell's width, a heavy one twice that,
/// and a double line two light strokes a light stroke apart. `None` for an
/// empty entry.
#[expect(
    clippy::cognitive_complexity,
    reason = "The four arms, single or double, are joined in one place, where each can see the others."
)]
// NOTE: the four arms are joined together, so each sees the others' weights.
// coland-ignore: @coland/high_decision_count
fn line_fills(arms: &str, px: Vec2d) -> Option<Vec<(Rect, f32)>> {
    let &[up, right, down, left] = arms.as_bytes() else {
        return None;
    };
    let [up, right, down, left] = [up, right, down, left].map(|digit| digit - b'0');
    let (w, h) = (px.x, px.y);
    let light = (w / 8.0).round().max(1.0);
    let thick = |weight: u8| light * f64::from(weight);
    // Where a stroke of width `t` starts when it is centred across `of`.
    let start = |of: f64, t: f64| ((of - t) / 2.0).floor();
    let (vertical, horizontal) = (thick(up.max(down)), thick(left.max(right)));
    let (x0, x1) = (start(w, vertical), start(w, vertical) + vertical);
    let (y0, y1) = (start(h, horizontal), start(h, horizontal) + horizontal);
    let (double_vertical, double_horizontal) = (up.max(down) == 3, left.max(right) == 3);
    // A double arm's strokes stop apart: the one on a side another double arm
    // leaves from turns into that arm's near stroke, the other runs on to the
    // band's far edge, so corners and tees close as they do in Ghostty.
    let mut fills = Vec::new();
    for (weight, edge, far, near) in [(up, 0.0, y1, y0 + light), (down, h, y0, y1 - light)] {
        if weight == 3 {
            for (offset, side) in [(0.0, left), (2.0 * light, right)] {
                let stop = if side > 0 && double_horizontal {
                    near
                } else {
                    far
                };
                fills.push(fill(
                    x0 + offset,
                    edge.min(stop),
                    x0 + offset + light,
                    edge.max(stop),
                ));
            }
        } else if weight > 0 {
            let t = thick(weight);
            fills.push(fill(
                start(w, t),
                edge.min(far),
                start(w, t) + t,
                edge.max(far),
            ));
        }
    }
    for (weight, edge, far, near) in [(left, 0.0, x1, x0 + light), (right, w, x0, x1 - light)] {
        if weight == 3 {
            for (offset, side) in [(0.0, up), (2.0 * light, down)] {
                let stop = if side > 0 && double_vertical {
                    near
                } else {
                    far
                };
                fills.push(fill(
                    edge.min(stop),
                    y0 + offset,
                    edge.max(stop),
                    y0 + offset + light,
                ));
            }
        } else if weight > 0 {
            let t = thick(weight);
            fills.push(fill(
                edge.min(far),
                start(h, t),
                edge.max(far),
                start(h, t) + t,
            ));
        }
    }
    Some(fills)
}

/// The dashed lines (U+2504 to U+250B, U+254C to U+254F) in a cell of `px`
/// device pixels: a light or heavy line in three, four or two dashes, set in
/// half a gap from each edge so a run of cells dashes evenly. Gaps take at
/// most half the line, and a leftover pixel lengthens a dash rather than a
/// gap, as in Ghostty.
fn dash_fills(c: char, px: Vec2d) -> Vec<(Rect, f32)> {
    // Each group runs light across, heavy across, light down, heavy down.
    let (count, n) = match c {
        '\u{2504}'..='\u{2507}' => (3, c as u32 - 0x2504),
        '\u{2508}'..='\u{250b}' => (4, c as u32 - 0x2508),
        _ => (2, c as u32 - 0x254c),
    };
    let down = n >= 2;
    let (along, across) = if down { (px.y, px.x) } else { (px.x, px.y) };
    let light = (px.x / 8.0).round().max(1.0);
    let t = light * f64::from(n % 2 + 1);
    let at = ((across - t) / 2.0).floor();
    let dashes = f64::from(count);
    let gap = light.max(4.0).min((along / (2.0 * dashes)).floor());
    let (dash, extra) = (
        ((along - gap * dashes) / dashes).floor(),
        (along - gap * dashes) % dashes,
    );
    (0..count)
        .map(f64::from)
        .map(|i| {
            let from = (gap / 2.0).floor() + i * (dash + gap) + i.min(extra);
            let to = from + dash + if i < extra { 1.0 } else { 0.0 };
            if down {
                fill(at, from, at + t, to)
            } else {
                fill(from, at, to, at + t)
            }
        })
        .collect()
}

/// The rounded corners (U+256D to U+2570) in a cell of `px` device pixels: a
/// quarter circle from one arm to the other, then straight on to the cell's
/// edge. As thick as a light line and centred where [`line_fills`] puts one,
/// so a corner meets the lines beside it, one device pixel row at a time.
fn arc_fills(c: char, px: Vec2d) -> Vec<(Rect, f32)> {
    let (w, h) = (px.x, px.y);
    let stroke = (w / 8.0).round().max(1.0);
    let (cx, cy) = (
        ((w - stroke) / 2.0).floor() + stroke / 2.0,
        ((h - stroke) / 2.0).floor() + stroke / 2.0,
    );
    // Which way the arms leave the cell: right or left, down or up.
    let (sx, sy) = match c {
        '\u{256d}' => (1.0, 1.0),
        '\u{256e}' => (-1.0, 1.0),
        '\u{256f}' => (-1.0, -1.0),
        _ => (1.0, -1.0),
    };
    let radius = cx.min(w - cx).min(cy).min(h - cy);
    let (ox, oy) = (cx + sx * radius, cy + sy * radius);
    let (outer, inner) = (radius + stroke / 2.0, (radius - stroke / 2.0).max(0.0));
    (0..h as u16)
        .map(f64::from)
        .filter_map(|y| {
            let dy = y + 0.5 - oy;
            if dy * sy > 0.0 {
                // Past the circle's centre: the straight run to the edge.
                return Some(fill(cx - stroke / 2.0, y, cx + stroke / 2.0, y + 1.0));
            }
            if dy.abs() > outer {
                return None;
            }
            let far = ox - sx * (outer * outer - dy * dy).sqrt();
            let near = ox - sx * (inner * inner - dy * dy).max(0.0).sqrt();
            Some(fill(
                far.min(near).round(),
                y,
                far.max(near).round(),
                y + 1.0,
            ))
        })
        .collect()
}

/// The diagonals (U+2571 to U+2573) in a cell of `px` device pixels: a light
/// stroke from corner to corner, one device pixel row at a time, so cells
/// down a slope join at their corners.
fn diagonal_fills(c: char, px: Vec2d) -> Vec<(Rect, f32)> {
    let (w, h) = (px.x, px.y);
    let light = (w / 8.0).round().max(1.0);
    // A slanted stroke cuts each row wider than it is thick.
    let half = light / 2.0 * w.hypot(h) / h;
    (0..h as u16)
        .map(f64::from)
        .flat_map(|y| {
            // Where ╱ crosses this row; ╲ is its mirror image.
            let x0 = (w * (1.0 - (y + 1.0) / h) - half).max(0.0).round();
            let x1 = (w * (1.0 - y / h) + half).min(w).round();
            let rising = (c != '\u{2572}').then(|| fill(x0, y, x1, y + 1.0));
            let falling = (c != '\u{2571}').then(|| fill(w - x1, y, w - x0, y + 1.0));
            rising.into_iter().chain(falling)
        })
        .collect()
}

/// The powerline separators in a cell of `px` device pixels, one device pixel
/// row at a time: an arrow (U+E0B0 to U+E0B3) or a half ellipse (U+E0B4 to
/// U+E0B7), solid or outlined, pointing right and then left, and reaching
/// across the whole cell at its middle.
fn powerline_fills(c: char, px: Vec2d) -> Vec<(Rect, f32)> {
    let (w, h) = (px.x, px.y);
    let light = (w / 8.0).round().max(1.0);
    let n = c as u32 - 0xe0b0;
    let (rounded, right, solid) = (n >= 4, n % 4 < 2, n.is_multiple_of(2));
    (0..h as u16)
        .map(f64::from)
        .map(|y| {
            // How far the shape reaches into this row from its flat side, and
            // how far an outline's inside does.
            let (outer, inner) = if rounded {
                let reach = |rx: f64, ry: f64| {
                    let dy = (y + 0.5 - h / 2.0) / ry;
                    (rx * (1.0 - dy * dy).max(0.0).sqrt()).round()
                };
                (reach(w, h / 2.0), reach(w - light, h / 2.0 - light))
            } else {
                let reach = (w * (1.0 - ((2.0 * y + 1.0) / h - 1.0).abs())).round();
                (reach, (reach - light).max(0.0))
            };
            let from = if solid { 0.0 } else { inner };
            if right {
                fill(from, y, outer, y + 1.0)
            } else {
                fill(w - outer, y, w - from, y + 1.0)
            }
        })
        .collect()
}

/// The slanted powerline triangles (U+E0B8, U+E0BA, U+E0BC, U+E0BE) in a cell
/// of `px` device pixels: lower left, lower right, upper left and upper right,
/// each filling its corner up to the cell's diagonal, one device pixel row at
/// a time.
fn triangle_fills(c: char, px: Vec2d) -> Vec<(Rect, f32)> {
    let (w, h) = (px.x, px.y);
    (0..h as u16)
        .map(f64::from)
        .map(|y| {
            // Where ╲ crosses the middle of this row; ╱ crosses at its mirror.
            let falling = (w * (y + 0.5) / h).round();
            let (x0, x1) = match c {
                '\u{e0b8}' => (0.0, falling),
                '\u{e0ba}' => (w - falling, w),
                '\u{e0bc}' => (0.0, w - falling),
                _ => (falling, w),
            };
            fill(x0, y, x1, y + 1.0)
        })
        .collect()
}

/// The underline `flags` ask for (SGR 4:1 to 4:5) under one cell of `size` at
/// `dpi`, on whole device pixels: straight, double, curly, dotted or dashed,
/// each a pattern that repeats from cell to cell. Nothing without one.
fn underline_fills(flags: Flags, size: Vec2d, dpi: f64) -> Vec<Rect> {
    let (w, h) = ((size.x * dpi).round(), (size.y * dpi).round());
    let t = (RULE_HEIGHT * dpi).round().max(1.0);
    let top = h - 2.0 * t;
    let rule = |x0: f64, y0: f64, x1: f64| Rect {
        pos: dvec2(x0, y0) / dpi,
        size: dvec2(x1 - x0, t) / dpi,
    };
    let columns = (0..w as u16).map(f64::from);
    if flags.contains(Flags::DOUBLE_UNDERLINE) {
        vec![rule(0.0, top, w), rule(0.0, top - 2.0 * t, w)]
    } else if flags.contains(Flags::UNDERCURL) {
        // One period of a wave per cell, a device pixel column at a time.
        columns
            .map(|x| {
                rule(
                    x,
                    top + (t * (x / w * std::f64::consts::TAU).sin()).round(),
                    x + 1.0,
                )
            })
            .collect()
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        columns
            .filter(|x| x % (2.0 * t) == 0.0)
            .map(|x| rule(x, top, (x + t).min(w)))
            .collect()
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        vec![rule(0.0, top, (w / 2.0).round())]
    } else if flags.contains(Flags::UNDERLINE) {
        vec![rule(0.0, top, w)]
    } else {
        Vec::new()
    }
}

/// A solid rectangle between two corners.
fn fill(x0: f64, y0: f64, x1: f64, y1: f64) -> (Rect, f32) {
    let rect = Rect {
        pos: dvec2(x0, y0),
        size: dvec2(x1 - x0, y1 - y0),
    };
    (rect, 1.0)
}

/// Braille: the pattern is in the codepoint, and no font we bundle carries the
/// block — the one on this platform that does is a proportional symbol face
/// whose dots do not line up between adjacent cells.
fn braille_fills(c: char, size: Vec2d) -> Vec<(Rect, f32)> {
    let (step_x, step_y) = (size.x / 2.0, size.y / 4.0);
    let dot = (step_x.min(step_y) * 0.7).max(1.0);
    braille_dots(c)
        .map(|(row, column)| {
            let rect = Rect {
                pos: dvec2(
                    f64::from(column) * step_x + (step_x - dot) / 2.0,
                    f64::from(row) * step_y + (step_y - dot) / 2.0,
                ),
                size: dvec2(dot, dot),
            };
            (rect, 1.0)
        })
        .collect()
}

/// The lit dots of a Braille cell as (row, column) in a two by four grid.
///
/// The low eight bits of the codepoint are the pattern: dots one to three run
/// down the left column, four to six down the right, and seven and eight add
/// the fourth row — the order the standard assigns, not raster order.
fn braille_dots(c: char) -> impl Iterator<Item = (u8, u8)> {
    const DOTS: [(u32, u8, u8); 8] = [
        (0x01, 0, 0),
        (0x02, 1, 0),
        (0x04, 2, 0),
        (0x40, 3, 0),
        (0x08, 0, 1),
        (0x10, 1, 1),
        (0x20, 2, 1),
        (0x80, 3, 1),
    ];
    let pattern = c as u32 - 0x2800;
    DOTS.into_iter()
        .filter_map(move |(bit, row, column)| (pattern & bit != 0).then_some((row, column)))
}

/// `lpxs` moved down onto the device pixel grid at `dpi`, the way Ghostty and
/// Alacritty place their cells, so a stem or a row edge never lands between
/// two pixels.
fn snap(lpxs: f64, dpi: f64) -> f64 {
    (lpxs * dpi).floor() / dpi
}

impl Widget for TerminalView {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.scroll_bars.begin(cx, walk, self.layout);
        self.viewport_rect = cx.turtle().rect();
        self.unscrolled_rect = cx.turtle().rect_unscrolled();
        self.refresh_cell_metrics(cx);
        self.ime_pos = dvec2(self.pad_x, self.pad_y + self.cell_height);

        let window = crate::frame_state(scope)
            .map(|state| state.id)
            .unwrap_or_default();
        let session = self.session_for_widget(cx, window);

        self.draw_bg.draw_abs(cx, self.unscrolled_rect);

        let (mut total_lines, mut display_offset) = (0, 0);
        let mut entered_alternate_screen = false;
        if let Some(session) = session {
            crate::terminal::resize(session, self.size());
            if let Some(shared) = crate::terminal::term(session) {
                let term = shared.lock();
                total_lines = term.grid().total_lines();
                display_offset = term.grid().display_offset();
                let alternate_screen = term.mode().contains(TermMode::ALT_SCREEN);
                entered_alternate_screen = alternate_screen && !self.alternate_screen;
                self.alternate_screen = alternate_screen;
                let focused = cx.has_key_focus(self.scroll_bars.area());
                self.draw_grid(cx, &term, focused);
            }
        }

        // The bar follows the display: content is every line the grid holds,
        // and the offset counts up from the bottom.
        let (_, cell_height) = (self.cell_width, self.cell_height);
        let max_scroll = self.max_scroll(total_lines);
        let scroll_y = scroll_pos_for(display_offset, max_scroll, cell_height);
        let _ = self
            .scroll_bars
            .set_scroll_pos_no_clip(cx, dvec2(0.0, scroll_y));

        cx.turtle_mut().set_used(
            self.viewport_rect.size.x.max(1.0),
            max_scroll + self.viewport_rect.size.y,
        );
        self.scroll_bars.end(cx);
        if entered_alternate_screen {
            cx.set_key_focus(self.scroll_bars.area());
        }
        // Focus and the IME go together: the platform sends typed text to
        // whoever asked for it, so a focused terminal always asks.
        if session.is_some() && cx.has_key_focus(self.scroll_bars.area()) {
            cx.show_text_ime(self.scroll_bars.area(), self.ime_pos);
        }
        // NOTE: over the top rows rather than above them: giving the bar its
        // own room would resize the PTY each time find opens or closes.
        if let Some(find) = &self.find {
            find_row::show(&self.find_bar, cx, find.options(), find.invalid());
            self.find_bar.draw_walk(
                cx,
                scope,
                Walk {
                    abs_pos: Some(self.viewport_rect.pos),
                    width: crate::makepad_widgets::Size::Fixed(self.viewport_rect.size.x),
                    ..self.find_bar.walk
                },
            )?;
            if std::mem::take(&mut self.focus_find) {
                let input = self.find_bar.text_input(cx, ids!(find_input));
                input.set_text(cx, self.find.as_ref().map(Find::query).unwrap_or_default());
                input.set_key_focus(cx);
            }
        }
        self.link_tooltip.draw_walk(cx, scope, Walk::default())
    }

    #[expect(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "Focus, terminal modes, and scroll handling are ordered here."
    )]
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::KeyDown(key) | Event::KeyUp(key) = event {
            self.held_keys = key.modifiers;
        }
        if self.blink.see(cx, event) {
            self.draw_bg.redraw(cx);
        }
        let input = self.find_bar.text_input(cx, ids!(find_input));
        let focused = [self.scroll_bars.area(), input.area()]
            .into_iter()
            .any(|area| area != Area::Empty && cx.has_key_focus(area));
        if let Event::KeyDown(key) = event
            && focused
            && key.key_code == KeyCode::KeyF
            // NOTE: macOS finds on Cmd; Ctrl+F belongs to the program.
            && (key.modifiers.logo || (!cfg!(target_os = "macos") && key.modifiers.control))
        {
            self.find.get_or_insert_default();
            self.focus_find = true;
            self.redraw(cx);
            return;
        }
        // Escape and the bar's × both close find, and give the keyboard back
        // to the shell.
        let escaped =
            matches!(event, Event::KeyDown(key) if focused && key.key_code == KeyCode::Escape);
        let closed = matches!(event, Event::Actions(actions)
            if self.find_bar.button(cx, ids!(find_close)).clicked(actions));
        if self.find.is_some() && (escaped || closed) {
            self.find = None;
            self.find_regex = None;
            self.find_match = None;
            cx.set_key_focus(self.scroll_bars.area());
            self.redraw(cx);
            return;
        }
        if self.find.is_some() {
            self.find_bar.handle_event(cx, event, scope);
            if let Event::Actions(actions) = event {
                if let Some(find) = self
                    .find
                    .as_ref()
                    .and_then(|find| find_row::changed(&self.find_bar, cx, actions, find))
                {
                    let regex = find::pattern(find.query(), find.options(), find::ASCII_WORD)
                        .map_err(|error| error.to_string())
                        .and_then(|pattern| {
                            RegexSearch::new(&pattern).map_err(|error| error.to_string())
                        });
                    self.find_match = None;
                    self.find_regex = None;
                    match regex {
                        // NOTE: the bar itself says when a regex does not parse.
                        _ if find.query().is_empty() || find.invalid() => {
                            self.find_bar.label(cx, ids!(find_count)).set_text(cx, "");
                        }
                        Ok(regex) => {
                            self.find_regex = Some(regex);
                            self.seek_match(cx, Direction::Right);
                        }
                        Err(error) => self
                            .find_bar
                            .label(cx, ids!(find_count))
                            .set_text(cx, &format!("Cannot search: {error}")),
                    }
                    self.find = Some(find);
                    self.redraw(cx);
                }
                let back = self.find_bar.button(cx, ids!(find_prev)).clicked(actions);
                let walked = input
                    .returned(actions)
                    .map(|(_, modifiers)| modifiers.shift);
                let next = self.find_bar.button(cx, ids!(find_next)).clicked(actions);
                if let Some(back) = walked.or(back.then_some(true)).or(next.then_some(false)) {
                    self.seek_match(
                        cx,
                        if back {
                            Direction::Left
                        } else {
                            Direction::Right
                        },
                    );
                }
            }
        }

        let state = crate::frame_state(scope);
        let window = state.as_ref().map(|state| state.id).unwrap_or_default();
        let session = self.session_for_widget(cx, window);
        if self.find.is_none()
            && cx.key_focus() == Area::Empty
            && state
                .as_ref()
                .is_some_and(|state| state.terminal_tab() == session.map(|session| session.tab))
            && let Some((session, shared)) = session
                .and_then(|session| crate::terminal::term(session).map(|term| (session, term)))
        {
            let mode = *shared.lock().mode();
            match event {
                Event::KeyDown(e) => {
                    self.key_press(cx, session, e, mode);
                    return;
                }
                Event::KeyUp(e) => {
                    Self::key_release(session, e, mode);
                    return;
                }
                Event::TextInput(e) => {
                    self.text_typed(cx, session, e, mode);
                    return;
                }
                _ => {}
            }
        }

        self.link_tooltip.handle_event(cx, event, scope);
        let mut scroll_actions = Vec::new();
        self.scroll_bars
            .handle_main_event(cx, event, scope, &mut scroll_actions);
        let hit = event.hits(cx, self.scroll_bars.area());
        let drag = event.drag_hits(cx, self.scroll_bars.area());
        if matches!(hit, Hit::Nothing)
            && matches!(drag, DragHit::NoHit)
            && scroll_actions.is_empty()
        {
            return;
        }
        let Some((session, shared)) =
            session.and_then(|s| crate::terminal::term(s).map(|term| (s, term)))
        else {
            self.scroll_bars
                .handle_scroll_event(cx, event, scope, &mut scroll_actions);
            return;
        };

        let mode = *shared.lock().mode();
        if self.find.is_some() {
            self.text_forwarded = false;
        }

        if self.handle_drop(cx, session, drag, mode) {
            return;
        }

        // The wheel belongs to the application while it is tracking the
        // pointer, or on the alt screen; otherwise it scrolls our scrollback.
        if let Event::Scroll(e) = event
            && self.scroll_bars.area().clipped_rect(cx).contains(e.abs)
        {
            let (_, cell_height) = (self.cell_width, self.cell_height);
            self.scroll_accum += e.scroll.y;
            let lines = (self.scroll_accum / cell_height).abs() as usize;
            if lines > 0 {
                let up = self.scroll_accum < 0.0;
                let bytes = {
                    let term = shared.lock();
                    let (point, _) = self.point_at(e.abs, &term);
                    mouse::wheel(up, lines, e.modifiers, point, *term.mode())
                };
                match bytes {
                    Some(bytes) => {
                        self.scroll_accum -=
                            self.scroll_accum.signum() * lines as f64 * cell_height;
                        crate::terminal::input(session, bytes);
                        e.handled_y.set(true);
                        self.draw_bg.redraw(cx);
                        return;
                    }
                    // The bar below takes this one, so what is left of a
                    // line here would only fire late.
                    None => self.scroll_accum = 0.0,
                }
            }
        }

        self.scroll_bars
            .handle_scroll_event(cx, event, scope, &mut scroll_actions);
        if !scroll_actions.is_empty() {
            // A drag or a wheel the bar took: turn the new position back into
            // a display offset, which is where the scrollback really lives.
            let (_, cell_height) = (self.cell_width, self.cell_height);
            let mut term = shared.lock();
            let max_scroll = self.max_scroll(term.grid().total_lines());
            let wanted =
                display_offset_for(self.scroll_bars.get_scroll_pos().y, max_scroll, cell_height)
                    .min(term.history_size());
            let delta = wanted as i32 - term.grid().display_offset() as i32;
            if delta != 0 {
                term.scroll_display(Scroll::Delta(delta));
            }
            drop(term);
            self.draw_bg.redraw(cx);
        }

        match hit {
            Hit::FingerDown(e) => {
                cx.set_key_focus(self.scroll_bars.area());
                if let Some(state) = &state {
                    state.set_terminal_tab(Some(session.tab));
                }
                self.held = e.device.mouse_button();
                if e.modifiers.logo
                    && let Some(target) =
                        self.link_at(&shared, e.abs, e.modifiers, state.as_deref())
                {
                    match target {
                        crate::links::Target::External(url) => {
                            cx.open_url(url.as_str(), OpenUrlInPlace::No);
                        }
                        crate::links::Target::File(path) => {
                            cx.widget_action(self.widget_uid(), TerminalViewAction::OpenFile(path));
                        }
                    }
                    return;
                }

                let bytes = {
                    let mut term = shared.lock();
                    let (point, side) = self.point_at(e.abs, &term);
                    let mode = *term.mode();
                    if let Some(button) = self
                        .held
                        .filter(|_| mouse::wants_pointer(e.modifiers, mode))
                    {
                        mouse::report(button, true, e.modifiers, point, mode)
                    } else {
                        let ty = if e.modifiers.control && e.modifiers.alt {
                            SelectionType::Block
                        } else {
                            match e.tap_count {
                                1 => SelectionType::Simple,
                                2 => SelectionType::Semantic,
                                _ => SelectionType::Lines,
                            }
                        };
                        self.selecting = true;
                        term.selection = Some(Selection::new(ty, point, side));
                        None
                    }
                };
                if let Some(bytes) = bytes {
                    crate::terminal::input(session, bytes);
                }
                self.draw_bg.redraw(cx);
            }
            Hit::FingerMove(e) => {
                cx.set_cursor(MouseCursor::Text);
                let bytes = {
                    let mut term = shared.lock();
                    let (point, side) = self.point_at(e.abs, &term);
                    if self.selecting {
                        if let Some(selection) = term.selection.as_mut() {
                            selection.update(point, side);
                        }
                        None
                    } else {
                        mouse::motion(self.held, e.modifiers, point, *term.mode())
                    }
                };
                if self.selecting {
                    self.draw_bg.redraw(cx);
                }
                if let Some(bytes) = bytes {
                    crate::terminal::input(session, bytes);
                }
            }
            Hit::FingerUp(e) => {
                let bytes = {
                    let term = shared.lock();
                    let (point, _) = self.point_at(e.abs, &term);
                    let mode = *term.mode();
                    self.held
                        .filter(|_| mouse::wants_pointer(e.modifiers, mode))
                        .and_then(|button| mouse::report(button, false, e.modifiers, point, mode))
                };
                if let Some(bytes) = bytes {
                    crate::terminal::input(session, bytes);
                }
                self.selecting = false;
                self.held = None;
            }
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let target = self.link_at(&shared, e.abs, e.modifiers, state.as_deref());
                if let Some(target) = &target {
                    let label = match target {
                        crate::links::Target::External(url) => url.as_str(),
                        crate::links::Target::File(path) => path,
                    };
                    let pos = dvec2(
                        e.abs
                            .x
                            .min(self.viewport_rect.pos.x + self.viewport_rect.size.x - 320.0)
                            .max(self.viewport_rect.pos.x),
                        (e.abs.y + 20.0)
                            .min(self.viewport_rect.pos.y + self.viewport_rect.size.y - 70.0),
                    );
                    self.link_tooltip.show_with_options(cx, pos, label);
                } else {
                    self.link_tooltip.hide(cx);
                }
                cx.set_cursor(if e.modifiers.logo && target.is_some() {
                    MouseCursor::Hand
                } else {
                    MouseCursor::Text
                });
            }
            Hit::FingerHoverOut(_) => self.link_tooltip.hide(cx),
            Hit::KeyFocus(_) => {
                if let Some(state) = &state {
                    state.set_terminal_tab(Some(session.tab));
                }
                if let Some(report) = keys::focus_report(true, mode) {
                    crate::terminal::input(session, report.to_vec());
                }
                self.draw_bg.redraw(cx);
            }
            Hit::KeyFocusLost(_) => {
                // NOTE: the newly focused input owns the IME now.
                if let Some(report) = keys::focus_report(false, mode) {
                    crate::terminal::input(session, report.to_vec());
                }
                self.draw_bg.redraw(cx);
            }
            Hit::KeyDown(e) => self.key_press(cx, session, &e, mode),
            Hit::KeyUp(e) => Self::key_release(session, &e, mode),
            Hit::TextInput(e) => self.text_typed(cx, session, &e, mode),
            Hit::TextCopy(e) => {
                *e.response.borrow_mut() = shared.lock().selection_to_string();
            }
            _ => {}
        }
    }
}

/// Whether a dragged item comes from outside the app. A dock tab drags as an
/// empty path carrying its id, and is not text for the shell.
// NOTE: handle_drop passes it to any(), a use coland does not count.
// coland-ignore: @coland/single_caller_private_function
fn from_outside(item: &DragItem) -> bool {
    matches!(
        item,
        DragItem::FilePath {
            internal_id: None,
            ..
        } | DragItem::String {
            internal_id: None,
            ..
        }
    )
}

fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        // NOTE: removing ESC and ETX prevents pasted text from ending the
        // bracketed payload or sending control commands to the foreground app.
        format!("\x1b[200~{}\x1b[201~", text.replace(['\x1b', '\x03'], "")).into_bytes()
    } else {
        text.replace("\r\n", "\n").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::{term::Config, vte::ansi::Processor};

    use super::*;
    use crate::find::Options;

    #[test]
    fn terminal_search_visits_scrollback_and_wraps_in_both_directions() {
        let size = Size {
            columns: 40,
            screen_lines: 3,
            cell_width: 8,
            cell_height: 16,
        };
        let mut term = Term::new(
            Config::default(),
            &size,
            alacritty_terminal::event::VoidListener,
        );
        let mut parser: Processor = Processor::default();
        parser.advance(
            &mut term,
            "[needle] 界\r\nother\r\n[needle] again\r\nfiller\r\nfiller\r\nfiller\r\n".as_bytes(),
        );
        let mut regex = RegexSearch::new(r"\[needle\]").unwrap();
        let first = next_terminal_match(&term, &mut regex, None, Direction::Right).unwrap();
        assert!(first.start().line.0 < 0, "the match is in scrollback");
        assert_eq!(
            term.bounds_to_string(*first.start(), *first.end()),
            "[needle]"
        );
        let second =
            next_terminal_match(&term, &mut regex, Some(&first), Direction::Right).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            next_terminal_match(&term, &mut regex, Some(&second), Direction::Right),
            Some(first.clone())
        );
        assert_eq!(
            next_terminal_match(&term, &mut regex, Some(&first), Direction::Left),
            Some(second)
        );
        let mut missing = RegexSearch::new("absent").unwrap();
        assert!(next_terminal_match(&term, &mut missing, None, Direction::Right).is_none());
    }

    #[test]
    fn terminal_whole_word_and_case_follow_the_find_options() {
        let size = Size {
            columns: 40,
            screen_lines: 3,
            cell_width: 8,
            cell_height: 16,
        };
        let mut term = Term::new(
            Config::default(),
            &size,
            alacritty_terminal::event::VoidListener,
        );
        let mut parser: Processor = Processor::default();
        parser.advance(&mut term, "filler\r\n".as_bytes());
        let options = Options {
            case: true,
            word: true,
            regex: false,
        };
        let found = |query: &str| {
            let pattern = find::pattern(query, options, find::ASCII_WORD).unwrap();
            let mut regex = RegexSearch::new(&pattern).unwrap();
            next_terminal_match(&term, &mut regex, None, Direction::Right).is_some()
        };
        assert!(found("filler"));
        assert!(!found("fill"), "only a whole word");
        assert!(!found("FILLER"), "only the typed case");
        assert!(
            RegexSearch::new(r"\bfiller\b").is_err(),
            "the terminal's search cannot bound a Unicode word"
        );
    }

    #[test]
    fn cells_snap_down_onto_device_pixels() {
        assert_eq!(snap(7.8, 2.0), 7.5);
        assert_eq!(snap(15.84, 2.0), 15.5);
        assert_eq!(snap(7.8, 1.0), 7.0);
        assert_eq!(snap(8.0, 2.0), 8.0);
    }

    /// A cell of 8 by 17 points at 2x: 16 by 34 device pixels, a light stroke
    /// 2 wide.
    const SIZE: Vec2d = Vec2d { x: 8.0, y: 17.0 };

    /// The fills of `c` in device pixels, as (x0, y0, x1, y1, ink).
    fn device_fills(c: char) -> Vec<(f64, f64, f64, f64, f32)> {
        fills(c, SIZE, 2.0)
            .unwrap()
            .into_iter()
            .map(|(r, ink)| {
                let (p, s) = (r.pos * 2.0, r.size * 2.0);
                (p.x, p.y, p.x + s.x, p.y + s.y, ink)
            })
            .collect()
    }

    #[test]
    fn blocks_fill_whole_device_pixels_to_the_cell_edges() {
        assert_eq!(device_fills('█'), [(0.0, 0.0, 16.0, 34.0, 1.0)]);
        assert_eq!(device_fills('▀'), [(0.0, 0.0, 16.0, 17.0, 1.0)]);
        assert_eq!(device_fills('▄'), [(0.0, 17.0, 16.0, 34.0, 1.0)]);
        assert_eq!(device_fills('▌'), [(0.0, 0.0, 8.0, 34.0, 1.0)]);
        assert_eq!(device_fills('▐'), [(8.0, 0.0, 16.0, 34.0, 1.0)]);
        assert_eq!(device_fills('▁'), [(0.0, 30.0, 16.0, 34.0, 1.0)]);
        assert_eq!(device_fills('▏'), [(0.0, 0.0, 2.0, 34.0, 1.0)]);
        assert_eq!(device_fills('▒'), [(0.0, 0.0, 16.0, 34.0, 0.5)]);
        assert_eq!(
            device_fills('▚'),
            [(0.0, 0.0, 8.0, 17.0, 1.0), (8.0, 17.0, 16.0, 34.0, 1.0)]
        );
    }

    #[test]
    fn box_lines_cross_the_cell_and_close_their_corners() {
        // A straight line runs edge to edge, centred, so neighbours join.
        assert_eq!(
            device_fills('─'),
            [(0.0, 16.0, 8.0, 18.0, 1.0), (8.0, 16.0, 16.0, 18.0, 1.0)]
        );
        assert_eq!(
            device_fills('│'),
            [(7.0, 0.0, 9.0, 17.0, 1.0), (7.0, 17.0, 9.0, 34.0, 1.0)]
        );
        assert_eq!(
            device_fills('━')[0].1..device_fills('━')[0].3,
            15.0..19.0,
            "heavy is twice light"
        );
        // A corner's arms overlap where they meet: no notch.
        let corner = device_fills('┍');
        let (down, right) = (corner[0], corner[1]);
        assert!(down.1 <= right.1 && right.0 <= down.0, "{corner:?}");
        assert!(fills('a', SIZE, 2.0).is_none());
    }

    #[test]
    fn dashes_keep_an_even_rhythm_across_cells() {
        // ┄ on ─'s row: dashes of 4, 3 and 3 with gaps of 2, half a gap at
        // each edge, so the gap between two cells is a whole one.
        assert_eq!(
            device_fills('┄'),
            [
                (1.0, 16.0, 5.0, 18.0, 1.0),
                (7.0, 16.0, 10.0, 18.0, 1.0),
                (12.0, 16.0, 15.0, 18.0, 1.0),
            ]
        );
        // ┆ in │'s column, its gaps 4 tall, top and bottom edge together too.
        let down = device_fills('┆');
        assert_eq!(
            down.iter().map(|d| (d.1, d.3)).collect::<Vec<_>>(),
            [(2.0, 10.0), (14.0, 21.0), (25.0, 32.0)]
        );
        assert!(down.iter().all(|d| (d.0, d.2) == (7.0, 9.0)));
        assert_eq!(device_fills('┉').len(), 4);
        assert_eq!(
            (device_fills('┉')[0].1, device_fills('┉')[0].3),
            (15.0, 19.0)
        );
        assert_eq!(device_fills('╎').len(), 2);
    }

    #[test]
    fn a_double_corner_closes_inside_and_out() {
        // ╔ in a 16 by 34 cell: strokes 2 wide, the band 6 wide.
        let corner = device_fills('╔');
        assert_eq!(
            corner,
            [
                (5.0, 14.0, 7.0, 34.0, 1.0),
                (9.0, 18.0, 11.0, 34.0, 1.0),
                (5.0, 14.0, 16.0, 16.0, 1.0),
                (9.0, 18.0, 16.0, 20.0, 1.0),
            ],
            "outer strokes meet at the band's corner, inner ones further in"
        );
        // ═ runs edge to edge in both strokes, so neighbours join.
        let line = device_fills('═');
        assert_eq!((line[0].0, line[3].2), (0.0, 16.0));
    }

    #[test]
    fn a_rounded_corner_meets_the_lines_beside_it() {
        let corner = device_fills('╭');
        // Straight down into the cell below, where │ runs.
        assert_eq!(corner.last(), Some(&(7.0, 33.0, 9.0, 34.0, 1.0)));
        // Out to the right edge on the rows ─ runs on.
        let right = |row: f64| corner.iter().find(|f| (f.1 - row).abs() < 0.5).map(|f| f.2);
        assert_eq!((right(16.0), right(17.0)), (Some(16.0), Some(16.0)));
        assert!(corner.iter().all(|f| f.1 >= 16.0), "nothing above the line");
        let other = device_fills('╯');
        assert_eq!(other.first(), Some(&(7.0, 0.0, 9.0, 1.0, 1.0)));
    }

    #[test]
    fn a_diagonal_runs_corner_to_corner() {
        let rising = device_fills('╱');
        assert_eq!(rising.len(), 34, "one row per device pixel");
        assert_eq!(rising[0], (14.0, 0.0, 16.0, 1.0, 1.0));
        assert_eq!(rising[33], (0.0, 33.0, 2.0, 34.0, 1.0));
        let falling = device_fills('╲');
        assert_eq!((falling[0].0, falling[33].2), (0.0, 16.0));
        assert_eq!(device_fills('╳').len(), 68, "both strokes on every row");
    }

    #[test]
    fn a_powerline_arrow_reaches_across_the_cell_at_its_middle() {
        let arrow = device_fills('\u{e0b0}');
        assert_eq!(arrow.len(), 34, "one row per device pixel");
        assert_eq!(arrow[17].2, 16.0);
        assert!(
            arrow[0].2 <= 1.0 && arrow[33].2 <= 1.0,
            "{:?}",
            (arrow[0], arrow[33])
        );
        assert_eq!(device_fills('\u{e0b2}')[17].0, 0.0);
    }

    #[test]
    fn a_rounded_powerline_cap_is_half_an_ellipse() {
        let cap = device_fills('\u{e0b4}');
        assert_eq!(cap[17], (0.0, 17.0, 16.0, 18.0, 1.0), "the whole cell");
        assert_eq!(cap[0], (0.0, 0.0, 4.0, 1.0, 1.0));
        assert_eq!(device_fills('\u{e0b6}')[0], (12.0, 0.0, 16.0, 1.0, 1.0));
        // An outline is a light line thick where it crosses the middle.
        assert_eq!(device_fills('\u{e0b5}')[17], (14.0, 17.0, 16.0, 18.0, 1.0));
    }

    #[test]
    fn a_slanted_separator_fills_its_corner_to_the_diagonal() {
        let lower_left = device_fills('\u{e0b8}');
        assert_eq!(lower_left.len(), 34, "one row per device pixel");
        assert_eq!((lower_left[0].2, lower_left[33].2), (0.0, 16.0));
        let upper_right = device_fills('\u{e0be}');
        assert_eq!((upper_right[0].0, upper_right[33].0), (0.0, 16.0));
        // Each row of the two together covers the cell exactly once.
        let rows: Vec<_> = lower_left
            .iter()
            .zip(&upper_right)
            .map(|(a, b)| (a.0, a.2 - b.0, b.2))
            .collect();
        assert_eq!(rows, vec![(0.0, 0.0, 16.0); 34]);
        assert_eq!(device_fills('\u{e0bb}'), device_fills('╱'));
        assert_eq!(device_fills('\u{e0bf}'), device_fills('╲'));
    }

    #[test]
    fn each_underline_style_has_its_own_rule() {
        let rules = |flags| {
            underline_fills(flags, SIZE, 2.0)
                .into_iter()
                .map(|r| {
                    let (p, s) = (r.pos * 2.0, r.size * 2.0);
                    (p.x, p.y, p.x + s.x, p.y + s.y)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(rules(Flags::UNDERLINE), [(0.0, 30.0, 16.0, 32.0)]);
        assert_eq!(
            rules(Flags::DOUBLE_UNDERLINE),
            [(0.0, 30.0, 16.0, 32.0), (0.0, 26.0, 16.0, 28.0)]
        );
        assert_eq!(rules(Flags::DASHED_UNDERLINE), [(0.0, 30.0, 8.0, 32.0)]);
        let dots = rules(Flags::DOTTED_UNDERLINE);
        assert_eq!(
            dots.iter().map(|d| d.0).collect::<Vec<_>>(),
            [0.0, 4.0, 8.0, 12.0]
        );
        let curl = rules(Flags::UNDERCURL);
        assert_eq!(curl.len(), 16, "one column per device pixel");
        assert!(curl.iter().all(|c| c.1 >= 28.0 && c.3 <= 34.0), "{curl:?}");
        assert!(curl.iter().any(|c| c.1 < 30.0) && curl.iter().any(|c| c.1 > 30.0));
        assert_eq!(rules(Flags::empty()), []);
    }

    #[test]
    fn braille_reads_its_dots_off_the_codepoint() {
        let dots = |c| braille_dots(c).collect::<Vec<_>>();
        // One dot in each corner of the two by four grid.
        assert_eq!(dots('\u{2801}'), [(0, 0)]);
        assert_eq!(dots('\u{2808}'), [(0, 1)]);
        assert_eq!(dots('\u{2840}'), [(3, 0)]);
        assert_eq!(dots('\u{2880}'), [(3, 1)]);
        // The blank cell draws nothing, the full one all eight.
        assert_eq!(dots('\u{2800}'), [] as [(u8, u8); 0]);
        assert_eq!(dots('\u{28ff}').len(), 8);
    }

    #[test]
    fn bracketed_paste_cannot_inject_nested_markers_or_controls() {
        assert_eq!(
            paste_bytes("\x1b\x1b[201~[201~\x03echo hello\n", true),
            b"\x1b[200~[201~[201~echo hello\n\x1b[201~"
        );
    }

    #[test]
    fn plain_paste_sends_one_return_per_line_break() {
        assert_eq!(
            paste_bytes("one\r\ntwo\nthree\r", false),
            b"one\rtwo\rthree\r"
        );
    }

    #[test]
    fn dropped_paths_keep_literal_percent_sequences() {
        assert_eq!(
            TerminalView::dropped_text_payload(&[DragItem::FilePath {
                path: "/tmp/it's%41.txt".into(),
                internal_id: None,
            }]),
            Some("'/tmp/it'\\''s%41.txt' ".into())
        );
    }

    #[test]
    fn a_dragged_dock_tab_is_not_pasted() {
        let tab = DragItem::FilePath {
            path: String::new(),
            internal_id: Some(LiveId(1)),
        };
        assert_eq!(TerminalView::dropped_text_payload(&[tab]), None);
    }

    /// The bar and the scrollback are the same number in two units, so the
    /// trip out and back has to land where it started. It did not: the offset
    /// used to be derived from the bar's own position, which made it always
    /// zero, and every scroll snapped to the bottom.
    #[test]
    fn the_bar_and_the_scrollback_agree_in_both_directions() {
        let (cell_height, max_scroll) = (15.0, 1500.0);
        for offset in [0usize, 1, 7, 50, 99, 100] {
            let pos = scroll_pos_for(offset, max_scroll, cell_height);
            assert_eq!(display_offset_for(pos, max_scroll, cell_height), offset);
        }
        // The foot of the bar is the live screen, its head the whole history.
        assert_eq!(scroll_pos_for(0, max_scroll, cell_height), max_scroll);
        assert_eq!(display_offset_for(max_scroll, max_scroll, cell_height), 0);
        assert_eq!(display_offset_for(0.0, max_scroll, cell_height), 100);
        // Half way up the bar is half the history back, not the bottom.
        assert_eq!(
            display_offset_for(max_scroll / 2.0, max_scroll, cell_height),
            50
        );
    }
}
