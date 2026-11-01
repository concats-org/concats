//! Markdown prose in the review list, parsed once and drawn many times.
//!
//! A row's text is parsed into owned `pulldown_cmark` events the first time
//! it draws; the list keeps them, since its row widgets go when they scroll
//! out, and every draw replays them into a `TextFlow`. makepad's own
//! `Markdown` widget parses its body again on every draw, which is why this
//! text used to be shown raw.

use std::sync::Arc;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::makepad_widgets::{Cx2d, LiveId, Scope, TextFlow, WidgetRef, live_id};

/// One text's Markdown, parsed.
pub(crate) type Parsed = Arc<[Event<'static>]>;

const OPTIONS: Options = Options::ENABLE_STRIKETHROUGH;

pub(crate) fn parse(md: &str) -> Parsed {
    Parser::new_ext(md, OPTIONS)
        .map(Event::into_static)
        .collect()
}

/// `md` cut into its top-level blocks, a bulleted list into its items. The
/// list draws only the rows on screen, so a long section costs what shows of
/// it. An ordered list stays whole, since an item alone would renumber from
/// its own marker, and so does text defining a link reference, which a cut
/// would strand. HTML draws nothing, so it makes no block.
pub(crate) fn blocks(md: &str) -> Vec<&str> {
    let mut parser = Parser::new_ext(md, OPTIONS).into_offset_iter();
    let mut cuts = Vec::new();
    let mut depth = 0;
    let mut in_bullets = false;
    for (event, range) in parser.by_ref() {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    in_bullets = tag == Tag::List(None);
                }
                let cut = match depth {
                    0 => !in_bullets && tag != Tag::HtmlBlock,
                    1 => in_bullets && tag == Tag::Item,
                    _ => false,
                };
                if cut {
                    cuts.push(range);
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            Event::Rule if depth == 0 => cuts.push(range),
            _ => {}
        }
    }
    if parser.reference_definitions().iter().next().is_some() {
        return vec![md];
    }
    cuts.into_iter().map(|range| md[range].trim_end()).collect()
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

/// Draw `events` into `tf`, which has begun its flow. A link draws as the
/// flow's `link` item with the text it wraps; `on_link` gets each one and
/// its destination before it draws, to say what a click on it does.
pub(crate) fn replay(
    tf: &mut TextFlow,
    cx: &mut Cx2d,
    events: &[Event<'static>],
    mut on_link: impl FnMut(&WidgetRef, &str),
) {
    // The next number of each open list; `None` for a bulleted one.
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut first = true;
    // The open link's destination and the text it wraps so far.
    let mut link: Option<(String, String)> = None;
    let mut links = 0;
    for event in events {
        match event {
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

    #[test]
    fn a_section_cuts_into_blocks_and_bullets_into_items() {
        let md = "## Plan\n\nOne *paragraph*\nover two lines.\n\n\
                  - first\n- second\n  - nested\n\n\
                  1. one\n1. two\n\n\
                  ---\n\n<!-- a note -->\n\n```rust\nfn main() {}\n```\n";
        assert_eq!(
            blocks(md),
            [
                "## Plan",
                "One *paragraph*\nover two lines.",
                "- first",
                "- second\n  - nested",
                "1. one\n1. two",
                "---",
                "```rust\nfn main() {}\n```",
            ]
        );
    }

    #[test]
    fn text_defining_a_link_reference_stays_whole() {
        let md = "See [the plan][plan].\n\n[plan]: docs/plan.md\n";
        assert_eq!(blocks(md), [md]);
    }
}
