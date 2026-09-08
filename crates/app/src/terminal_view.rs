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
//! TODO: find in the scrollback. `alacritty_terminal::term::search` is the
//! whole mechanism, but the panel should reuse the review list's find rather
//! than grow a second one — and that interaction wants rework of its own
//! first, which is not this change's to do.

use std::collections::HashMap;

use alacritty_terminal::{
    Term,
    grid::{Dimensions, Scroll},
    index::{Column, Point, Side},
    selection::{Selection, SelectionType},
    term::{TermMode, cell::Flags, point_to_viewport, viewport_to_point},
    vte::ansi::CursorShape,
};

use crate::{
    makepad_widgets::{
        text::{geom::Point as TextPoint, rasterizer::RasterizedGlyph},
        *,
    },
    terminal::{Proxy, Session, Size, colors, keys, mouse},
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
        color_unfocused: mod.app_theme.color_cursor
        focus: 0.0
        border_width: 1.0
        pixel: fn() {
            if self.focus > 0.5 {
                return vec4(self.color.rgb * self.color.a, self.color.a)
            }
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let inset = self.border_width * 0.5
            let color = self.color_unfocused
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
        font_size: 9.0
        cell_width_factor: 0.6
        cell_height_factor: 1.32
        pad_x: 6.0
        pad_y: 4.0
        text_y_offset: 0.0
        cursor_y_offset: 0.0
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
        draw_cell_bg +: {}
        draw_cursor +: {}
    }
}

#[derive(Clone, Debug, Default)]
pub enum TerminalViewAction {
    Input {
        session: Session,
        data: Vec<u8>,
    },
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
    color_unfocused: Vec4f,
    #[live]
    focus: f32,
    #[live]
    border_width: f32,
}

#[derive(Clone, Copy)]
struct CachedTerminalGlyph {
    rasterized: RasterizedGlyph,
    font_size_in_lpxs: f32,
    x_offset_in_lpxs: f32,
    baseline_offset_in_lpxs: f32,
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
pub struct TerminalView {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    scroll_bars: ScrollBars,
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
    #[live(9.0)]
    font_size: f64,
    #[live(0.6)]
    cell_width_factor: f64,
    #[live(1.32)]
    cell_height_factor: f64,
    #[live(4.0)]
    pad_x: f64,
    #[live(2.0)]
    pad_y: f64,
    #[live(0.0)]
    text_y_offset: f64,
    #[live(0.0)]
    cursor_y_offset: f64,
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
    glyph_cache: HashMap<char, CachedTerminalGlyph>,
    #[rust]
    glyph_cache_font_size: f32,
    #[rust]
    glyph_cache_font_scale: f32,
    #[rust]
    glyph_cache_dpi_factor: f64,
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
    #[rust]
    ime_pos: Option<Vec2d>,
}

impl ScriptHook for TerminalView {}

impl TerminalView {
    /// Which terminal session this widget instance shows: the nearest
    /// enclosing dock tab with a live session (the widget tree path contains
    /// the dock item ids).
    ///
    /// `window` comes from the scope the pane put there. The tab ids are the
    /// same in every window, so without it this resolves to whichever window
    /// opened that tab first.
    fn session_for_widget(cx: &Cx, widget_uid: WidgetUid, window: LiveId) -> Option<Session> {
        cx.widget_tree()
            .path_to(widget_uid)
            .iter()
            .rev()
            .map(|node| Session { window, tab: *node })
            .find(|session| crate::terminal::is_open(*session))
    }

    fn fallback_cell_metrics(&self) -> (f64, f64) {
        let w = (self.font_size * self.cell_width_factor).max(1.0);
        let h = (self.font_size * self.cell_height_factor).max(1.0);
        (w, h)
    }

    fn refresh_cell_metrics(&mut self, cx: &mut Cx2d) {
        self.draw_text.text_style.font_size = self.font_size as f32;
        let (fallback_w, fallback_h) = self.fallback_cell_metrics();

        let layout = self
            .draw_text
            .layout(cx, 0.0, 0.0, None, false, Align::default(), "M");
        let Some(first_glyph) = layout.rows.first().and_then(|row| row.glyphs.first()) else {
            self.cell_width = fallback_w;
            self.cell_height = fallback_h;
            self.cell_offset_y = 0.0;
            return;
        };

        let width_in_lpxs = first_glyph.advance_in_lpxs();
        let glyph_h_in_lpxs = first_glyph.ascender_in_lpxs() - first_glyph.descender_in_lpxs();
        let line_spacing_in_lpxs = glyph_h_in_lpxs * self.draw_text.text_style.line_spacing;

        self.cell_width = if width_in_lpxs > 0.0 {
            width_in_lpxs as f64
        } else {
            fallback_w
        };
        self.cell_height = if line_spacing_in_lpxs > 0.0 {
            line_spacing_in_lpxs as f64
        } else {
            fallback_h
        };
        self.cell_offset_y = ((self.cell_height - glyph_h_in_lpxs as f64) * 0.5).max(0.0);
    }

    fn cell_metrics(&self) -> (f64, f64) {
        let (fallback_w, fallback_h) = self.fallback_cell_metrics();
        (
            if self.cell_width > 0.0 {
                self.cell_width
            } else {
                fallback_w
            },
            if self.cell_height > 0.0 {
                self.cell_height
            } else {
                fallback_h
            },
        )
    }

    /// The widget's geometry in cells, for the term and the PTY.
    fn size(&self) -> Size {
        let (cell_width, cell_height) = self.cell_metrics();
        Size {
            columns: ((self.viewport_rect.size.x - self.pad_x * 2.0) / cell_width)
                .floor()
                .max(1.0) as usize,
            screen_lines: ((self.viewport_rect.size.y - self.pad_y * 2.0) / cell_height)
                .floor()
                .max(1.0) as usize,
            cell_width: cell_width.round().max(1.0) as u16,
            cell_height: cell_height.round().max(1.0) as u16,
        }
    }

    /// How far the scroll bar can travel over a grid of `total_lines`: the
    /// content it stands for, less the part already on screen.
    fn max_scroll(&self, total_lines: usize) -> f64 {
        let (_, cell_height) = self.cell_metrics();
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

    fn invalidate_glyph_cache_if_needed(&mut self, cx: &Cx2d) {
        let font_size = self.draw_text.text_style.font_size;
        let font_scale = self.draw_text.font_scale;
        let dpi_factor = cx.current_dpi_factor();
        if self.glyph_cache_font_size.to_bits() == font_size.to_bits()
            && self.glyph_cache_font_scale.to_bits() == font_scale.to_bits()
            && self.glyph_cache_dpi_factor.to_bits() == dpi_factor.to_bits()
        {
            return;
        }
        self.glyph_cache.clear();
        self.glyph_cache_font_size = font_size;
        self.glyph_cache_font_scale = font_scale;
        self.glyph_cache_dpi_factor = dpi_factor;
    }

    fn cached_terminal_glyph(&mut self, cx: &mut Cx2d, ch: char) -> Option<CachedTerminalGlyph> {
        if let Some(cached) = self.glyph_cache.get(&ch) {
            return Some(*cached);
        }
        let mut utf8 = [0u8; 4];
        let text = ch.encode_utf8(&mut utf8);
        let run = self.draw_text.prepare_single_line_run(cx, text)?;
        let glyph = run.glyphs.first()?;
        let cached = CachedTerminalGlyph {
            rasterized: glyph.rasterized,
            font_size_in_lpxs: glyph.font_size_in_lpxs,
            x_offset_in_lpxs: glyph.pen_x_in_lpxs + glyph.offset_x_in_lpxs,
            baseline_offset_in_lpxs: run.ascender_in_lpxs,
        };
        self.glyph_cache.insert(ch, cached);
        Some(cached)
    }

    /// Walk the visible grid: a background where a cell asked for one, the
    /// glyph, whatever rules its flags call for, and the cursor on top.
    fn draw_grid(&mut self, cx: &mut Cx2d, term: &Term<Proxy>, focused: bool) {
        let theme = crate::theme::active_theme();
        let content = term.renderable_content();
        let (display_offset, cursor, selection) =
            (content.display_offset, content.cursor, content.selection);
        let (cell_width, cell_height) = self.cell_metrics();
        let origin = self.origin();

        self.draw_cell_bg.new_draw_call(cx);
        self.draw_cursor.new_draw_call(cx);
        self.draw_text.new_draw_call(cx);
        self.draw_text.begin_many_instances(cx);
        self.invalidate_glyph_cache_if_needed(cx);

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
                    colors::foreground(
                        alacritty_terminal::vte::ansi::Color::Named(
                            alacritty_terminal::vte::ansi::NamedColor::Background,
                        ),
                        Flags::empty(),
                        content.colors,
                        &theme,
                    )
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
            } else if indexed.cell.c != ' '
                && !indexed.cell.c.is_control()
                && !flags.contains(Flags::HIDDEN)
                && let Some(glyph) = self.cached_terminal_glyph(cx, indexed.cell.c)
            {
                let baseline_y = y
                    + self.cell_offset_y
                    + self.text_y_offset
                    + glyph.baseline_offset_in_lpxs as f64;
                self.draw_text.draw_rasterized_glyph_abs(
                    cx,
                    TextPoint::new(
                        (x + glyph.x_offset_in_lpxs as f64) as f32,
                        baseline_y as f32,
                    ),
                    glyph.font_size_in_lpxs,
                    glyph.rasterized,
                    fg,
                );
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

        self.draw_cursor_at(cx, cursor.shape, cursor.point, display_offset, focused);
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
        let (cell_width, cell_height) = self.cell_metrics();
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
        let (cell_width, cell_height) = self.cell_metrics();
        let origin = self.origin();
        let x = origin.x + viewport.column.0 as f64 * cell_width;
        let y = origin.y + viewport.line as f64 * cell_height + self.cursor_y_offset;

        let (pos, size) = match shape {
            CursorShape::Beam => (dvec2(x, y), dvec2(BEAM_WIDTH, cell_height)),
            CursorShape::Underline => (
                dvec2(x, y + cell_height - RULE_HEIGHT * 2.0),
                dvec2(cell_width, RULE_HEIGHT * 2.0),
            ),
            _ => (dvec2(x, y), dvec2(cell_width, cell_height)),
        };

        // The IME panel opens under the cursor, wherever it is.
        self.ime_pos = Some(dvec2(
            x - self.unscrolled_rect.pos.x,
            y - self.unscrolled_rect.pos.y + cell_height,
        ));
        self.draw_cursor.focus = if focused && shape != CursorShape::HollowBlock {
            1.0
        } else {
            0.0
        };
        self.draw_cursor.draw_abs(cx, Rect { pos, size });
    }

    /// The cell under the pointer, and which half of it — the side decides
    /// whether a selection takes the character or stops before it.
    fn point_at(
        &self,
        abs: Vec2d,
        display_offset: usize,
        columns: usize,
        screen_lines: usize,
    ) -> (Point, Side) {
        let (cell_width, cell_height) = self.cell_metrics();
        let origin = self.origin();
        let x = (abs.x - origin.x).max(0.0);
        let y = (abs.y - origin.y).max(0.0);
        let column = ((x / cell_width).floor() as usize).min(columns.saturating_sub(1));
        let line = ((y / cell_height).floor() as usize).min(screen_lines.saturating_sub(1));
        let side = if x - (column as f64 * cell_width) > cell_width / 2.0 {
            Side::Right
        } else {
            Side::Left
        };
        (
            viewport_to_point(display_offset, Point::new(line, Column(column))),
            side,
        )
    }

    fn emit_input_bytes(&self, cx: &mut Cx, session: Session, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        cx.widget_action(
            self.widget_uid(),
            TerminalViewAction::Input { session, data },
        );
    }

    fn emit_paste_text(&mut self, cx: &mut Cx, session: Session, text: &str, bracketed: bool) {
        if text.is_empty() {
            return;
        }
        self.emit_input_bytes(cx, session, paste_bytes(text, bracketed));
    }

    fn shell_quote_path(path: &str) -> String {
        let mut out = String::with_capacity(path.len() + 2);
        out.push('\'');
        for ch in path.chars() {
            if ch == '\'' {
                out.push_str("'\\''");
            } else {
                out.push(ch);
            }
        }
        out.push('\'');
        out
    }

    fn dropped_text_payload(items: &[DragItem]) -> Option<String> {
        if items.is_empty() {
            return None;
        }
        let mut payload_parts = Vec::new();
        let mut only_paths = true;
        for item in items {
            match item {
                DragItem::String { value, .. } => {
                    only_paths = false;
                    payload_parts.push(value.clone());
                }
                DragItem::FilePath { path, .. } => {
                    payload_parts.push(Self::shell_quote_path(path));
                }
            }
        }
        if payload_parts.is_empty() {
            None
        } else if only_paths {
            Some(format!("{} ", payload_parts.join(" ")))
        } else if payload_parts.len() == 1 {
            payload_parts.into_iter().next()
        } else {
            Some(payload_parts.join("\n"))
        }
    }

    fn handle_drop(
        &mut self,
        cx: &mut Cx,
        session: Session,
        event: &Event,
        mode: TermMode,
    ) -> bool {
        match event.drag_hits(cx, self.scroll_bars.area()) {
            DragHit::Drag(drag) => {
                if Self::dropped_text_payload(drag.items.as_ref()).is_none() {
                    return false;
                }
                *drag.response.lock().unwrap() = DragResponse::Copy;
                true
            }
            DragHit::Drop(drop) => {
                let Some(payload) = Self::dropped_text_payload(drop.items.as_ref()) else {
                    return false;
                };
                let bracketed = mode.contains(TermMode::BRACKETED_PASTE);
                self.emit_paste_text(cx, session, &payload, bracketed);
                self.draw_bg.redraw(cx);
                true
            }
            _ => false,
        }
    }

    /// Typing pulls the view back to the prompt, the way every terminal does.
    fn scroll_to_bottom(&self, session: Session) {
        if let Some(shared) = crate::terminal::term(session) {
            shared.lock().scroll_display(Scroll::Bottom);
        }
    }
}

impl Widget for TerminalView {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.scroll_bars.begin(cx, walk, self.layout);
        self.viewport_rect = cx.turtle().rect();
        self.unscrolled_rect = cx.turtle().rect_unscrolled();
        self.refresh_cell_metrics(cx);
        self.ime_pos = Some(dvec2(self.pad_x, self.pad_y + self.cell_height));

        let window = crate::frame_state(scope)
            .map(|state| state.id)
            .unwrap_or_default();
        let session = Self::session_for_widget(cx, self.widget_uid(), window);

        self.draw_bg.draw_abs(cx, self.unscrolled_rect);

        let (mut total_lines, mut display_offset) = (0, 0);
        if let Some(session) = session {
            crate::terminal::resize(session, self.size());
            if let Some(shared) = crate::terminal::term(session) {
                let term = shared.lock();
                total_lines = term.grid().total_lines();
                display_offset = term.grid().display_offset();
                let focused = cx.has_key_focus(self.scroll_bars.area());
                self.draw_grid(cx, &term, focused);
            }
        }

        // The bar follows the display: content is every line the grid holds,
        // and the offset counts up from the bottom.
        let (_, cell_height) = self.cell_metrics();
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
        if session.is_some()
            && cx.has_key_focus(self.scroll_bars.area())
            && let Some(ime_pos) = self.ime_pos
        {
            cx.show_text_ime(self.scroll_bars.area(), ime_pos);
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let window = crate::frame_state(scope)
            .map(|state| state.id)
            .unwrap_or_default();
        let session = Self::session_for_widget(cx, self.widget_uid(), window);
        let Some((session, shared)) =
            session.and_then(|s| crate::terminal::term(s).map(|term| (s, term)))
        else {
            self.scroll_bars.handle_event(cx, event, scope);
            return;
        };

        // One read of everything the handlers need, so no lock is held while
        // an action is emitted.
        let (mode, display_offset, columns, screen_lines) = {
            let term = shared.lock();
            (
                *term.mode(),
                term.grid().display_offset(),
                term.columns(),
                term.screen_lines(),
            )
        };

        if self.handle_drop(cx, session, event, mode) {
            return;
        }

        // The wheel belongs to the application while it is tracking the
        // pointer, or on the alt screen; otherwise it scrolls our scrollback.
        if let Event::Scroll(e) = event
            && self.scroll_bars.area().clipped_rect(cx).contains(e.abs)
        {
            let (_, cell_height) = self.cell_metrics();
            self.scroll_accum += e.scroll.y;
            let lines = (self.scroll_accum / cell_height).abs() as usize;
            if lines > 0 {
                let up = self.scroll_accum < 0.0;
                let (point, _) = self.point_at(e.abs, display_offset, columns, screen_lines);
                match mouse::wheel(up, lines, &e.modifiers, point, mode) {
                    Some(bytes) => {
                        self.scroll_accum -=
                            self.scroll_accum.signum() * lines as f64 * cell_height;
                        self.emit_input_bytes(cx, session, bytes);
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

        let scroll_actions = self.scroll_bars.handle_event(cx, event, scope);
        if !scroll_actions.is_empty() {
            // A drag or a wheel the bar took: turn the new position back into
            // a display offset, which is where the scrollback really lives.
            let (_, cell_height) = self.cell_metrics();
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

        match event.hits(cx, self.scroll_bars.area()) {
            Hit::FingerDown(e) => {
                cx.set_key_focus(self.scroll_bars.area());
                self.held = e.device.mouse_button();
                let (point, side) = self.point_at(e.abs, display_offset, columns, screen_lines);

                // ⌘-click follows a link the program marked with OSC 8, the
                // way every terminal that understands them does.
                if e.modifiers.logo {
                    let uri = shared.lock().grid()[point]
                        .hyperlink()
                        .map(|link| link.uri().to_string());
                    if let Some(uri) = uri {
                        cx.open_url(&uri, OpenUrlInPlace::No);
                        return;
                    }
                }

                if let Some(button) = self
                    .held
                    .filter(|_| mouse::wants_pointer(&e.modifiers, mode))
                {
                    if let Some(bytes) = mouse::report(button, true, &e.modifiers, point, mode) {
                        self.emit_input_bytes(cx, session, bytes);
                    }
                } else {
                    let ty = match e.tap_count {
                        1 => SelectionType::Simple,
                        2 => SelectionType::Semantic,
                        _ => SelectionType::Lines,
                    };
                    let ty = if e.modifiers.control && e.modifiers.alt {
                        SelectionType::Block
                    } else {
                        ty
                    };
                    self.selecting = true;
                    shared.lock().selection = Some(Selection::new(ty, point, side));
                }
                self.draw_bg.redraw(cx);
            }
            Hit::FingerMove(e) => {
                cx.set_cursor(MouseCursor::Text);
                let (point, side) = self.point_at(e.abs, display_offset, columns, screen_lines);
                if self.selecting {
                    if let Some(selection) = shared.lock().selection.as_mut() {
                        selection.update(point, side);
                    }
                    self.draw_bg.redraw(cx);
                } else if mouse::wants_pointer(&e.modifiers, mode)
                    && mouse::wants_motion(self.held.is_some(), mode)
                    && let Some(bytes) = mouse::motion(self.held, &e.modifiers, point, mode)
                {
                    self.emit_input_bytes(cx, session, bytes);
                }
            }
            Hit::FingerUp(e) => {
                let (point, _) = self.point_at(e.abs, display_offset, columns, screen_lines);
                if let Some(button) = self
                    .held
                    .filter(|_| mouse::wants_pointer(&e.modifiers, mode))
                    && let Some(bytes) = mouse::report(button, false, &e.modifiers, point, mode)
                {
                    self.emit_input_bytes(cx, session, bytes);
                }
                self.selecting = false;
                self.held = None;
            }
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                // The hand is the only hint that a link is there to be taken.
                let (point, _) = self.point_at(e.abs, display_offset, columns, screen_lines);
                let over_link =
                    e.modifiers.logo && shared.lock().grid()[point].hyperlink().is_some();
                cx.set_cursor(if over_link {
                    MouseCursor::Hand
                } else {
                    MouseCursor::Text
                });
            }
            Hit::KeyFocus(_) => {
                self.draw_bg.redraw(cx);
            }
            Hit::KeyFocusLost(_) => {
                cx.hide_text_ime();
                self.draw_bg.redraw(cx);
            }
            Hit::KeyDown(e) => {
                // NOTE: makepad emits a paste for both shortcuts on macOS.
                if matches!(e.key_code, KeyCode::KeyV)
                    && (e.modifiers.logo || (cfg!(target_os = "macos") && e.modifiers.control))
                {
                    return;
                }
                if let Some(bytes) = keys::encode(&e, true, mode) {
                    self.emit_input_bytes(cx, session, bytes);
                    self.scroll_to_bottom(session);
                    self.draw_bg.redraw(cx);
                }
            }
            Hit::KeyUp(e) => {
                if let Some(bytes) = keys::encode(&e, false, mode) {
                    self.emit_input_bytes(cx, session, bytes);
                }
            }
            Hit::TextInput(e) => {
                if e.replace_last {
                    return;
                }
                if e.was_paste {
                    let bracketed = mode.contains(TermMode::BRACKETED_PASTE);
                    self.emit_paste_text(cx, session, &e.input, bracketed);
                } else if let Some(bytes) =
                    keys::encode_text(&e.input, &KeyModifiers::default(), mode)
                {
                    self.emit_input_bytes(cx, session, bytes);
                }
                self.scroll_to_bottom(session);
                self.draw_bg.redraw(cx);
            }
            Hit::TextCopy(e) => {
                *e.response.borrow_mut() = shared.lock().selection_to_string();
            }
            _ => {}
        }
    }
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
    use super::*;

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
