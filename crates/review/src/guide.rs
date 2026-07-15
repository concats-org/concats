//! The agent contract: how an agent says what to display.
//!
//! # The shape
//!
//! A review is a markdown document whose file links are transclusions. The
//! agent writes prose; wherever it drops a link to a location in the diff, the
//! app splices the actual diff rows in at that point:
//!
//! ```markdown
//! ## Gamepad camera
//!
//! The right stick now orbits the camera, reusing the mouse-drag deadzone curve
//! so stick and mouse produce identical motion.
//!
//! [examples/gamemaker/src/game_view.rs (1790:1802)](file:///…/game_view.rs#L1790-1802)
//!
//! Note that `dz` rescales past the deadzone rather than clamping — that is what
//! keeps slow pushes usable.
//! ```
//!
//! A link alone in a paragraph becomes a diff block. Everything else is prose.
//!
//! # Why links and not a JSON block list
//!
//! Because an LLM already emits this citation format natively. There is no new
//! syntax to teach and no schema to get wrong. The guide is also a perfectly
//! good document outside the app: readable in any markdown viewer, and the
//! links resolve in an editor.
//!
//! # The two rules that make it trustworthy
//!
//! **1. The agent never emits code.** Every code block is a reference; the app
//! renders the bytes from git. So an agent cannot fabricate, "clean up" or
//! quietly alter a line of the diff. The worst it can do is point somewhere
//! unhelpful, and that is visible.
//!
//! **2. The agent cannot make code disappear.** We keep track of which hunks it
//! placed, and everything it never mentioned lands under "Not discussed".
//! Reorder and explain, yes; shrink the diff, no. A review tool whose AI can
//! quietly drop a hunk is a liability.
//!
//! # The sharp edge: LLMs cannot do line arithmetic
//!
//! Ask an agent to compute `#L1790-1802` and you invite off-by-N errors, and a
//! review pointing at the wrong lines is worse than one pointing nowhere. So we
//! turn it around: [`manifest`] hands the agent ready-made links with correct
//! ranges, one per hunk. The agent's job is to select, order and explain. It
//! copies a link; it never authors coordinates.
//!
//! Resolution is forgiving anyway (any overlapping line range snaps to the
//! hunks it touches, a bare path takes the whole file), but validation is
//! strict: a reference that resolves to nothing becomes a loud
//! [`Row::Warning`], never a silent omission.

use std::collections::HashSet;

use concats_diff::{FileChange, Hunk, Row, load::Loaded};

/// What we hand the agent: every reviewable unit, with a ready-made link.
///
/// Markdown, not JSON, because this goes into a prompt: the agent copies a line
/// of it straight into its answer, no parsing on its side.
pub fn manifest(loaded: &Loaded, repo_root: &str) -> String {
    let mut md = String::new();
    let s = &loaded.stats;

    md.push_str(&format!(
        "# Diff manifest\n\n\
         {} files, +{}/-{}. Every hunk in this diff is below: a `+adds/-dels` line \
         with a preview, then the link.\n\n\
         Copy a **link line** into your review, alone on its own line, and it becomes \
         that diff. Do not write out code — reference it. Do not compose or edit a \
         line range; only use the links given here.\n\n",
        s.files, s.adds, s.dels
    ));

    for f in &loaded.files {
        if let Some(from) = &f.from {
            md.push_str(&format!(
                "\n## `{}` → `{}` (renamed {}%)\n\n",
                from,
                f.path,
                f.similarity.unwrap_or(0)
            ));
            if f.hunks.is_empty() {
                md.push_str("_Content unchanged — nothing to review._\n");
                continue;
            }
        } else {
            md.push_str(&format!(
                "\n## `{}` ({}, +{}/-{})\n\n",
                f.path, f.lang, f.adds, f.dels
            ));
        }

        for h in &f.hunks {
            md.push_str(&hunk_link(f, h, repo_root));
            md.push_str("\n\n");
        }
    }
    md
}

/// One hunk, as two lines: an annotation, then a bare link.
///
/// The link line stands on its own, nothing before or after it, so it can be
/// pasted verbatim and work. That is not cosmetic. The first version emitted `-
/// [link](…)  +13/-0  \`preview\``, and a line like that is not a lone link:
/// pasting it verbatim, exactly as the skill instructed, rendered it as prose
/// and counted zero coverage, with no error. The tool was handing out lines
/// that could not be used as told. So: annotation above, link alone below.
pub fn hunk_link(f: &FileChange, h: &Hunk, repo_root: &str) -> String {
    let end = h.new_start as usize + h.adds.max(1) - 1;
    format!(
        "+{}/-{}  `{}`\n[{} ({}:{})](file://{}/{}#L{}-{})",
        h.adds,
        h.dels,
        h.preview.replace('`', "'"),
        f.path.rsplit('/').next().unwrap_or(&f.path),
        h.new_start,
        end,
        repo_root.trim_end_matches('/'),
        f.path,
        h.new_start,
        end,
    )
}

/// A parsed piece of the agent's document.
enum Piece {
    Prose(String),
    /// A transclusion: the locator the agent wrote, and the 1-based line of the
    /// guide it appeared on, so the linter can point the agent at its mistake.
    Ref {
        locator: String,
        line: usize,
    },
}

/// A link occupying a paragraph on its own is a transclusion. Anything else —
/// including an inline link inside a sentence — stays prose.
fn parse(guide: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut prose = String::new();

    for (i, line) in guide.lines().enumerate() {
        let t = line.trim();
        if let Some(target) = lone_link_target(t) {
            if !prose.trim().is_empty() {
                out.push(Piece::Prose(std::mem::take(&mut prose)));
            }
            prose.clear();
            out.push(Piece::Ref {
                locator: target,
                line: i + 1,
            });
        } else {
            prose.push_str(line);
            prose.push('\n');
        }
    }
    if !prose.trim().is_empty() {
        out.push(Piece::Prose(prose));
    }
    out
}

// ---------------------------------------------------------------------------
// Lint — the CLI half of the app.
// ---------------------------------------------------------------------------

pub struct Problem {
    /// 1-based line in the review guide.
    pub line: usize,
    pub locator: String,
    pub message: String,
}

pub struct UncoveredFile {
    pub path: String,
    pub total_hunks: usize,
    /// Ready-made links for the hunks the agent never referenced.
    pub links: Vec<String>,
}

pub struct Report {
    /// Links that resolve to nothing. These are errors.
    pub broken: Vec<Problem>,
    /// The same hunk pulled in twice. A warning — sometimes deliberate.
    pub duplicates: Vec<Problem>,
    pub hunks_placed: usize,
    pub hunks_total: usize,
    pub lines_covered: usize,
    pub lines_total: usize,
    pub uncovered: Vec<UncoveredFile>,
    /// The agent wrote prose but referenced nothing at all.
    pub no_refs: bool,

    /// Covered lines that came from whole-new-file hunks.
    ///
    /// A brand-new 700-line file is a single hunk, so one link buys 700 lines
    /// of coverage. On a diff dominated by new files you could clear an 80%
    /// gate by listing them and never reading a line of the changes to existing
    /// code. We cannot forbid that — linking a new file is legitimate — so we
    /// make it visible: a review whose coverage is mostly new-file bulk gets
    /// told so.
    pub lines_covered_new_files: usize,
    pub new_file_hunks_placed: usize,
}

impl Report {
    pub fn coverage_pct(&self) -> f64 {
        if self.lines_total == 0 {
            return 100.0;
        }
        self.lines_covered as f64 / self.lines_total as f64 * 100.0
    }

    /// Share of covered lines that came from whole-new-file links.
    pub fn new_file_share(&self) -> f64 {
        if self.lines_covered == 0 {
            return 0.0;
        }
        self.lines_covered_new_files as f64 / self.lines_covered as f64 * 100.0
    }
}

/// Check an agent's review guide against the diff it claims to describe.
pub fn lint(guide: &str, loaded: &Loaded, repo_root: &str) -> Report {
    let mut broken = Vec::new();
    let mut duplicates = Vec::new();
    let mut placed: HashSet<String> = HashSet::new();
    let mut any_ref = false;

    // A link sharing its line with anything else renders as prose and covers
    // nothing. That has to be an error, not a shrug: it is the easiest way to
    // produce a guide that looks right and shows no code.
    for (i, raw) in guide.lines().enumerate() {
        if let Some(target) = inert_link(raw)
            && resolve(&target, loaded).is_some()
        {
            broken.push(Problem {
                line: i + 1,
                locator: target,
                message: "this link shares its line with other text, so it renders as \
                              prose and shows no diff. Put it alone on its own line."
                    .into(),
            });
        }
    }

    for piece in parse(guide) {
        let Piece::Ref { locator, line } = piece else {
            continue;
        };
        any_ref = true;

        match resolve(&locator, loaded) {
            Some(r) if !r.hunks.is_empty() => {
                for h in r.hunks {
                    if !placed.insert(h.id.clone()) {
                        duplicates.push(Problem {
                            line,
                            locator: locator.clone(),
                            message: format!(
                                "hunk {}:{} is already shown earlier in the review",
                                r.file.path, h.new_start
                            ),
                        });
                    }
                }
            }
            // Resolved to a pure rename — legitimate, nothing to cover.
            Some(r) if r.file.from.is_some() && r.file.hunks.is_empty() => {}
            Some(r) => broken.push(Problem {
                line,
                locator: locator.clone(),
                message: format!(
                    "`{}` matched, but the line range hits none of its {} hunks",
                    r.file.path,
                    r.file.hunks.len()
                ),
            }),
            None => broken.push(Problem {
                line,
                locator: locator.clone(),
                message: "no file in this diff matches that path".into(),
            }),
        }
    }

    // Coverage is measured in *changed lines*, not hunks: one 200-line hunk
    // matters more than five one-liners, and an agent that cites only the tiny
    // ones should not score well.
    let mut lines_total = 0usize;
    let mut lines_covered = 0usize;
    let mut hunks_total = 0usize;
    let mut lines_covered_new_files = 0usize;
    let mut new_file_hunks_placed = 0usize;
    let mut uncovered = Vec::new();

    for f in &loaded.files {
        let mut links = Vec::new();
        for h in &f.hunks {
            hunks_total += 1;
            let changed = h.adds + h.dels;
            lines_total += changed;
            if placed.contains(&h.id) {
                lines_covered += changed;
                if f.is_new {
                    lines_covered_new_files += changed;
                    new_file_hunks_placed += 1;
                }
            } else {
                links.push(hunk_link(f, h, repo_root));
            }
        }
        if !links.is_empty() {
            uncovered.push(UncoveredFile {
                path: f.path.clone(),
                total_hunks: f.hunks.len(),
                links,
            });
        }
    }

    Report {
        broken,
        duplicates,
        hunks_placed: placed.len(),
        hunks_total,
        lines_covered,
        lines_total,
        uncovered,
        no_refs: !any_ref,
        lines_covered_new_files,
        new_file_hunks_placed,
    }
}

/// Strip a leading markdown list marker (`- `, `* `, `+ `, `1. `), so a link
/// pasted straight out of a bulleted list still transcludes. Forgiving here
/// costs nothing and removes a whole class of silent failures.
fn strip_list_marker(line: &str) -> &str {
    let t = line.trim();
    for m in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(m) {
            return r.trim_start();
        }
    }
    // "1. " / "12. "
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty()
        && let Some(r) = t[digits.len()..].strip_prefix(". ")
    {
        return r.trim_start();
    }
    t
}

/// `[label](target)` alone on the line (bullet allowed) -> Some(target).
fn lone_link_target(line: &str) -> Option<String> {
    let rest = strip_list_marker(line).strip_prefix('[')?;
    let close = rest.find("](")?;
    let after = &rest[close + 2..];
    let end = after.rfind(')')?;
    // Nothing may follow the link: a trailing annotation means the author meant
    // prose, and guessing otherwise would silently swallow their words.
    if after[end + 1..].trim().is_empty() {
        Some(after[..end].trim().to_string())
    } else {
        None
    }
}

/// A link that will not transclude, because something else shares its line.
///
/// This is the failure that has to be loud. A link the author thinks embeds a
/// diff but renders as prose gives a guide that shows no code and scores zero
/// coverage for that hunk, silently. Detect it and say so, rather than letting
/// it pass as an innocent paragraph.
fn inert_link(line: &str) -> Option<String> {
    let t = strip_list_marker(line);
    if lone_link_target(line).is_some() {
        return None; // fine — it transcludes
    }
    // Does this line contain a link that points into the repo at all?
    let open = t.find("](")?;
    let after = &t[open + 2..];
    let end = after.find(')')?;
    let target = after[..end].trim();
    if target.starts_with("file://") || target.contains('#') {
        Some(target.to_string())
    } else {
        None
    }
}

/// A resolved reference: which file, and which of its hunks.
struct Resolved<'a> {
    file: &'a FileChange,
    hunks: Vec<&'a Hunk>,
}

/// Longest matching path suffix wins, so `game_view.rs` still resolves if it
/// is unambiguous, and a full path always resolves. Shared between guide-link
/// resolution and the CLI's comment anchoring so the two can never drift.
pub fn find_file<'a>(loaded: &'a Loaded, path: &str) -> Option<&'a FileChange> {
    loaded
        .files
        .iter()
        .filter(|f| path.ends_with(&f.path) || f.path.ends_with(path))
        .max_by_key(|f| f.path.len())
}

/// Resolve a locator against the diff.
///
/// Accepts, in the order an agent is most likely to get right:
///   `…/path/to/file.rs#L120-140`   a line range on the new side (manifest)
///   `…/path/to/file.rs#L120`       a single line
///   `…/path/to/file.rs`            the whole file
///
/// `file://` prefixes, absolute paths and repo-relative paths all work: we
/// match on path suffix, because an agent will not reliably reproduce the repo
/// root.
fn resolve<'a>(locator: &str, loaded: &'a Loaded) -> Option<Resolved<'a>> {
    let loc = locator.trim();
    let loc = loc.strip_prefix("file://").unwrap_or(loc);

    let (path_part, frag) = match loc.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (loc, None),
    };
    let path_part = path_part.trim_end_matches('/');
    let file = find_file(loaded, path_part)?;

    let Some(frag) = frag else {
        return Some(Resolved {
            file,
            hunks: file.hunks.iter().collect(),
        });
    };

    // `L120-140`, `L120:140`, `L120`
    let nums = frag.trim_start_matches(['L', 'l']);
    let (a, b) = match nums.split_once(['-', ':']) {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (nums.trim(), nums.trim()),
    };
    let (Ok(start), Ok(end)) = (a.parse::<u32>(), b.parse::<u32>()) else {
        // Unparseable fragment: fall back to the whole file rather than to
        // nothing.
        return Some(Resolved {
            file,
            hunks: file.hunks.iter().collect(),
        });
    };
    let (start, end) = (start.min(end), start.max(end));

    // Snap to every hunk the range touches. Forgiving on purpose: an agent that
    // is a line or two off still lands on the right hunk.
    let hunks: Vec<&Hunk> = file
        .hunks
        .iter()
        .filter(|h| {
            let h_start = h.new_start;
            let h_end = h.new_start + h.adds.max(1) as u32 - 1;
            h_start <= end && start <= h_end
        })
        .collect();

    Some(Resolved { file, hunks })
}

/// Render the agent's document into the row stream, and report on it.
pub struct Rendered {
    pub rows: Vec<Row>,
    /// Locators that matched nothing. Surfaced in the UI, never swallowed.
    pub unresolved: Vec<String>,
    pub hunks_placed: usize,
    pub hunks_total: usize,
}

pub fn render(guide: &str, loaded: &Loaded) -> Rendered {
    let mut rows = Vec::new();
    let mut unresolved = Vec::new();
    let mut placed: HashSet<&str> = HashSet::new();

    for piece in parse(guide) {
        match piece {
            Piece::Prose(md) => {
                if !md.trim().is_empty() {
                    rows.push(Row::Prose { md });
                }
            }
            Piece::Ref { locator: loc, .. } => match resolve(&loc, loaded) {
                Some(r) if !r.hunks.is_empty() => {
                    rows.push(file_header(r.file));
                    for h in r.hunks {
                        placed.insert(h.id.as_str());
                        rows.extend(h.rows.iter().cloned());
                    }
                }
                // Resolved to a file, but that file has no hunks (a pure
                // rename).
                Some(r) if r.file.from.is_some() => {
                    rows.push(file_header(r.file));
                }
                _ => {
                    rows.push(Row::Warning {
                        text: format!(
                            "The agent referenced `{loc}`, which does not match anything \
                             in this diff. Nothing was hidden — see “Not discussed” below."
                        ),
                    });
                    unresolved.push(loc);
                }
            },
        }
    }

    // --- coverage ------------------------------------------------------------
    // The agent may reorder and explain. It may not make code vanish. Anything
    // it never referenced gets appended verbatim.
    let total: usize = loaded.files.iter().map(|f| f.hunks.len()).sum();
    let missed: Vec<&FileChange> = loaded
        .files
        .iter()
        .filter(|f| f.hunks.iter().any(|h| !placed.contains(h.id.as_str())))
        .collect();

    if !missed.is_empty() {
        let n: usize = missed
            .iter()
            .map(|f| {
                f.hunks
                    .iter()
                    .filter(|h| !placed.contains(h.id.as_str()))
                    .count()
            })
            .sum();
        rows.push(Row::Prose {
            md: format!(
                "---\n\n## Not discussed\n\n\
                 The agent did not reference {n} of the {total} hunks in this diff. \
                 They are shown below unchanged. *An organized review never hides code — \
                 it only decides what to lead with.*"
            ),
        });
        for f in missed {
            rows.push(file_header(f));
            for h in f.hunks.iter().filter(|h| !placed.contains(h.id.as_str())) {
                rows.extend(h.gap_before.clone());
                rows.extend(h.rows.iter().cloned());
            }
        }
    }

    Rendered {
        rows,
        unresolved,
        hunks_placed: placed.len(),
        hunks_total: total,
    }
}

fn file_header(f: &FileChange) -> Row {
    Row::FileHeader {
        path: f.path.clone(),
        lang: f.lang,
        adds: f.adds,
        dels: f.dels,
        from: f.from.clone(),
        similarity: f.similarity,
    }
}
