//! Syntax highlighting: a retained parse tree per buffer, queried by viewport.
//!
//! A diff hunk cannot be parsed on its own; tree-sitter needs the whole file to
//! know that a `}` closes an `impl`. So the full text is parsed, the tree is
//! kept, and only the bytes of the lines being drawn are queried. Measured on a
//! 5,865-line file: parsing costs 24.8 ms, querying all of it 11.8 ms, querying
//! a 40-line viewport 47.8 µs. The parse is worth keeping and the query is
//! worth scoping, and together they let a frame be drawn already coloured
//! instead of plain until a worker catches up.
//!
//! The backend is a seam, not a fact: with `treesitter` off the same API
//! answers in unstyled runs, which is what the web target needs (the C grammars
//! do not build for wasm32).

// NOTE: `pedantic` is off here, not because it is wrong but because this code
// arrived from a crate that never ran under it — 300+ findings, almost all
// `must_use_candidate`, numeric casts and missing-`# Errors` docs. Turning it on
// is worth doing; doing it inside a move would hide the move. Everything else
// the workspace enables (`all`, `style`, `complexity`) is enforced.
#![allow(clippy::pedantic, clippy::cognitive_complexity)]

use std::collections::HashMap;

#[cfg(feature = "treesitter")]
use concats_syntax::{Hl, capture_to_hl};
use concats_syntax::{LineSpans, Span};
#[cfg(feature = "treesitter")]
use concats_text::{line_of, line_starts};

/// Per-language cost, so you can compare grammars head to head.
#[derive(Default, Clone)]
pub struct LangStat {
    pub lang: String,
    pub blobs: usize,
    pub lines: usize,
    pub ms: f64,
}

#[derive(Default, Clone)]
pub struct HlStats {
    pub per_lang: HashMap<String, LangStat>,
}

impl HlStats {
    /// Sorted slowest-first — the table the bench prints.
    pub fn ranked(&self) -> Vec<LangStat> {
        let mut v: Vec<_> = self.per_lang.values().cloned().collect();
        v.sort_by(|a, b| b.ms.partial_cmp(&a.ms).unwrap());
        v
    }
}

/// What the highlighter needs to know about a buffer to colour it.
///
/// Borrowed fields rather than a blob: highlighting sits below the diff model,
/// and reaching up for its types is the coupling that would stop this from
/// being its own crate.
#[cfg(feature = "treesitter")]
pub struct Buffer<'a> {
    /// The content this buffer was read as. Stable while it is typed into (the
    /// oid only moves on a save), which is what makes it an identity.
    pub oid: gix::ObjectId,
    /// Bumped by every edit: the cheap signal that the text moved on.
    pub rev: u64,
    /// Whether this text can change at all. Part of the identity because a
    /// read-only blob and an editable buffer can hold the same oid, and one of
    /// them drifts — sharing an entry would have them reparsing each other's
    /// text on every frame.
    pub editable: bool,
    pub ext: &'a str,
    pub text: &'a str,
}

#[cfg(feature = "treesitter")]
impl Buffer<'_> {
    fn key(&self) -> (gix::ObjectId, bool) {
        (self.oid, self.editable)
    }
}

/// A parsed tree, with the text and revision it describes.
#[cfg(feature = "treesitter")]
struct Parsed {
    tree: tree_sitter::Tree,
    /// The text the tree was parsed from, kept so the next revision can be
    /// diffed against it. That diff is where the edits come from.
    source: String,
    rev: u64,
}

#[cfg(feature = "treesitter")]
impl Parsed {
    fn new(src: &str, rev: u64, language: &tree_sitter::Language) -> Option<Self> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(language).ok()?;
        Some(Self {
            tree: parser.parse(src, None)?,
            source: src.to_string(),
            rev,
        })
    }

    /// Bring the tree up to `src` by telling it what changed, then reparsing
    /// against it, so tree-sitter reuses every subtree the edit did not touch.
    ///
    /// The edits are derived by diffing the text this tree was parsed from
    /// against the text now, not reported by whoever made them. Text reaches a
    /// buffer through more paths than typing — undo, redo, an external write
    /// merged in, a cached buffer restored — and a hook on each is a hook you
    /// can forget, which leaves a tree describing content nobody has. Looking
    /// at the result cannot be lied to.
    fn catch_up(&mut self, src: &str, rev: u64, language: &tree_sitter::Language) {
        let starts = line_starts(&self.source);
        let point = |at: usize| {
            let line = line_of(&starts, at);
            tree_sitter::Point {
                row: line,
                column: at - starts[line] as usize,
            }
        };
        // Highest offset first, which is what `hunks` returns and what applying
        // several edits to one tree needs: everything before the edit is still
        // where the tree thinks it is, so each edit's coordinates stay valid as
        // the ones after it are applied.
        for (range, insert) in concats_text::replacements(&self.source, src) {
            let newlines = insert.bytes().filter(|b| *b == b'\n').count();
            let start = point(range.start);
            self.tree.edit(&tree_sitter::InputEdit {
                start_byte: range.start,
                old_end_byte: range.end,
                new_end_byte: range.start + insert.len(),
                start_position: start,
                old_end_position: point(range.end),
                new_end_position: tree_sitter::Point {
                    row: start.row + newlines,
                    column: match insert.rfind('\n') {
                        Some(last) => insert.len() - last - 1,
                        None => start.column + insert.len(),
                    },
                },
            });
        }
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(language).is_err() {
            return;
        }
        // NOTE: a failed reparse leaves the old tree in place rather than
        // dropping the colours; it would describe the previous text, so the
        // revision is left behind too and the next call tries again.
        if let Some(tree) = parser.parse(src, Some(&self.tree)) {
            self.tree = tree;
            self.source = src.to_string();
            self.rev = rev;
        }
    }
}

/// Owns the grammar table. Holds no span cache: the spans live on the blob they
/// describe, filled lazily on first draw, so files you never scroll to are
/// never highlighted and nothing is stored twice.
///
/// It does hold parsed trees, which is a different thing: a tree is what makes
/// asking about a few lines cheap. The numbers are in the module docs — the
/// parse is worth keeping and the query is worth scoping.
pub struct Highlighter {
    #[cfg(feature = "treesitter")]
    langs: HashMap<&'static str, tree_sitter_highlight::HighlightConfiguration>,
    /// One parsed tree per buffer, with the text and revision it was parsed at.
    ///
    /// The revision is in the value, not the key. Keying by it would give each
    /// edit a cache entry of its own and put the previous tree out of reach,
    /// and that is the tree an incremental reparse needs.
    #[cfg(feature = "treesitter")]
    parsed: HashMap<(gix::ObjectId, bool), Parsed>,
    pub stats: HlStats,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "treesitter")]
            langs: concats_languages::registry(),
            #[cfg(feature = "treesitter")]
            parsed: HashMap::new(),
            stats: HlStats::default(),
        }
    }

    /// Number of extensions mapped to a grammar.
    pub fn grammar_count(&self) -> usize {
        #[cfg(feature = "treesitter")]
        {
            self.langs.len()
        }
        #[cfg(not(feature = "treesitter"))]
        {
            0
        }
    }

    /// Book-keeping for the bench's per-language cost table.
    pub fn record(&mut self, ext: &str, lines: usize, dur: std::time::Duration) {
        let lang = concats_languages::lang_for_ext(ext);
        let e = self
            .stats
            .per_lang
            .entry(lang.to_string())
            .or_insert_with(|| LangStat {
                lang: lang.to_string(),
                ..Default::default()
            });
        e.blobs += 1;
        e.lines += lines;
        e.ms += dur.as_secs_f64() * 1000.0;
    }

    /// One line's spans for `buffer`.
    ///
    /// The per-row entry point: a row is drawn knowing its blob and its line,
    /// and this answers in microseconds once the tree is parsed. That is what
    /// lets a frame be painted already coloured instead of plain first.
    #[cfg(feature = "treesitter")]
    pub fn spans_for_line(&mut self, buffer: Buffer<'_>, line: usize) -> Vec<Span> {
        // Bounded rather than clever: a tree is large, and a big review can put
        // hundreds of blobs on screen over a session. Dropping the lot when it
        // grows past this costs one reparse each for whatever is still visible,
        // which is the same price as having never cached them.
        const KEEP_TREES: usize = 64;
        if self.parsed.len() > KEEP_TREES && !self.parsed.contains_key(&buffer.key()) {
            self.parsed.clear();
        }
        let mut spans = self.spans_for_lines(buffer, line..line + 1);
        spans.get_mut(line).map(std::mem::take).unwrap_or_default()
    }

    /// Spans for `lines` only, over a tree parsed once and kept in step.
    ///
    /// This is the path that lets a frame be painted already coloured. The
    /// whole file is parsed, because a hunk cannot be highlighted on its own
    /// (tree-sitter needs the surrounding context to know what a `}` closes),
    /// but the capture query is scoped to the bytes those lines occupy. That
    /// part scales with how much is asked for, not with how big the file is.
    ///
    /// The returned table is the length of the file with only `lines` filled: a
    /// row indexes it by line number, and the rest waits until it is scrolled
    /// to.
    #[cfg(feature = "treesitter")]
    pub fn spans_for_lines(
        &mut self,
        buffer: Buffer<'_>,
        lines: std::ops::Range<usize>,
    ) -> LineSpans {
        use tree_sitter::{QueryCursor, StreamingIterator};

        let src = buffer.text;
        let Some(config) = self.langs.get(buffer.ext) else {
            return plain(src);
        };
        let starts = line_starts(src);
        // Off the line table rather than off `src.lines()`, so this agrees with
        // the table a blob keeps — and so an empty file has no lines to fill
        // instead of one that is not there.
        let count = starts.len() - 1;
        let lines = lines.start.min(count)..lines.end.min(count);
        let mut out: LineSpans = vec![Vec::new(); count];
        if lines.is_empty() {
            return out;
        }

        let tree = match self.parsed.entry(buffer.key()) {
            std::collections::hash_map::Entry::Occupied(held) => {
                let held = held.into_mut();
                if held.rev != buffer.rev {
                    held.catch_up(src, buffer.rev, &config.language);
                }
                &held.tree
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                let Some(fresh) = Parsed::new(src, buffer.rev, &config.language) else {
                    return plain(src);
                };
                &slot.insert(fresh).tree
            }
        };

        let from = starts[lines.start] as usize;
        let to = starts.get(lines.end).map_or(src.len(), |at| *at as usize);
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(from..to);
        let names = config.query.capture_names();
        // Captured runs, per line, before overlaps are resolved.
        let mut caught: Vec<Vec<Caught>> = vec![Vec::new(); lines.len()];
        let mut matches = cursor.matches(&config.query, tree.root_node(), src.as_bytes());
        let mut seq = 0usize;
        while let Some(found) = matches.next() {
            for capture in found.captures {
                seq += 1;
                let hl = names
                    .get(capture.index as usize)
                    .and_then(|name| capture_to_hl(name));
                let node = capture.node.byte_range();
                clip_to_lines(&mut caught, &starts, node, (hl, seq), &lines);
            }
        }
        for (offset, runs) in caught.into_iter().enumerate() {
            let line = lines.start + offset;
            out[line] = partition(runs, width_of(&starts, line));
        }
        out
    }

    #[cfg(feature = "treesitter")]
    pub fn compute(&mut self, ext: &str, src: &str) -> LineSpans {
        use tree_sitter_highlight::{HighlightEvent, Highlighter as TsHighlighter};

        let Some(config) = self.langs.get(ext) else {
            return plain(src);
        };

        let starts = line_starts(src);
        let column = |at: usize| at - starts[line_of(&starts, at)] as usize;

        let mut out: LineSpans = vec![Vec::new(); starts.len() - 1];
        let mut ts = TsHighlighter::new();
        let Ok(events) = ts.highlight(config, src.as_bytes(), None, |_| None) else {
            return plain(src);
        };

        let mut stack: Vec<usize> = Vec::new();
        for ev in events {
            let Ok(ev) = ev else { return plain(src) };
            match ev {
                HighlightEvent::HighlightStart(h) => stack.push(h.0),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    let hl = stack
                        .last()
                        .and_then(|&i| capture_to_hl(concats_syntax::CAPTURES[i]));
                    let (mut l, mut c) = (line_of(&starts, start), column(start));
                    let el = line_of(&starts, end);
                    let ec = column(end);
                    while l < el {
                        let eol = width_of(&starts, l);
                        if l < out.len() && eol > c {
                            out[l].push(Span {
                                start: c,
                                end: eol,
                                hl,
                            });
                        }
                        l += 1;
                        c = 0;
                    }
                    if l < out.len() && ec > c {
                        out[l].push(Span {
                            start: c,
                            end: ec,
                            hl,
                        });
                    }
                }
            }
        }
        out
    }

    #[cfg(not(feature = "treesitter"))]
    pub fn compute(&mut self, _ext: &str, src: &str) -> LineSpans {
        // Web build: no C grammars. syntect/fancy-regex slots in here.
        plain(src)
    }
}

/// How many columns a line has, not counting the newline that ends it — the
/// width every span list has to add up to.
#[cfg(feature = "treesitter")]
fn width_of(starts: &[u32], line: usize) -> usize {
    starts
        .get(line + 1)
        .map_or(0, |end| (end - 1).saturating_sub(starts[line]) as usize)
}

/// One capture, clipped to a line: its columns, its colour, and where it came in
/// the query — which decides who wins when two patterns catch the same node.
#[cfg(feature = "treesitter")]
#[derive(Clone)]
struct Caught {
    from: usize,
    to: usize,
    hl: Option<Hl>,
    seq: usize,
}

/// Cut one capture's byte range into per-line column runs, keeping only the
/// lines in `wanted`.
///
/// A capture can span several lines (a block comment, a raw string), so it is
/// cut at the line boundaries, and the lines nobody asked for are dropped
/// rather than filled. That keeps the cost proportional to the viewport, not
/// the file.
#[cfg(feature = "treesitter")]
fn clip_to_lines(
    caught: &mut [Vec<Caught>],
    starts: &[u32],
    bytes: std::ops::Range<usize>,
    (hl, seq): (Option<Hl>, usize),
    wanted: &std::ops::Range<usize>,
) {
    let first = line_of(starts, bytes.start);
    let last = line_of(starts, bytes.end.max(bytes.start));
    for line in first..=last {
        if !wanted.contains(&line) || line + 1 >= starts.len() {
            continue;
        }
        let at = starts[line] as usize;
        let width = width_of(starts, line);
        let from = bytes.start.saturating_sub(at).min(width);
        let to = (bytes.end - at).min(width);
        if to > from {
            caught[line - wanted.start].push(Caught { from, to, hl, seq });
        }
    }
}

/// Turn overlapping captures into the complete, non-overlapping partition of a
/// line that a row is drawn from.
///
/// Two things have to hold for the result, and neither holds for the raw
/// captures. Every column must be covered, including the ones nothing captured:
/// a row draws its spans in order and paints nothing between them, so a gap is
/// text that never appears. And where captures overlap, the narrowest wins: a
/// query reports the enclosing node as well as the identifier inside it, and
/// the identifier's colour is the one to show.
#[cfg(feature = "treesitter")]
fn partition(mut caught: Vec<Caught>, width: usize) -> Vec<Span> {
    if width == 0 {
        return Vec::new();
    }
    // Widest first, so narrower captures paint over them; for two captures of
    // the same range, the later one first, so the earlier one lands on top.
    // That is tree-sitter's own precedence: a node caught by two patterns takes
    // the colour of whichever comes first in the query, which is how
    // `names.get(key)` reads as a method call rather than a field.
    caught.sort_by_key(|c| (c.from, std::cmp::Reverse(c.to), std::cmp::Reverse(c.seq)));
    let mut column: Vec<Option<Hl>> = vec![None; width];
    for run in caught {
        for at in column.iter_mut().take(run.to.min(width)).skip(run.from) {
            *at = run.hl;
        }
    }
    let mut out: Vec<Span> = Vec::new();
    for (at, hl) in column.into_iter().enumerate() {
        match out.last_mut() {
            Some(run) if run.hl == hl => run.end = at + 1,
            _ => out.push(Span {
                start: at,
                end: at + 1,
                hl,
            }),
        }
    }
    out
}

fn plain(src: &str) -> LineSpans {
    src.lines()
        .map(|l| {
            vec![Span {
                start: 0,
                end: l.len(),
                hl: None,
            }]
        })
        .collect()
}

#[cfg(all(test, feature = "treesitter"))]
mod tests {
    use super::*;

    const SRC: &str = r#"use std::collections::HashMap;

/// Resolve a name, falling back to the environment.
pub fn resolve(names: &HashMap<String, String>, key: &str) -> Option<String> {
    if let Some(found) = names.get(key) {
        return Some(found.clone()); // an inline comment
    }
    std::env::var(key).ok()
}

fn main() {
    let raw = r"a raw string";
    println!("{raw} {:?}", resolve(&HashMap::new(), "greeting"));
}
"#;

    fn oid(n: u8) -> gix::ObjectId {
        gix::ObjectId::from_hex(format!("{n:040x}").as_bytes()).expect("hex")
    }

    fn buf<'a>(text: &'a str, rev: u64, ext: &'a str) -> Buffer<'a> {
        Buffer {
            oid: oid(1),
            rev,
            editable: true,
            ext,
            text,
        }
    }

    /// Every column's colour, however the spans happen to be segmented.
    fn painted(hl: &mut Highlighter, buffer: Buffer<'_>, lines: usize) -> Vec<Vec<Option<Hl>>> {
        let text = buffer.text.to_string();
        let spans = hl.spans_for_lines(buffer, 0..lines);
        text.lines()
            .take(lines)
            .enumerate()
            .map(|(line, run)| colours(&spans[line], run.len()))
            .collect()
    }

    /// The test that makes an incremental reparse safe to have.
    ///
    /// A wrong `InputEdit` does not panic. It yields a tree that describes text
    /// nobody has, and every colour after it is quietly wrong. So the only test
    /// that means anything is that a tree carried through a series of edits
    /// says the same as a tree parsed from the final text.
    #[test]
    fn a_tree_carried_through_edits_says_what_a_fresh_parse_says() {
        let steps = [
            // typing at the end of a line
            SRC.replace(
                "let raw = r\"a raw string\";",
                "let raw = r\"a raw string!\";",
            ),
            // a new line in the middle
            SRC.replace("fn main() {", "fn main() {\n    let extra = 1;"),
            // deleting a line
            SRC.replace("    std::env::var(key).ok()\n", ""),
            // a multi-line insert, and a change on the first line at once
            SRC.replace(
                "use std::collections::HashMap;",
                "use std::fmt;\nuse std::collections::BTreeMap;\n\nconst N: usize = 2;",
            ),
        ];
        let mut carried = Highlighter::new();
        // Seed the tree with the original, then walk it through every step.
        painted(&mut carried, buf(SRC, 0, "rs"), 6);
        for (step, text) in steps.iter().enumerate() {
            let rev = step as u64 + 1;
            let got = painted(&mut carried, buf(text, rev, "rs"), 6);
            let want = painted(&mut Highlighter::new(), buf(text, 0, "rs"), 6);
            assert_eq!(got, want, "step {step} drifted from a fresh parse");
        }
    }

    #[test]
    fn a_buffer_and_a_read_only_blob_of_one_oid_do_not_share_a_tree() {
        // They can hold the same oid and only one of them drifts; sharing an
        // entry would have each reparsing the other's text every frame.
        let mut hl = Highlighter::new();
        let edited = "fn main() { let x = 1; }\n";
        let editable = Buffer {
            editable: true,
            ..buf(edited, 7, "rs")
        };
        let frozen = Buffer {
            editable: false,
            ..buf(SRC, 0, "rs")
        };
        hl.spans_for_lines(editable, 0..1);
        hl.spans_for_lines(frozen, 0..1);
        assert_eq!(hl.parsed.len(), 2);
    }

    /// The colour of every column, which is what a reader actually sees.
    ///
    /// Compared this way rather than span for span because the segmentation is
    /// an implementation detail: two adjacent captures of the same colour are one
    /// run here and two in the whole-file path, and that is a difference in how
    /// many quads get drawn, not in what anybody sees.
    fn colours(spans: &[Span], width: usize) -> Vec<Option<Hl>> {
        let mut out = vec![None; width];
        for span in spans {
            for at in out.iter_mut().take(span.end.min(width)).skip(span.start) {
                *at = span.hl;
            }
        }
        out
    }

    /// Asking for a few lines must give the same colours as asking for the
    /// file. Scoping the query is only sound if it changes the cost and not the
    /// answer, and the answer is what a frame is painted from.
    #[test]
    fn a_viewport_is_coloured_exactly_as_the_whole_file_would_be() {
        let mut hl = Highlighter::new();
        let whole = hl.compute("rs", SRC);
        let width = |line: usize| SRC.lines().nth(line).map_or(0, str::len);
        for lines in [0..3, 3..6, 4..5, 10..13] {
            let part = hl.spans_for_lines(buf(SRC, 0, "rs"), lines.clone());
            for line in lines.clone() {
                assert_eq!(
                    colours(&part[line], width(line)),
                    colours(&whole[line], width(line)),
                    "line {line} differs when asked for as {lines:?}"
                );
            }
        }
    }

    /// …and it covers every column, because a row paints its spans in order and
    /// paints nothing between them: a gap is text that never appears.
    #[test]
    fn a_viewport_line_is_covered_end_to_end() {
        let mut hl = Highlighter::new();
        let part = hl.spans_for_lines(buf(SRC, 0, "rs"), 0..6);
        for (line, spans) in part.iter().enumerate().take(6) {
            let width = SRC.lines().nth(line).map_or(0, str::len);
            let covered: usize = spans.iter().map(|s| s.end - s.start).sum();
            assert_eq!(covered, width, "line {line} has a hole in it");
        }
    }

    #[test]
    fn lines_outside_the_viewport_are_left_alone() {
        let mut hl = Highlighter::new();
        let part = hl.spans_for_lines(buf(SRC, 0, "rs"), 3..6);
        assert_eq!(part.len(), SRC.lines().count());
        assert!(
            part[0].is_empty() && part[9].is_empty(),
            "work was done for lines nobody asked for"
        );
        assert!(!part[3].is_empty(), "and the ones asked for were done");
    }

    #[test]
    fn the_tree_is_parsed_once_and_reused() {
        let mut hl = Highlighter::new();
        let cold = std::time::Instant::now();
        hl.spans_for_lines(buf(SRC, 0, "rs"), 0..4);
        let cold = cold.elapsed();
        let warm = std::time::Instant::now();
        hl.spans_for_lines(buf(SRC, 0, "rs"), 4..8);
        let warm = warm.elapsed();
        assert!(
            warm < cold,
            "a second viewport re-parsed the file: cold {cold:?}, warm {warm:?}"
        );
    }

    #[test]
    fn an_unknown_extension_falls_back_to_plain_text() {
        let mut hl = Highlighter::new();
        let part = hl.spans_for_lines(buf(SRC, 0, "wat"), 0..2);
        assert_eq!(part.len(), SRC.lines().count());
        assert!(part.iter().all(|l| l.iter().all(|s| s.hl.is_none())));
    }
}
