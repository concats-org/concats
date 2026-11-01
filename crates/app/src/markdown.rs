//! Markdown prose in the review list, parsed once and drawn many times.
//!
//! A row's text is parsed into owned `pulldown_cmark` events the first time
//! it draws; the list keeps them, since its row widgets go when they scroll
//! out, and every draw replays them into a `TextFlow`. makepad's own
//! `Markdown` widget parses its body again on every draw, which is why this
//! text used to be shown raw.

use std::sync::Arc;

use concats_diff::{Row, Table};
use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::makepad_widgets::{
    Area, Cx2d, Flow, FlowBlockType, Inset, Layout, LiveId, Rect, Scope, Size, TextFlow, Vec4f,
    Walk, WidgetRef, dvec2, live_id,
};

/// One text's Markdown, parsed.
pub(crate) type Parsed = Arc<[Event<'static>]>;

const OPTIONS: Options = Options::ENABLE_STRIKETHROUGH.union(Options::ENABLE_TABLES);

pub(crate) fn parse(md: &str) -> Parsed {
    Parser::new_ext(md, OPTIONS)
        .map(Event::into_static)
        .collect()
}

/// What the table row `md` parses as: itself if it is the head, otherwise
/// beneath the head, as GFM reads a row only there.
pub(crate) fn row_text(md: &str, table: &Table) -> String {
    if md == table.head {
        md.into()
    } else {
        format!("{}\n{md}", table.head)
    }
}

/// A table row's [`row_text`], parsed. A body row's head draws in a row of
/// its own, so the parse leaves it out.
pub(crate) fn parse_row(text: &str) -> Parsed {
    if !Parser::new_ext(text, OPTIONS).any(|event| event == Event::Start(Tag::TableRow)) {
        return parse(text);
    }
    let mut in_head = false;
    Parser::new_ext(text, OPTIONS)
        .filter(|event| {
            let skip = in_head || matches!(event, Event::Start(Tag::TableHead));
            in_head = skip && !matches!(event, Event::End(TagEnd::TableHead));
            !skip
        })
        .map(Event::into_static)
        .collect()
}

/// The rows `md` draws as: its top-level blocks, a bulleted list cut into
/// its items and a table into its rows. The list draws only the rows on
/// screen, so a long section costs what shows of it. An ordered list stays
/// whole, since an item alone would renumber from its own marker, and so
/// does text defining a link reference, which a cut would strand. HTML draws
/// nothing, so it makes no row.
pub(crate) fn rows(md: &str) -> Vec<Row> {
    let mut parser = Parser::new_ext(md, OPTIONS).into_offset_iter();
    let mut rows = Vec::new();
    let mut depth = 0;
    let mut in_bullets = false;
    for (event, range) in parser.by_ref() {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    in_bullets = tag == Tag::List(None);
                }
                match tag {
                    Tag::Table(_) if depth == 0 => rows.extend(table_rows(&md[range])),
                    Tag::HtmlBlock => {}
                    Tag::Item if depth == 1 && in_bullets => rows.push(prose(&md[range])),
                    _ if depth == 0 && !in_bullets => rows.push(prose(&md[range])),
                    _ => {}
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            Event::Rule if depth == 0 => rows.push(prose(&md[range])),
            _ => {}
        }
    }
    if parser.reference_definitions().iter().next().is_some() {
        return vec![prose(md)];
    }
    rows
}

fn prose(md: &str) -> Row {
    Row::Prose {
        md: md.trim_end().into(),
    }
}

/// The table `md`, a row each, its head first: they share its widths.
fn table_rows(md: &str) -> Vec<Row> {
    let events: Vec<_> = Parser::new_ext(md, OPTIONS).into_offset_iter().collect();
    let body: Vec<_> = events
        .iter()
        .filter(|(event, _)| matches!(event, Event::Start(Tag::TableRow)))
        .map(|(_, range)| range.clone())
        .collect();
    let head = body.first().map_or(md, |row| &md[..row.start]).trim_end();
    let table = Arc::new(Table {
        head: head.into(),
        widths: widths(&longest_cells(events.iter().map(|(event, _)| event))),
    });
    std::iter::once(head)
        .chain(body.into_iter().map(|row| md[row].trim_end()))
        .map(|md| Row::TableRow {
            md: md.into(),
            table: table.clone(),
        })
        .collect()
}

/// The longest cell of each column of the table `events` open, in chars.
fn longest_cells<'a, 'md: 'a>(events: impl IntoIterator<Item = &'a Event<'md>>) -> Vec<usize> {
    let mut longest = Vec::new();
    let (mut column, mut cell) = (0, 0);
    for event in events {
        match event {
            Event::Text(text) | Event::Code(text) => cell += text.chars().count(),
            Event::End(TagEnd::TableCell) => {
                match longest.get_mut(column) {
                    Some(widest) => *widest = cell.max(*widest),
                    None => longest.push(cell),
                }
                (column, cell) = (column + 1, 0);
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => column = 0,
            Event::End(TagEnd::Table) => break,
            _ => {}
        }
    }
    longest
}

/// Each column's share of a table's width: what its longest cell asks for,
/// up to a cap, so a column of numbers stays narrow and the columns of prose
/// split the rest.
fn widths(longest: &[usize]) -> Vec<f32> {
    let asks: Vec<f32> = longest
        .iter()
        .map(|&chars| f32::from(u8::try_from(chars.clamp(3, 40)).unwrap_or(40)) + 2.0)
        .collect();
    let total: f32 = asks.iter().sum();
    asks.iter().map(|ask| ask / total).collect()
}

/// Inside a table cell: room around its text.
const CELL_PADDING: Inset = Inset {
    left: 6.0,
    top: 4.0,
    right: 6.0,
    bottom: 4.0,
};

/// The table being drawn: its columns, and the row in progress.
#[derive(Default)]
struct Grid {
    widths: Vec<f32>,
    aligns: Vec<Alignment>,
    head: bool,
    /// The head's fill, begun before its text so that it draws beneath it.
    fill: Option<Area>,
    /// The cells of the row in progress, drawn.
    cells: Vec<Rect>,
}

impl Grid {
    /// Draw a table's part: open it, or begin or end a row or a cell. A
    /// table opens with the column `widths` it was cut with, or, whole,
    /// with its own from `events`, which start at it.
    fn draw(
        &mut self,
        tf: &mut TextFlow,
        cx: &mut Cx2d,
        events: &[Event<'static>],
        widths: Option<&[f32]>,
    ) {
        match &events[0] {
            Event::Start(Tag::Table(aligns)) => {
                *self = Grid {
                    widths: widths
                        .map_or_else(|| self::widths(&longest_cells(events)), <[f32]>::to_vec),
                    aligns: aligns.clone(),
                    ..Grid::default()
                };
                cx.begin_turtle(Walk::fill_fit(), Layout::flow_down());
            }
            Event::End(TagEnd::Table) => {
                cx.end_turtle();
            }
            Event::Start(Tag::TableHead) => self.begin_row(tf, cx, true),
            Event::Start(Tag::TableRow) => self.begin_row(tf, cx, false),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => self.end_row(tf, cx),
            Event::Start(Tag::TableCell) => self.begin_cell(tf, cx),
            Event::End(TagEnd::TableCell) => self.end_cell(tf, cx),
            _ => {}
        }
    }

    fn begin_row(&mut self, tf: &mut TextFlow, cx: &mut Cx2d, head: bool) {
        self.head = head;
        self.cells.clear();
        let (walk, layout) = (Walk::fill_fit(), Layout::flow_right());
        if head {
            tf.draw_block.block_type = FlowBlockType::TableCell;
            tf.draw_block.begin(cx, walk, layout);
            self.fill = Some(tf.draw_block.draw_vars.area);
        } else {
            cx.begin_turtle(walk, layout);
        }
    }

    fn begin_cell(&self, tf: &mut TextFlow, cx: &mut Cx2d) {
        let column = self.cells.len();
        let share = self.widths.get(column).copied().unwrap_or_default();
        let walk = Walk {
            width: Size::Fixed(cx.turtle().inner_width() * f64::from(share)),
            ..Walk::fit()
        };
        let layout = Layout {
            flow: Flow::right_wrap(),
            padding: CELL_PADDING,
            ..Layout::default()
        };
        cx.begin_turtle(walk, layout);
        tf.first_thing_on_a_line = true;
        tf.cell_text_align_x = match self.aligns.get(column) {
            Some(Alignment::Center) => 0.5,
            Some(Alignment::Right) => 1.0,
            _ => 0.0,
        };
        if self.head {
            tf.bold.push();
        }
    }

    fn end_cell(&mut self, tf: &mut TextFlow, cx: &mut Cx2d) {
        if self.head {
            tf.bold.pop();
        }
        tf.cell_text_align_x = 0.0;
        self.cells.push(cx.end_turtle());
    }

    /// Close the row and rule it: each cell draws its right and bottom edge,
    /// the first cell the left one, and the head the top.
    fn end_row(&mut self, tf: &mut TextFlow, cx: &mut Cx2d) {
        let block = &mut tf.draw_block;
        let row = match self.fill.take() {
            Some(fill) => {
                block.draw_vars.area = fill;
                block.end(cx);
                fill.rect(cx)
            }
            None => cx.end_turtle(),
        };
        let shade = block.table_header_bg_color;
        block.block_type = FlowBlockType::TableCell;
        block.table_header_bg_color = Vec4f::default();
        for cell in &self.cells {
            let size = dvec2(cell.size.x, row.size.y);
            block.draw_abs(
                cx,
                Rect {
                    pos: cell.pos,
                    size,
                },
            );
        }
        if let Some(first) = self.cells.first() {
            let size = dvec2(1.0, row.size.y);
            block.draw_abs(
                cx,
                Rect {
                    pos: first.pos,
                    size,
                },
            );
        }
        if self.head {
            let size = dvec2(row.size.x, 1.0);
            block.draw_abs(cx, Rect { pos: row.pos, size });
        }
        block.table_header_bg_color = shade;
    }
}

/// Air between blocks, and above a code block.
const BLOCK_SPACING: f64 = 6.0;

fn heading_scale(level: HeadingLevel) -> f64 {
    match level {
        HeadingLevel::H1 => 1.45,
        HeadingLevel::H2 => 1.25,
        HeadingLevel::H3 => 1.1,
        _ => 1.0,
    }
}

/// Start a block: air above it unless it is the first.
fn block(tf: &mut TextFlow, cx: &mut Cx2d, first: &mut bool) {
    if !*first {
        tf.new_line_collapsed_with_spacing(cx, BLOCK_SPACING);
    }
    *first = false;
}

/// Draw `events` into `tf`, which has begun its flow. A table takes the
/// column `widths` of the one it was cut from, or works out its own. A link
/// draws as the flow's `link` item with the text it wraps; `on_link` gets
/// each one and its destination before it draws, to say what a click on it
/// does.
#[expect(
    clippy::too_many_lines,
    reason = "One arm per Markdown element, over the list, link and table state they share."
)]
pub(crate) fn replay(
    tf: &mut TextFlow,
    cx: &mut Cx2d,
    events: &[Event<'static>],
    widths: Option<&[f32]>,
    mut on_link: impl FnMut(&WidgetRef, &str),
) {
    // The next number of each open list; `None` for a bulleted one.
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut first = true;
    // The open link's destination and the text it wraps so far.
    let mut link: Option<(String, String)> = None;
    let mut links = 0;
    let mut grid = Grid::default();
    for (at, event) in events.iter().enumerate() {
        match event {
            Event::Start(Tag::Table(_)) => {
                block(tf, cx, &mut first);
                grid.draw(tf, cx, &events[at..], widths);
            }
            Event::Start(Tag::TableHead | Tag::TableRow | Tag::TableCell)
            | Event::End(
                TagEnd::Table | TagEnd::TableHead | TagEnd::TableRow | TagEnd::TableCell,
            ) => {
                grid.draw(tf, cx, &events[at..], widths);
            }
            Event::Start(Tag::Heading { level, .. }) => {
                block(tf, cx, &mut first);
                tf.push_size_rel_scale(heading_scale(*level));
                tf.bold.push();
            }
            Event::End(TagEnd::Heading(_)) => {
                tf.bold.pop();
                tf.font_sizes.pop();
                tf.new_line_collapsed(cx);
            }
            Event::Start(Tag::Paragraph) => block(tf, cx, &mut first),
            Event::Start(Tag::BlockQuote(_)) => {
                block(tf, cx, &mut first);
                tf.begin_quote(cx);
            }
            Event::End(TagEnd::BlockQuote(_)) => tf.end_quote(cx),
            Event::Start(Tag::List(start)) => lists.push(*start),
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                if !first {
                    tf.new_line_collapsed(cx);
                }
                first = false;
                let marker = match lists.last_mut() {
                    Some(Some(next)) => {
                        *next += 1;
                        format!("{}.", *next - 1)
                    }
                    _ => "•".to_string(),
                };
                tf.begin_list_item(cx, &marker, 2.5);
            }
            Event::End(TagEnd::Item) => tf.end_list_item(cx),
            Event::Start(Tag::Emphasis) => tf.italic.push(),
            Event::End(TagEnd::Emphasis) => tf.italic.pop(),
            Event::Start(Tag::Strong) => tf.bold.push(),
            Event::End(TagEnd::Strong) => tf.bold.pop(),
            Event::Start(Tag::Strikethrough) => tf.strikethrough.push(),
            Event::End(TagEnd::Strikethrough) => tf.strikethrough.pop(),
            Event::Start(Tag::Link { dest_url, .. }) => {
                link = Some((dest_url.to_string(), String::new()));
            }
            Event::End(TagEnd::Link) => {
                // NOTE: an item bypasses the flow's line allowance; past it, a
                // clamped body draws no more.
                if let Some((href, text)) = link.take()
                    && !tf.is_content_truncated()
                {
                    links += 1;
                    let item: WidgetRef = tf.item(cx, LiveId(links), live_id!(link));
                    item.set_text(cx, &text);
                    on_link(&item, &href);
                    // The link draws its text into the flow it finds in scope.
                    item.draw_all(cx, &mut Scope::with_data(tf));
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                block(tf, cx, &mut first);
                tf.combine_spaces.push(false);
                tf.fixed.push();
                tf.begin_code(cx);
            }
            Event::End(TagEnd::CodeBlock) => {
                tf.fixed.pop();
                tf.combine_spaces.pop();
                tf.end_code(cx);
            }
            Event::Code(code) => {
                if let Some((_, text)) = &mut link {
                    text.push_str(code);
                } else {
                    tf.fixed.push();
                    tf.inline_code.push();
                    tf.draw_text(cx, code);
                    tf.inline_code.pop();
                    tf.fixed.pop();
                }
            }
            Event::Text(text) => match &mut link {
                Some((_, linked)) => linked.push_str(text),
                None => tf.draw_text(cx, text.trim_end_matches('\n')),
            },
            Event::SoftBreak => match &mut link {
                Some((_, linked)) => linked.push(' '),
                None => tf.draw_text(cx, " "),
            },
            Event::HardBreak => tf.new_line_collapsed(cx),
            Event::Rule => {
                block(tf, cx, &mut first);
                tf.sep(cx);
                tf.new_line_collapsed_with_spacing(cx, BLOCK_SPACING);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parse_owns_its_events() {
        let parsed = parse("# Tour\n\nThe **greeting** changes in `editor.rs`.\n");
        assert!(matches!(
            parsed[0],
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            })
        ));
        assert!(
            parsed
                .iter()
                .any(|e| matches!(e, Event::Code(code) if &**code == "editor.rs"))
        );
        assert!(
            parsed
                .iter()
                .any(|e| matches!(e, Event::Start(Tag::Strong)))
        );
    }

    /// Each row's kind and text.
    fn cut(md: &str) -> Vec<(&'static str, String)> {
        rows(md)
            .into_iter()
            .map(|row| match row {
                Row::Prose { md } => ("prose", md),
                Row::TableRow { md, .. } => ("table", md),
                _ => ("other", String::new()),
            })
            .collect()
    }

    #[test]
    fn a_section_cuts_into_blocks_and_bullets_into_items() {
        let md = "## Plan\n\nOne *paragraph*\nover two lines.\n\n\
                  - first\n- second\n  - nested\n\n\
                  1. one\n1. two\n\n\
                  ---\n\n<!-- a note -->\n\n```rust\nfn main() {}\n```\n";
        let prose = |md: &str| ("prose", md.to_string());
        assert_eq!(
            cut(md),
            [
                prose("## Plan"),
                prose("One *paragraph*\nover two lines."),
                prose("- first"),
                prose("- second\n  - nested"),
                prose("1. one\n1. two"),
                prose("---"),
                prose("```rust\nfn main() {}\n```"),
            ]
        );
    }

    #[test]
    fn a_table_cuts_into_its_rows_beneath_one_head() {
        let md = "Before.\n\n| # | Stop | Files |\n|--:|---|---|\n\
                  | 1 | Who reads a `program` | `auth.ts`, `rbac.ts` |\n\
                  | 2 | Roles | [guards](guards.ts) |\n\nAfter.\n";
        let rows = rows(md);
        let Row::TableRow { table, .. } = &rows[1] else {
            panic!("a table row");
        };
        assert_eq!(table.head, "| # | Stop | Files |\n|--:|---|---|");
        assert_eq!(
            cut(md),
            [
                ("prose", "Before.".into()),
                ("table", table.head.clone()),
                (
                    "table",
                    "| 1 | Who reads a `program` | `auth.ts`, `rbac.ts` |".into()
                ),
                ("table", "| 2 | Roles | [guards](guards.ts) |".into()),
                ("prose", "After.".into()),
            ]
        );
        // The number column asks for little; the two of prose split the rest.
        assert!(table.widths[0] < table.widths[1] && table.widths[0] < table.widths[2]);
        assert!((table.widths.iter().sum::<f32>() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_body_row_parses_beneath_its_head_without_drawing_it() {
        let rows = rows("| a | b |\n|---|:-:|\n| one | two |\n");
        let (Row::TableRow { md: head, table }, Row::TableRow { md: body, .. }) =
            (&rows[0], &rows[1])
        else {
            panic!("two table rows");
        };
        let head = parse_row(&row_text(head, table));
        assert!(
            head.iter()
                .any(|e| matches!(e, Event::Start(Tag::TableHead)))
        );
        let body = parse_row(&row_text(body, table));
        assert!(
            !body
                .iter()
                .any(|e| matches!(e, Event::Start(Tag::TableHead)))
        );
        assert!(
            matches!(&body[0], Event::Start(Tag::Table(aligns)) if aligns[1] == Alignment::Center)
        );
        assert!(
            body.iter()
                .any(|e| matches!(e, Event::Text(t) if &**t == "two"))
        );
    }

    #[test]
    fn text_defining_a_link_reference_stays_whole() {
        let md = "See [the plan][plan].\n\n[plan]: docs/plan.md\n";
        assert_eq!(cut(md), [("prose", md.trim_end().to_string())]);
    }
}
