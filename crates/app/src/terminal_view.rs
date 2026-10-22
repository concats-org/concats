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
    makepad_widgets::{text::geom::Point as TextPoint, *},
    terminal::{Proxy, Session, SharedTerm, Size, blink::Blink, colors, keys, mouse},
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
    // `mod.app_font`, at the terminal's own size, so the cell grid stays put
    // when the app font size changes. (This module's script scope can't see the
    // `FONT` `let` in `main.rs`.) Braille is drawn, not shaped — `draw_braille`.
    let TERM_FONT = TextStyle{
        font_family: FontFamily{
            first := FontMember{res: mod.app_font.first asc: 0.0 desc: 0.0}
            second := FontMember{res: mod.app_font.second asc: 0.0 desc: 0.0}
            third := FontMember{res: mod.app_font.third asc: 0.0 desc: 0.0}
            fourth := FontMember{res: mod.app_font.fourth asc: 0.0 desc: 0.0}
            mono := FontMember{res: mod.app_font.mono asc: 0.0 desc: 0.0}
            cjk := FontMember{res: mod.app_font.cjk asc: 0.0 desc: 0.0}
            emoji := FontMember{res: mod.app_font.emoji asc: 0.0 desc: 0.0}
        }
        font_size: 9
        // A terminal cell is the glyph box, with no leading added: box-drawing
        // characters are cut to fill that box exactly, so any extra spacing
        // opens gaps between the rows of a frame an application draws, and the
        // grid reads as stretched. JetBrains Mono is 1.32 em tall over a 0.6 em
        // advance, so this lands at the 2.2:1 cell a terminal expects.
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
        find_bar: mod.widgets.TabBar {
            find_row := mod.widgets.FindRow {
                find_input +: { empty_text: "Find in terminal" }
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

/// The Braille block, which an agent's spinner is made of.
fn is_braille(c: char) -> bool {
    ('\u{2800}'..='\u{28ff}').contains(&c)
}

/// The lit dots of a Braille cell as (row, column) in a two by four grid.
///
/// The low eight bits of the codepoint are the pattern: dots one to three run
/// down the left column, four to six down the right, and seven and eight add
/// the fourth row — the order the standard assigns, not raster order.
fn braille_dots(c: char) -> impl Iterator<Item = (usize, usize)> {
    const DOTS: [(u32, usize, usize); 8] = [
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
    find: Option<String>,
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
    /// Shaped cells by text and face (bold, italic). Every face is the one
    /// font today.
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
        self.cell_width = f64::from(glyph.advance_in_lpxs() * font_scale);
        self.cell_height = f64::from(glyph_height * self.draw_text.text_style.line_spacing);
        self.cell_offset_y = ((self.cell_height - f64::from(glyph_height)) * 0.5).max(0.0);
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
            self.unscrolled_rect.pos.x + self.pad_x,
            self.unscrolled_rect.pos.y + self.pad_y,
        )
    }

    fn cache_terminal_glyphs(&mut self, cx: &mut Cx2d, key: &(String, Flags)) {
        if self.glyph_cache.contains_key(key) {
            return;
        }
        let Some(run) = self.draw_text.prepare_single_line_run(cx, &key.0) else {
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
            if is_braille(indexed.cell.c) && !flags.contains(Flags::HIDDEN) {
                self.draw_cell_bg.color = fg;
                self.draw_braille(cx, indexed.cell.c, x, y);
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
                    let baseline_y = y + self.cell_offset_y + f64::from(run.ascender_in_lpxs);
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
                if flags.intersects(Flags::ALL_UNDERLINES) {
                    self.draw_rule(cx, x, y + cell_height - RULE_HEIGHT * 2.0, width);
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

    /// Braille is drawn rather than shaped. No font we bundle carries the
    /// block, the one on this platform that does is a proportional symbol face
    /// whose dots do not line up between adjacent cells — and the pattern is
    /// already in the codepoint. Alacritty draws its own for the same reasons.
    fn draw_braille(&mut self, cx: &mut Cx2d, c: char, x: f64, y: f64) {
        let (cell_width, cell_height) = (self.cell_width, self.cell_height);
        let (step_x, step_y) = (cell_width / 2.0, cell_height / 4.0);
        let dot = (step_x.min(step_y) * 0.7).max(1.0);
        for (row, column) in braille_dots(c) {
            self.draw_cell_bg.draw_abs(
                cx,
                Rect {
                    pos: dvec2(
                        x + column as f64 * step_x + (step_x - dot) / 2.0,
                        y + row as f64 * step_y + (step_y - dot) / 2.0,
                    ),
                    size: dvec2(dot, dot),
                },
            );
        }
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
        if self.find.is_some() {
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
                input.set_text(cx, self.find.as_deref().unwrap_or_default());
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
                if let Some(query) = input.changed(actions) {
                    self.find_match = None;
                    self.find_regex = None;
                    if query.is_empty() {
                        self.find_bar.label(cx, ids!(find_count)).set_text(cx, "");
                    } else {
                        match RegexSearch::new(&format!("(?i:{})", regex::escape(&query))) {
                            Ok(regex) => {
                                self.find_regex = Some(regex);
                                self.seek_match(cx, Direction::Right);
                            }
                            Err(error) => self
                                .find_bar
                                .label(cx, ids!(find_count))
                                .set_text(cx, &format!("Cannot search: {error}")),
                        }
                    }
                    self.find = Some(query);
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

    fn dots(c: char) -> Vec<(usize, usize)> {
        braille_dots(c).collect()
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

    #[test]
    fn braille_reads_its_dots_off_the_codepoint() {
        assert!(is_braille('\u{2801}') && !is_braille('a'));
        // One dot in each corner of the two by four grid.
        assert_eq!(dots('\u{2801}'), [(0, 0)]);
        assert_eq!(dots('\u{2808}'), [(0, 1)]);
        assert_eq!(dots('\u{2840}'), [(3, 0)]);
        assert_eq!(dots('\u{2880}'), [(3, 1)]);
        // The blank cell draws nothing, the full one all eight.
        assert_eq!(dots('\u{2800}'), []);
        assert_eq!(dots('\u{28ff}').len(), 8);
        // Six dots, the top three rows of both columns.
        assert_eq!(
            dots('\u{283f}'),
            [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)]
        );
    }
}
