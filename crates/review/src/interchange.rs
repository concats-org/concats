//! The comment interchange format: a review's comments as one markdown file.
//!
//! # The shape
//!
//! ```markdown
//! ---
//! schema: concats-app/1
//! repo: concats
//! base: 8f7c3d2a…                       (full oid of the merge base)
//! head: 1a2b3c4d…
//! ---
//!
//! ## `crates/review/src/store.rs`
//!
//! ### L305-L310
//! <!-- concats-app id=3 author="claude" at=1721234567 -->
//! issue: [HIGH] ids are allocated without consulting disk.
//!
//! ### L305-L310
//! <!-- concats-app id=4 author="Ada L" at=1721234599 reply-to=3 -->
//! the database allocates them now.
//! ```
//!
//! Per-file `##` sections, one `### L<start>-L<end>` entry per comment, body
//! until the next anchor. Line numbers are the 1-based new-side numbers the
//! manifest's `#Lstart-end` links carry — the one convention agents already
//! copy — and `### old L…` addresses deleted lines on the old side. The HTML
//! comment carries what only machines care about. Every key is optional on
//! import, so the smallest hand-written document is a heading and a body.
//!
//! A reply carries its own anchor: its root's, unless it was written on the
//! lines a fix moved the conversation to. `reply-to` names the entry it answers
//! by that entry's `id`. Ids are document-local — an export carries store ids,
//! a pull request's payload carries GitHub ids, and import remaps either onto
//! fresh store ids. `ref` records where an imported comment came from, so
//! running the same import twice converges instead of duplicating.
//!
//! Structural lines are matched by shape, not position: only `## ` plus a lone
//! code span opens a file section, only `### ` plus the anchor grammar opens an
//! entry, and inside a code fence nothing is structural. That is what keeps
//! fenced `suggestion` blocks and prose headings in bodies safe. The one escape
//! rule: a body line that would itself parse as structural is written with a
//! leading `\` and read back without it.
//!
//! # The lenient profile
//!
//! The same parser also accepts the file-grouped prompt format review bots post
//! (CodeRabbit et al.), so a pasted review imports directly:
//!
//! ```text
//! In `@apps/web/lib/env.ts`:
//! - Around line 229-234: Require the URL and token when enabled.
//! ```
//!
//! Openers are tolerant; resolution is not. Every entry, from either profile,
//! must land on lines of the loaded diff before anything is stored, the same
//! validation `comments add` applies. Nothing a document claims about blobs is
//! trusted: anchors re-resolve to `(blob oid, line)` on import.
//!
//! [`render_prompt`] emits this profile too (plus `old line` for deleted-line
//! anchors), so export has both voices: canonical for the archive, prompt for
//! handing a review to an agent.
//!
//! Parsing and rendering are pure and share [`Entry`], so `parse(&render(meta,
//! entries))` returns the same entries (modulo trimmed trailing whitespace).
//! Resolution against a [`Loaded`] diff is the only part that touches git
//! state.

use std::collections::{HashMap, HashSet};

use concats_diff::{Blob, FileChange, LineKind, Row, load::Loaded};
use gix::ObjectId;

use crate::{Error, guide, store::Comment};

pub const SCHEMA: &str = "concats-app/1";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    pub repo: Option<String>,
    pub base: Option<String>,
    pub head: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    New,
    Old,
}

/// One comment as the document carries it: 1-based display lines, unresolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: String,
    pub side: Side,
    /// 1-based, inclusive.
    pub start: u32,
    pub end: u32,
    pub body: String,
    pub author: Option<String>,
    /// This entry's identity within this document: the store id for an export,
    /// the GitHub id for a pull request's comments. `reply_to` refers to it;
    /// import allocates fresh store ids and remaps.
    pub id: Option<u64>,
    /// The `id` of the entry this answers, threading the document. Import
    /// resolves it to a store id — within the batch, or against a thread
    /// already stored under the same `external`.
    pub reply_to: Option<u64>,
    /// Where the comment came from, `"<source>:<id>"` — carried through a
    /// round-trip so a re-import converges instead of duplicating.
    pub external: Option<String>,
    pub created_at: Option<u64>,
    /// 1-based source line of the anchor, for lint-style error reporting.
    pub line: usize,
}

pub struct Document {
    pub meta: Meta,
    pub entries: Vec<Entry>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------

/// Parse a document in either profile. `Err` only for a schema this version
/// cannot honor; everything else degrades to a warning or a skipped entry.
/// Resolution against the diff is the real gate.
pub fn parse(text: &str) -> Result<Document, Error> {
    let lines: Vec<&str> = text.lines().collect();
    let (meta, mut at) = frontmatter(&lines)?;

    let mut doc = Document {
        meta,
        entries: Vec::new(),
        warnings: Vec::new(),
    };
    let mut fence: Option<(char, usize)> = None;
    let mut path: Option<String> = None;
    // Set once a canonical `## ` heading is seen: from then on the bot-style
    // openers are plain prose, so exported bodies can quote them freely.
    let mut canonical = false;
    let mut lenient_section = false;
    let mut open: Option<Entry> = None;

    while at < lines.len() {
        let line = lines[at];
        let lineno = at + 1;
        at += 1;

        if fence.is_none() {
            if let Some(p) = file_heading(line) {
                flush(&mut open, &mut doc);
                path = Some(p);
                canonical = true;
                lenient_section = false;
                continue;
            }
            if let Some((side, s, e)) = anchor_heading(line) {
                flush(&mut open, &mut doc);
                match &path {
                    Some(p) => open = Some(entry(p.clone(), side, s, e, lineno)),
                    None => doc.warnings.push(format!(
                        "line {lineno}: anchor before any file heading — skipped"
                    )),
                }
                continue;
            }
            if !canonical {
                if let Some(p) = lenient_file(line) {
                    flush(&mut open, &mut doc);
                    path = Some(p);
                    lenient_section = true;
                    continue;
                }
                if lenient_section && let Some((side, s, e, first)) = bullet_anchor(line) {
                    flush(&mut open, &mut doc);
                    let p = path.clone().expect("lenient_section implies a path");
                    let mut b = entry(p, side, s, e, lineno);
                    b.body = first;
                    open = Some(b);
                    continue;
                }
            }
            if let Some(b) = &mut open
                && b.body.trim().is_empty()
                && machine_meta(line, b)
            {
                continue;
            }
        }

        fence_toggle(line, &mut fence);
        if let Some(b) = &mut open {
            b.body.push_str(unescape(line));
            b.body.push('\n');
        }
    }
    flush(&mut open, &mut doc);
    Ok(doc)
}

fn frontmatter(lines: &[&str]) -> Result<(Meta, usize), Error> {
    let mut meta = Meta::default();
    if lines.first().map(|l| l.trim_end()) != Some("---") {
        return Ok((meta, 0));
    }
    let Some(close) = lines[1..].iter().position(|l| l.trim_end() == "---") else {
        return Ok((meta, 0)); // an unclosed `---` is prose, not frontmatter
    };
    for l in &lines[1..close + 1] {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "schema" => {
                if v != SCHEMA {
                    return Err(Error::UnsupportedSchema {
                        found: v.to_string(),
                    });
                }
            }
            "repo" => meta.repo = Some(v.to_string()),
            "base" => meta.base = Some(v.to_string()),
            "head" => meta.head = Some(v.to_string()),
            _ => {} // unknown keys are a later version's business
        }
    }
    Ok((meta, close + 2))
}

fn entry(path: String, side: Side, start: u32, end: u32, line: usize) -> Entry {
    Entry {
        path,
        side,
        start,
        end,
        body: String::new(),
        author: None,
        id: None,
        reply_to: None,
        external: None,
        created_at: None,
        line,
    }
}

fn flush(open: &mut Option<Entry>, doc: &mut Document) {
    let Some(mut e) = open.take() else { return };
    e.body = e.body.trim().to_string();
    if e.body.is_empty() {
        doc.warnings.push(format!(
            "line {}: entry has an empty body — skipped",
            e.line
        ));
        return;
    }
    doc.entries.push(e);
}

/// `## ` + a lone code span → the path inside it. A longer backtick run quotes
/// a path that itself contains backticks, CommonMark-style.
fn file_heading(line: &str) -> Option<String> {
    code_span(line.strip_prefix("## ")?.trim())
}

fn code_span(s: &str) -> Option<String> {
    let ticks = s.chars().take_while(|c| *c == '`').count();
    if ticks == 0 {
        return None;
    }
    let open = &s[..ticks];
    let rest = &s[ticks..];
    let close = rest.find(open)?;
    if !rest[close + ticks..].trim().is_empty() {
        return None; // trailing text: prose, not a section heading
    }
    let inner = rest[..close].trim();
    (!inner.is_empty()).then(|| inner.to_string())
}

/// `### L12`, `### L12-L14` (the `L` on the end is optional), `### old L…`.
fn anchor_heading(line: &str) -> Option<(Side, u32, u32)> {
    let rest = line.strip_prefix("### ")?.trim();
    let (side, rest) = match rest.strip_prefix("old ") {
        Some(r) => (Side::Old, r.trim_start()),
        None => (Side::New, rest),
    };
    let rest = rest.strip_prefix('L')?;
    let (a, b) = match rest.split_once('-') {
        Some((a, b)) => (a.trim(), b.trim().trim_start_matches('L')),
        None => (rest.trim(), rest.trim()),
    };
    range(a, b).map(|(s, e)| (side, s, e))
}

fn range(a: &str, b: &str) -> Option<(u32, u32)> {
    let (s, e) = (a.parse::<u32>().ok()?, b.parse::<u32>().ok()?);
    (s != 0 && e != 0).then(|| (s.min(e), s.max(e)))
}

/// `In `@path`:` / `In @path:` — the file opener review bots emit. The colon
/// is required; a bare path must look like one (no spaces), so prose starting
/// with "In " stays prose.
fn lenient_file(line: &str) -> Option<String> {
    let t = line.trim().strip_suffix(':')?;
    let rest = t.strip_prefix("In ")?.trim();
    let inner = if rest.starts_with('`') {
        code_span(rest)?
    } else if rest.contains(char::is_whitespace) {
        return None;
    } else {
        rest.to_string()
    };
    let inner = inner.trim().trim_start_matches('@');
    (!inner.is_empty()).then(|| inner.to_string())
}

/// `- Around line 229-234: body…` and its variants (`lines`, bare `Line X:`).
/// Bots only address the new side; our own prompt-style export additionally
/// writes `- Around old line 3: …` so old-side comments survive the trip.
fn bullet_anchor(line: &str) -> Option<(Side, u32, u32, String)> {
    let t = line.trim_start();
    let mut rest = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))?;
    let mut strip = |word: &str| -> bool {
        if rest.to_ascii_lowercase().starts_with(word) {
            rest = rest[word.len()..].trim_start();
            true
        } else {
            false
        }
    };
    strip("around ");
    let side = if strip("old ") { Side::Old } else { Side::New };
    if !strip("lines") && !strip("line") {
        return None;
    }
    let (nums, body) = rest.split_once(':')?;
    let nums = nums.trim();
    let (a, b) = match nums.split_once('-') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (nums, nums),
    };
    let (s, e) = range(a, b)?;
    let mut body = body.trim_start().to_string();
    if !body.is_empty() {
        body.push('\n');
    }
    Some((side, s, e, body))
}

/// `<!-- concats-app id=3 author="Ada L" at=17… reply-to=1 ref="github:218…"
/// -->` is the export-side extra. Every key is optional; unknown keys are
/// ignored.
fn machine_meta(line: &str, e: &mut Entry) -> bool {
    let t = line.trim();
    let Some(rest) = t
        .strip_prefix("<!-- concats-app")
        .and_then(|r| r.strip_suffix("-->"))
    else {
        return false;
    };
    for (k, v) in parse_kv(rest) {
        match k.as_str() {
            "id" => e.id = v.parse().ok(),
            "author" => e.author = Some(v),
            "at" => e.created_at = v.parse().ok(),
            "reply-to" => e.reply_to = v.parse().ok(),
            "ref" => e.external = Some(v),
            _ => {}
        }
    }
    true
}

/// `key=value` pairs, values either bare tokens or `"quoted"` with `\"`/`\\`.
fn parse_kv(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| *c != '=' && !c.is_whitespace()) {
            key.push(c);
        }
        if key.is_empty() {
            return out;
        }
        if chars.next_if_eq(&'=').is_none() {
            continue; // a stray token, not a pair
        }
        let mut val = String::new();
        if chars.next_if_eq(&'"').is_some() {
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => val.push(chars.next().unwrap_or('\\')),
                    _ => val.push(c),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                val.push(c);
            }
        }
        out.push((key, val));
    }
}

fn fence_toggle(line: &str, fence: &mut Option<(char, usize)>) {
    let t = line.trim_start();
    let Some(c) = t.chars().next().filter(|c| *c == '`' || *c == '~') else {
        return;
    };
    let run = t.chars().take_while(|ch| *ch == c).count();
    match fence {
        Some((fc, flen)) => {
            if c == *fc && run >= *flen && t[run..].trim().is_empty() {
                *fence = None;
            }
        }
        None => {
            if run >= 3 {
                *fence = Some((c, run));
            }
        }
    }
}

fn unescape(line: &str) -> &str {
    match line.strip_prefix('\\') {
        Some(r) if r.starts_with("##") || r.starts_with("<!--") => r,
        _ => line,
    }
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

/// The canonical document for a set of entries, grouped by path in the order
/// given (callers sort). `parse` reads it back to the same entries.
pub fn render(meta: &Meta, entries: &[Entry]) -> String {
    let mut md = format!("---\nschema: {SCHEMA}\n");
    for (k, v) in [
        ("repo", &meta.repo),
        ("base", &meta.base),
        ("head", &meta.head),
    ] {
        if let Some(v) = v {
            md.push_str(&format!("{k}: {v}\n"));
        }
    }
    md.push_str(
        "---\n\n# Review comments\n\nLine anchors are 1-based new-side line numbers; \
         `old L…` ranges address deleted\nlines on the old side.\n",
    );

    let mut last_path = None;
    for e in entries {
        if last_path != Some(e.path.as_str()) {
            md.push_str(&format!("\n## {}\n", backtick_span(&e.path)));
            last_path = Some(e.path.as_str());
        }
        let old = if e.side == Side::Old { "old " } else { "" };
        if e.start == e.end {
            md.push_str(&format!("\n### {old}L{}\n", e.start));
        } else {
            md.push_str(&format!("\n### {old}L{}-L{}\n", e.start, e.end));
        }
        if e.id.is_some()
            || e.author.is_some()
            || e.created_at.is_some()
            || e.reply_to.is_some()
            || e.external.is_some()
        {
            md.push_str("<!-- concats-app");
            if let Some(id) = e.id {
                md.push_str(&format!(" id={id}"));
            }
            if let Some(a) = &e.author {
                md.push_str(&format!(" author=\"{}\"", quoted(a)));
            }
            if let Some(at) = e.created_at {
                md.push_str(&format!(" at={at}"));
            }
            if let Some(parent) = e.reply_to {
                md.push_str(&format!(" reply-to={parent}"));
            }
            if let Some(source) = &e.external {
                md.push_str(&format!(" ref=\"{}\"", quoted(source)));
            }
            md.push_str(" -->\n");
        }
        md.push('\n');
        push_body(&mut md, &e.body);
    }
    md
}

/// The terse, file-grouped prompt review bots post: the export twin of the
/// lenient profile, so a pasted export imports right back. Pasteable straight
/// into an agent prompt. Carries no ids, authors or timestamps, only anchors
/// and bodies (re-import dedupes on those, so the trip is still a no-op).
///
/// A reply is indented under its root but keeps its own anchor, because
/// `bullet_anchor` trims leading whitespace: the thread reads as one, and
/// nothing is smuggled into a body that dedupe compares byte for byte.
pub fn render_prompt(entries: &[Entry]) -> String {
    let mut md = String::from(
        "Verify each finding against the current code. Fix only still-valid issues, skip \
         the rest with a brief reason, keep changes minimal, and validate.\n\nInline comments:\n",
    );
    let mut last_path = None;
    for e in entries {
        if last_path != Some(e.path.as_str()) {
            md.push_str(&format!("\nIn `@{}`:\n", e.path));
            last_path = Some(e.path.as_str());
        }
        let old = if e.side == Side::Old { "old " } else { "" };
        let range = if e.start == e.end {
            format!("{}", e.start)
        } else {
            format!("{}-{}", e.start, e.end)
        };
        let lead = if e.reply_to.is_some() { "  " } else { "" };
        // A one-line body rides on the bullet; anything longer goes flush-left
        // below it, so fence lines pass through the parser's fence tracking.
        if e.body.lines().count() <= 1 {
            md.push_str(&format!("{lead}- Around {old}line {range}: {}\n", e.body));
        } else {
            md.push_str(&format!("{lead}- Around {old}line {range}:\n"));
            push_body(&mut md, &e.body);
        }
    }
    md
}

/// Body lines verbatim, except that a line which would itself parse as
/// structural gets the `\` escape — tracked through fences, because escaping
/// inside a code block would corrupt it (and nothing is structural there).
fn push_body(md: &mut String, body: &str) {
    let mut fence: Option<(char, usize)> = None;
    for line in body.lines() {
        let structural = fence.is_none()
            && (file_heading(line).is_some()
                || anchor_heading(line).is_some()
                || line.starts_with("<!-- concats-app"));
        if structural {
            md.push('\\');
        }
        fence_toggle(line, &mut fence);
        md.push_str(line);
        md.push('\n');
    }
}

fn quoted(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"")
}

fn backtick_span(path: &str) -> String {
    let longest = path.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    if longest == 0 {
        return format!("`{path}`");
    }
    let fence = "`".repeat(longest + 1);
    format!("{fence} {path} {fence}")
}

// ---------------------------------------------------------------------------
// Resolution — the import gate, shared with `comments add`
// ---------------------------------------------------------------------------

/// An entry mapped into store terms: content-addressed, 0-based.
pub struct ResolvedEntry {
    pub path: String,
    pub blob: ObjectId,
    pub start: u32,
    pub end: u32,
}

pub enum ResolveError<'a> {
    UnknownPath,
    /// The range includes lines the diff does not show on that side.
    MissingLines {
        file: &'a FileChange,
        missing: Vec<u32>,
    },
}

/// 1-based display line → `(blob idx, 0-based blob line)`.
pub type LineMap = HashMap<u32, (u32, u32)>;

/// `(new_no → (blob idx, blob line), old_no → …)` for a file's hunks. The new
/// side covers additions and context (the GUI allows commenting both); the
/// old side covers deletions only — that is what `old L…` addresses.
pub fn line_maps(file: &FileChange) -> (LineMap, LineMap) {
    let mut new = HashMap::new();
    let mut old = HashMap::new();
    for r in file.hunks.iter().flat_map(|h| h.rows.iter()) {
        if let Row::Code {
            kind,
            old_no,
            new_no,
            blob,
            line,
        } = r
        {
            if let Some(n) = new_no {
                new.insert(*n, (*blob, *line));
            }
            if *kind == LineKind::Del
                && let Some(n) = old_no
            {
                old.insert(*n, (*blob, *line));
            }
        }
    }
    (new, old)
}

/// Re-anchor one entry against the loaded diff — the same validation
/// `comments add` applies, batched: every line of the range must be shown by
/// the diff on the entry's side.
pub fn resolve_entry<'a>(loaded: &'a Loaded, e: &Entry) -> Result<ResolvedEntry, ResolveError<'a>> {
    let file =
        guide::find_file(loaded, e.path.trim_end_matches('/')).ok_or(ResolveError::UnknownPath)?;
    let (new_map, old_map) = line_maps(file);
    let map = match e.side {
        Side::New => &new_map,
        Side::Old => &old_map,
    };
    let missing: Vec<u32> = (e.start..=e.end).filter(|n| !map.contains_key(n)).collect();
    if !missing.is_empty() {
        return Err(ResolveError::MissingLines { file, missing });
    }
    let (blob, line_s) = map[&e.start];
    let (_, line_e) = map[&e.end];
    Ok(ResolvedEntry {
        path: file.path.clone(),
        blob: loaded.blobs[blob as usize].oid,
        start: line_s,
        end: line_e,
    })
}

// ---------------------------------------------------------------------------
// Export — store comments back into entries
// ---------------------------------------------------------------------------

/// Which blob oids appear on which side of a row stream — how an exported
/// comment knows whether its anchor is `L…` or `old L…`.
pub fn blob_sides<'a>(
    rows: impl Iterator<Item = &'a Row>,
    blobs: &[Blob],
) -> (HashSet<ObjectId>, HashSet<ObjectId>) {
    let mut old = HashSet::new();
    let mut new = HashSet::new();
    for r in rows {
        if let Row::Code { kind, blob, .. } = r {
            let oid = blobs[*blob as usize].oid;
            match kind {
                LineKind::Del => old.insert(oid),
                _ => new.insert(oid),
            };
        }
    }
    (old, new)
}

/// Stored comments as document entries, 1-based. A comment whose blob is on
/// neither side is stale for this range: exported as recorded (new side, best
/// effort), and it will not re-import against this diff. Callers count those
/// via `outside_range` and warn.
pub fn entries_from(
    comments: &[Comment],
    old: &HashSet<ObjectId>,
    new: &HashSet<ObjectId>,
) -> Vec<Entry> {
    comments
        .iter()
        .map(|c| Entry {
            path: c.path.clone(),
            side: if !new.contains(&c.anchor.blob) && old.contains(&c.anchor.blob) {
                Side::Old
            } else {
                Side::New
            },
            start: c.anchor.start + 1,
            end: c.anchor.end + 1,
            body: c.body.clone(),
            author: c.author.clone(),
            id: Some(c.id),
            reply_to: c.parent,
            external: c.external.clone(),
            created_at: Some(c.created_at),
            line: 0,
        })
        .collect()
}

pub fn outside_range(c: &Comment, old: &HashSet<ObjectId>, new: &HashSet<ObjectId>) -> bool {
    !new.contains(&c.anchor.blob) && !old.contains(&c.anchor.blob)
}

#[cfg(test)]
mod tests {
    use concats_diff::Hunk;

    use super::*;

    fn oid(n: u8) -> ObjectId {
        ObjectId::from_hex(format!("{n:040x}").as_bytes()).unwrap()
    }

    fn entry_full(path: &str, side: Side, start: u32, end: u32) -> Entry {
        Entry {
            path: path.into(),
            side,
            start,
            end,
            body: String::new(),
            author: None,
            id: None,
            reply_to: None,
            external: None,
            created_at: None,
            line: 0,
        }
    }

    /// Everything but the source line, which parse assigns and render ignores.
    fn key(e: &Entry) -> impl PartialEq + std::fmt::Debug + '_ {
        (
            &e.path,
            e.side,
            e.start,
            e.end,
            &e.body,
            &e.author,
            e.id,
            e.reply_to,
            &e.external,
            e.created_at,
        )
    }

    #[test]
    fn round_trips_through_render_and_parse() {
        let meta = Meta {
            repo: Some("concats".into()),
            base: Some(oid(1).to_string()),
            head: Some(oid(2).to_string()),
        };
        let mut a = entry_full("src/store.rs", Side::New, 12, 14);
        a.body = "issue: [HIGH] off-by-one.\n\n```suggestion\nfor i in 0..n {\n```".into();
        a.author = Some("Ada \"L\"".into());
        a.id = Some(3);
        a.created_at = Some(1_721_234_567);

        let mut b = entry_full("src/store.rs", Side::Old, 120, 120);
        // A fenced anchor stays verbatim; an unfenced one needs the escape.
        b.body = "```\n### L1\n---\n```\n\n### L12\n\n## `x`".into();

        let mut c = entry_full("dir with space/a`b.rs", Side::New, 1, 2);
        c.body = "nit: naming — überschreiben?".into();

        // A reply repeats its root's anchor and names it by document id.
        let mut r = entry_full("src/store.rs", Side::New, 12, 14);
        r.body = "the database allocates them now.".into();
        r.id = Some(4);
        r.reply_to = Some(3);
        r.external = Some("github:2181234567".into());

        let md = render(&meta, &[a.clone(), r.clone(), b.clone(), c.clone()]);
        let doc = parse(&md).unwrap();
        assert_eq!(doc.meta, meta);
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert_eq!(doc.entries.len(), 4);
        assert_eq!(key(&doc.entries[0]), key(&a));
        assert_eq!(key(&doc.entries[1]), key(&r));
        assert_eq!(key(&doc.entries[2]), key(&b));
        assert_eq!(key(&doc.entries[3]), key(&c));
    }

    #[test]
    fn the_prompt_profile_nests_a_reply_without_touching_its_body() {
        let mut root = entry_full("src/lib.rs", Side::New, 63, 68);
        root.body = "this leaks on the error path".into();
        root.id = Some(1);
        let mut reply = entry_full("src/lib.rs", Side::New, 63, 68);
        reply.body = "fixed — the early return drops it".into();
        reply.id = Some(2);
        reply.reply_to = Some(1);

        let md = render_prompt(&[root.clone(), reply.clone()]);
        assert!(
            md.contains("\n- Around line 63-68: this leaks on the error path\n"),
            "{md}"
        );
        assert!(
            md.contains("\n  - Around line 63-68: fixed — the early return drops it\n"),
            "{md}"
        );
        // The indent is presentation only: the anchor still parses, and the
        // bodies come back byte for byte, which is what import dedupes on.
        let back = parse(&md).unwrap();
        assert_eq!(back.entries.len(), 2);
        assert_eq!(back.entries[0].body, root.body);
        assert_eq!(back.entries[1].body, reply.body);
        assert_eq!(
            (back.entries[1].start, back.entries[1].end),
            (reply.start, reply.end)
        );
    }

    #[test]
    fn parses_the_minimal_handwritten_document() {
        let doc =
            parse("## `src/cli.rs`\n\n### L493-509\nsuggestion: extract the parsing.\n").unwrap();
        assert_eq!(doc.entries.len(), 1);
        let e = &doc.entries[0];
        assert_eq!(
            (e.path.as_str(), e.side, e.start, e.end),
            ("src/cli.rs", Side::New, 493, 509)
        );
        assert_eq!(e.body, "suggestion: extract the parsing.");
        assert_eq!(
            (e.author.as_deref(), e.id, e.created_at),
            (None, None, None)
        );
    }

    #[test]
    fn parses_a_pasted_bot_review() {
        let doc = parse(
            "Inline comments:\n\
             In `@apps/web/lib/env.ts`:\n\
             - Around line 229-234: Update the environment schema containing AUTHZED_ENABLED\n\
               to require the URL and token when enabled.\n\
             \n\
             In @docker/authzed-smoke.sh:\n\
             - Line 5: Separate declaration and assignment for SCRIPT_DIR.\n\
             - Around lines 8-9: Mark each variable readonly after assignment.\n",
        )
        .unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert_eq!(doc.entries.len(), 3);
        let e = &doc.entries[0];
        assert_eq!(
            (e.path.as_str(), e.start, e.end),
            ("apps/web/lib/env.ts", 229, 234)
        );
        assert_eq!(
            e.body,
            "Update the environment schema containing AUTHZED_ENABLED\n\
             to require the URL and token when enabled."
        );
        assert_eq!(
            (
                doc.entries[1].path.as_str(),
                doc.entries[1].start,
                doc.entries[1].end
            ),
            ("docker/authzed-smoke.sh", 5, 5)
        );
        assert_eq!((doc.entries[2].start, doc.entries[2].end), (8, 9));
    }

    #[test]
    fn prompt_render_round_trips_through_the_lenient_profile() {
        let mut a = entry_full("apps/web/lib/env.ts", Side::New, 229, 234);
        a.body = "Require the URL and token when enabled.".into();
        a.author = Some("claude".into()); // dropped by design: prompt carries no metadata
        a.id = Some(9);
        let mut b = entry_full("src/a.rs", Side::Old, 3, 3);
        b.body = "why was doomed() removed?".into();
        let mut c = entry_full("src/a.rs", Side::New, 2, 3);
        c.body = "suggestion: keep line2 stable.\n\n```suggestion\nline2\n```".into();

        let md = render_prompt(&[a.clone(), b.clone(), c.clone()]);
        let doc = parse(&md).unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert_eq!(doc.entries.len(), 3);
        for (got, want) in doc.entries.iter().zip([&a, &b, &c]) {
            assert_eq!(
                (&got.path, got.side, got.start, got.end, &got.body),
                (&want.path, want.side, want.start, want.end, &want.body)
            );
            assert_eq!(
                (got.author.as_deref(), got.id, got.created_at),
                (None, None, None)
            );
        }
    }

    #[test]
    fn frontmatter_is_optional_and_gated_on_schema() {
        assert_eq!(
            parse("## `a.rs`\n### L1\nx\n").unwrap().meta,
            Meta::default()
        );
        // Unknown keys ride along; known ones land.
        let doc = parse("---\nschema: concats-app/1\nbase: abc\nflavor: mild\n---\n").unwrap();
        assert_eq!(doc.meta.base.as_deref(), Some("abc"));
        assert!(parse("---\nschema: concats-app/2\n---\n").is_err());
    }

    #[test]
    fn skips_bodyless_entries_and_anchors_without_a_file() {
        let doc = parse("### L3\nlost\n\n## `a.rs`\n\n### L1\n\n### L2\nkept\n").unwrap();
        assert_eq!(doc.entries.len(), 1);
        assert_eq!(doc.entries[0].body, "kept");
        assert_eq!(doc.warnings.len(), 2);
    }

    #[test]
    fn prose_headings_and_bot_openers_in_bodies_stay_body() {
        let doc = parse(
            "## `a.rs`\n\n### L1\nSee below.\n\n### Why this matters\nIn `x`:\n- Line 9: quoted\n",
        )
        .unwrap();
        assert_eq!(doc.entries.len(), 1);
        assert!(doc.entries[0].body.contains("### Why this matters"));
        assert!(doc.entries[0].body.contains("- Line 9: quoted"));
    }

    // -- resolution ---------------------------------------------------------

    /// One file, one hunk: old line 10 deleted, new lines 10-11 added, with a
    /// context line at 12 — the shapes `git_load` lowers.
    fn fixture() -> Loaded {
        let old_b = Blob::new(oid(7), "rs".into(), "old\n".into());
        let new_b = Blob::new(oid(8), "rs".into(), "a\nb\nc\n".into());
        let rows = vec![
            Row::Code {
                kind: LineKind::Del,
                old_no: Some(10),
                new_no: None,
                blob: 0,
                line: 9,
            },
            Row::Code {
                kind: LineKind::Add,
                old_no: None,
                new_no: Some(10),
                blob: 1,
                line: 9,
            },
            Row::Code {
                kind: LineKind::Add,
                old_no: None,
                new_no: Some(11),
                blob: 1,
                line: 10,
            },
            Row::Code {
                kind: LineKind::Context,
                old_no: Some(11),
                new_no: Some(12),
                blob: 1,
                line: 11,
            },
        ];
        Loaded {
            merge_base: Some(oid(1)),
            head: Some(oid(2)),
            git_dir: std::path::PathBuf::new(),
            workdir: None,
            stage: Vec::new(),
            tree: vec!["src/a.rs".into()],
            files: vec![FileChange {
                id: "f0".into(),
                path: "src/a.rs".into(),
                is_new: false,
                from: None,
                similarity: None,
                lang: "rust",
                adds: 2,
                dels: 1,
                hunks: vec![Hunk {
                    id: "h0".into(),
                    old_start: 10,
                    new_start: 10,
                    adds: 2,
                    dels: 1,
                    gap_before: None,
                    preview: String::new(),
                    rows,
                }],
                gap_after: None,
            }],
            blobs: vec![old_b, new_b],
            stats: Default::default(),
        }
    }

    #[test]
    fn resolves_new_and_old_side_ranges() {
        let loaded = fixture();
        let e = entry_full("a.rs", Side::New, 10, 12);
        let r = resolve_entry(&loaded, &e).ok().unwrap();
        assert_eq!(
            (r.path.as_str(), r.blob, r.start, r.end),
            ("src/a.rs", oid(8), 9, 11)
        );

        let o = entry_full("src/a.rs", Side::Old, 10, 10);
        let r = resolve_entry(&loaded, &o).ok().unwrap();
        assert_eq!((r.blob, r.start, r.end), (oid(7), 9, 9));
    }

    #[test]
    fn reports_unknown_paths_and_missing_lines() {
        let loaded = fixture();
        assert!(matches!(
            resolve_entry(&loaded, &entry_full("nope.rs", Side::New, 1, 1)),
            Err(ResolveError::UnknownPath)
        ));
        match resolve_entry(&loaded, &entry_full("a.rs", Side::New, 11, 14)) {
            Err(ResolveError::MissingLines { missing, .. }) => assert_eq!(missing, vec![13, 14]),
            _ => panic!("expected MissingLines"),
        }
        // Old side addresses deletions only.
        assert!(matches!(
            resolve_entry(&loaded, &entry_full("a.rs", Side::Old, 11, 11)),
            Err(ResolveError::MissingLines { .. })
        ));
    }

    #[test]
    fn export_entries_carry_sides_and_one_based_lines() {
        let loaded = fixture();
        let rows: Vec<&Row> = loaded.files[0].hunks[0].rows.iter().collect();
        let (old, new) = blob_sides(rows.into_iter(), &loaded.blobs);
        let anchor = |blob, start, end| crate::store::Anchor { blob, start, end };
        let comments = vec![
            Comment {
                id: 1,
                path: "src/a.rs".into(),
                anchor: anchor(oid(8), 9, 10),
                body: "boundary".into(),
                author: Some("claude".into()),
                created_at: 5,
                parent: None,
                external: None,
                cursors: None,
            },
            Comment {
                id: 2,
                path: "src/a.rs".into(),
                anchor: anchor(oid(7), 9, 9),
                body: "old side".into(),
                author: None,
                created_at: 6,
                parent: Some(1),
                external: None,
                cursors: None,
            },
            Comment {
                id: 3,
                path: "gone.rs".into(),
                anchor: anchor(oid(9), 0, 0),
                body: "stale".into(),
                author: None,
                created_at: 7,
                parent: None,
                external: None,
                cursors: None,
            },
        ];
        let entries = entries_from(&comments, &old, &new);
        assert_eq!(
            (entries[0].side, entries[0].start, entries[0].end),
            (Side::New, 10, 11)
        );
        assert_eq!(entries[1].side, Side::Old);
        assert_eq!(entries[2].side, Side::New); // best effort
        let stale: Vec<bool> = comments
            .iter()
            .map(|c| outside_range(c, &old, &new))
            .collect();
        assert_eq!(stale, vec![false, false, true]);
    }
}
