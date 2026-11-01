//! How a transcript reads when it is drawn line by line. Prompts and
//! responses are Markdown, and their rows are addressed byte by byte —
//! comments, quote underlines, find hits, the caret — so nothing can be
//! rendered away. Each line's bytes are styled where they stand instead:
//! emphasis italic, strong text and headings bold, and the markup dimmed.
//! A link stays its bytes too, and ⌘-click follows it.

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};

const OPTIONS: Options = Options::ENABLE_STRIKETHROUGH;

/// Where the link at byte `at` of transcript `text` goes, written as
/// `[text](destination)` or `<destination>`.
pub(crate) fn link_at(text: &str, at: usize) -> Option<String> {
    Parser::new_ext(text, OPTIONS)
        .into_offset_iter()
        .find_map(|(event, range)| match event {
            Event::Start(Tag::Link { dest_url, .. }) if range.contains(&at) => {
                Some(dest_url.into_string())
            }
            _ => None,
        })
}

/// How a run of transcript is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Style {
    pub bold: bool,
    pub italic: bool,
    /// Markup rather than content: `**`, backticks, a heading's `#`s.
    pub dim: bool,
}

/// One line's styled runs, `(start, end, style)` in bytes of that line, in
/// order; bytes in none of them are plain.
pub(crate) type Runs = Vec<(usize, usize, Style)>;

/// Every line's runs, for `text` whose lines start at `line_starts` (one
/// entry past the last line, as `Blob` keeps them). Parsed as a whole: a
/// code fence or a list crosses lines.
pub(crate) fn line_styles(text: &str, line_starts: &[u32]) -> Vec<Runs> {
    let mut bytes = vec![Style::default(); text.len()];
    for (event, range) in Parser::new_ext(text, OPTIONS).into_offset_iter() {
        match event {
            Event::Start(Tag::Strong) => {
                mark(&mut bytes, range.clone(), |s| s.bold = true);
                dim_ends(&mut bytes, text, range, b"*_", 2);
            }
            Event::Start(Tag::Emphasis) => {
                mark(&mut bytes, range.clone(), |s| s.italic = true);
                dim_ends(&mut bytes, text, range, b"*_", 1);
            }
            Event::Start(Tag::Heading { .. }) => {
                mark(&mut bytes, range.clone(), |s| s.bold = true);
                let hashes = leading(text, range.start, b"#", usize::MAX);
                mark(&mut bytes, range.start..range.start + hashes, |s| {
                    s.dim = true;
                });
            }
            Event::Code(_) => dim_ends(&mut bytes, text, range, b"`", usize::MAX),
            // `[text](url)`: the brackets and the destination are markup.
            Event::Start(Tag::Link { .. }) if text[range.clone()].starts_with('[') => {
                mark(&mut bytes, range.start..range.start + 1, |s| s.dim = true);
                if let Some(close) = text[range.clone()].find("](") {
                    mark(&mut bytes, range.start + close..range.end, |s| s.dim = true);
                }
            }
            _ => {}
        }
    }
    line_starts
        .windows(2)
        .map(|line| {
            let (start, end) = (line[0] as usize, (line[1] as usize).min(text.len()));
            runs(&bytes[start..end])
        })
        .collect()
}

fn mark(bytes: &mut [Style], range: Range<usize>, apply: impl Fn(&mut Style)) {
    bytes[range].iter_mut().for_each(apply);
}

/// How many of the bytes from `at` are among `delims`, at most `at_most`.
fn leading(text: &str, at: usize, delims: &[u8], at_most: usize) -> usize {
    text.as_bytes()[at..]
        .iter()
        .take(at_most)
        .take_while(|b| delims.contains(b))
        .count()
}

/// Dim the delimiter runs `range` opens and closes with.
fn dim_ends(bytes: &mut [Style], text: &str, range: Range<usize>, delims: &[u8], at_most: usize) {
    let open = leading(text, range.start, delims, at_most);
    let close = text.as_bytes()[range.clone()]
        .iter()
        .rev()
        .take(at_most)
        .take_while(|b| delims.contains(b))
        .count();
    mark(bytes, range.start..range.start + open, |s| s.dim = true);
    mark(bytes, range.end - close..range.end, |s| s.dim = true);
}

/// The runs of `line` that are not plain, each one style.
fn runs(line: &[Style]) -> Runs {
    let mut out = Runs::new();
    let mut start = 0;
    for at in 1..=line.len() {
        if at == line.len() || line[at] != line[start] {
            if line[start] != Style::default() {
                out.push((start, at, line[start]));
            }
            start = at;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn starts(text: &str) -> Vec<u32> {
        std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .chain((!text.ends_with('\n')).then_some(text.len()))
            .map(|at| u32::try_from(at).expect("a short fixture"))
            .collect()
    }

    const BOLD: Style = Style {
        bold: true,
        italic: false,
        dim: false,
    };
    const DIM_BOLD: Style = Style {
        bold: true,
        italic: false,
        dim: true,
    };
    const DIM: Style = Style {
        bold: false,
        italic: false,
        dim: true,
    };

    #[test]
    fn markup_is_dimmed_where_it_stands_and_content_styled() {
        let text = "Make it **strict** and `fast`.\n";
        let lines = line_styles(text, &starts(text));
        assert_eq!(
            lines[0],
            [
                (8, 10, DIM_BOLD),
                (10, 16, BOLD),
                (16, 18, DIM_BOLD),
                (23, 24, DIM),
                (28, 29, DIM),
            ]
        );
    }

    #[test]
    fn a_heading_is_bold_its_hashes_dim_and_lines_keep_their_own_offsets() {
        let text = "# Plan\nplain *it*\n";
        let lines = line_styles(text, &starts(text));
        assert_eq!(lines[0][0], (0, 1, DIM_BOLD));
        assert_eq!(lines[0][1].2, BOLD);
        let italic = Style {
            italic: true,
            ..Style::default()
        };
        assert_eq!(
            lines[1],
            [
                (
                    6,
                    7,
                    Style {
                        dim: true,
                        ..italic
                    }
                ),
                (7, 9, italic),
                (
                    9,
                    10,
                    Style {
                        dim: true,
                        ..italic
                    }
                ),
            ]
        );
    }

    #[test]
    fn a_link_is_found_by_any_byte_of_it() {
        let text = "Read [the guide](/tmp/guide.md), or <https://x.dev>.\n";
        let at = |needle: &str| link_at(text, text.find(needle).unwrap());
        assert_eq!(at("the guide").as_deref(), Some("/tmp/guide.md"));
        assert_eq!(at("guide.md").as_deref(), Some("/tmp/guide.md"));
        assert_eq!(at("x.dev").as_deref(), Some("https://x.dev"));
        assert_eq!(at("Read"), None);
        assert_eq!(at(", or"), None);
    }

    #[test]
    fn a_links_brackets_and_destination_are_markup() {
        let text = "See [docs](https://x.dev) now\n";
        let runs = &line_styles(text, &starts(text))[0];
        assert_eq!(runs, &[(4, 5, DIM), (9, 25, DIM)]);
    }
}
