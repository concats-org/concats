//! The row stream: what a review is made of.
//!
//! A review here is not a list of file diffs. It is one flat, ordered stream of
//! rows, and diff lines and prose are peers in it. That is what lets an agent
//! organize a change: group by concern instead of by path, put a rationale in
//! front of the hunk it explains, collapse a rename to one line. It is also why
//! the model is its own crate — the app draws the same stream with makepad, a
//! terminal renderer prints it with ANSI, and neither belongs in the model.
//!
//! [`FileChange`] and [`Hunk`] are the intermediate the loader produces and an
//! agent reorders; [`Row`] is what they lower to.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Context,
    Add,
    Del,
}

#[derive(Clone)]
pub enum Row {
    Title {
        text: String,
    },
    /// Agent prose. Markdown, so it carries links, emphasis, tables.
    Prose {
        md: String,
    },
    /// The agent referenced something that does not exist in this diff. Loud on
    /// purpose: a bad reference must never look like an absent one.
    Warning {
        text: String,
    },
    FileHeader {
        path: String,
        lang: &'static str,
        adds: usize,
        dels: usize,
        /// Set when the file was renamed or moved. A 100% rename has nothing to
        /// review, so it gets no code rows and collapses to this one line
        /// instead of +N/-N of noise.
        from: Option<String>,
        similarity: Option<u8>,
    },
    /// A run of unchanged lines collapsed out of the diff: the gap between two
    /// hunks, or a file's unchanged head or tail. It carries where the run
    /// sits, not only how long it is, because the indicator that renders it can
    /// reveal it — see [`CollapsedEnd`]. The run is unchanged on both sides, so its
    /// lines advance in step and one offset indexes both.
    Collapsed {
        /// The new-side blob the hidden lines live on. Context rows reference
        /// the new side, so this is the blob revealed lines get.
        blob: u32,
        /// First hidden line on each side, 0-based.
        old_start: u32,
        new_start: u32,
        count: u32,
    },
    /// Lines this range removed, collapsed at the point they went from.
    ///
    /// The whole-file view shows the file as it is at the head, and the head
    /// does not contain these lines, so they have nowhere to sit. This row
    /// marks where they were and offers them as a reveal, instead of splicing
    /// content into a file that never had it.
    Removed {
        /// The old-side blob the removed lines live on — the only place they
        /// still exist.
        blob: u32,
        /// The removed run, 0-based and inclusive, on that blob.
        start: u32,
        end: u32,
    },
    /// One diff line — a *reference*, not a copy. 16 bytes.
    Code {
        kind: LineKind,
        old_no: Option<u32>,
        new_no: Option<u32>,
        /// Index into the blob table these rows were lowered against
        /// ([`crate::Loaded::blobs`]). Del rows point at the old blob,
        /// inserted and context rows at the new one.
        blob: u32,
        line: u32,
    },
    /// The head of one hunk: it hosts the seen tick box and names the ranges.
    /// The shared lowering emits it as the first row of every hunk, so every
    /// view (guide, classic, sessions) gets it for free. Each side is
    /// the changed lines of the hunk — deletions on the old blob, additions on
    /// the new — and their `(blob oid, line)` pairs are the keys the review
    /// store marks as seen.
    HunkBar {
        old: Option<Side>,
        new: Option<Side>,
    },
    /// A review comment, spliced in right below the last line of its range,
    /// wherever that line appears. The anchor lives in the store; this row is
    /// only its rendering. `parent` is the thread root when this is a reply: it
    /// picks the indented template and lets the composer walk a thread, since
    /// `inject_comments` emits a thread's rows contiguously.
    Comment {
        id: u64,
        parent: Option<u64>,
        body: String,
        meta: String,
    },
    /// 10pt of air between a card's code and the chrome that brackets it — its
    /// header and bottom cap, or the collapsed-run bands that sit against them.
    /// Inserted by `finalize_cards` with the cap, so no row has to know whether
    /// it is first or last in its card.
    Spacer,
    /// The bottom cap of a file card: closes the rounded border that the
    /// `FileHeader` opened. Inserted by `finalize_cards` after the last row
    /// that belongs to a file, so the virtualized list can draw a "card"
    /// without any row knowing about its neighbours.
    CardEnd,
    /// The inline comment composer, spliced in below the last line of the
    /// range being commented (GitHub's interaction). Transient UI state, not
    /// document content: the row marks the place, and whoever spliced it in
    /// owns the range it targets.
    Composer,
}

/// Where the wall-clock went.
#[derive(Default, Clone)]
pub struct LoadStats {
    pub git_ms: f64,    // merge-base + tree diff + blob reads
    pub rename_ms: f64, // diffcore-rename
    pub diff_ms: f64,   // histogram line diff (imara)
    pub lower_ms: f64,  // hunks -> rows
    pub total_ms: f64,
    pub files: usize,
    pub skipped_binary: usize,
    pub bytes: usize,
    pub adds: usize,
    pub dels: usize,
    pub renames_exact: usize,
    pub renames_inexact: usize,
    pub rename_limit_hit: bool,
}

/// A contiguous, inclusive run of lines (`start..=end`, 0-based) on one blob,
/// named by its index into the blob table. The one in-memory line range: a
/// hunk's changed-line runs (`HunkBar`) and a comment selection's two sides
/// (`Compose`) are both `Side`s. (Persisted comments key the same shape by blob
/// *oid* — see `store::Anchor`.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Side {
    pub blob: u32,
    pub start: u32,
    pub end: u32,
}

/// Which end of a collapsed run an expansion takes its lines from. Named after
/// the run, not after a screen direction: `Head` reveals the run's first lines,
/// which join the code above the indicator, and `Tail` its last, which lead
/// into the code below. That is also how the design's two chevrons read — each
/// sits at the edge of the band it grows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CollapsedEnd {
    Head,
    Tail,
}

/// One addressable change: a hunk with its context, already lowered to rows but
/// not yet placed in the document.
///
/// This is the unit an agent selects and orders. Its `id` is stable, so the
/// agent references it (`"refs": ["h7"]`) and never transcribes code. That is
/// what makes an agent-organized review trustworthy: the agent chooses what to
/// show and in what order, and the app renders the bytes from git. It cannot
/// make up a diff line it would rather have.
pub struct Hunk {
    pub id: String,
    pub old_start: u32,
    pub new_start: u32,
    pub adds: usize,
    pub dels: usize,
    /// Unchanged lines collapsed right before this hunk, already lowered to the
    /// row that stands in for them. The gap's geometry is the lowering's
    /// business, and every view that places this hunk places the same row.
    pub gap_before: Option<Row>,
    /// First changed line, trimmed — gives the agent something to reason about
    /// without shipping it the whole file.
    pub preview: String,
    /// Context + deletions + insertions. Self-contained and renderable alone.
    pub rows: Vec<Row>,
}

/// A file's diff, resolved but not yet flattened — the intermediate an agent
/// reorders before we lower it to a row stream.
pub struct FileChange {
    pub id: String,
    pub path: String,
    /// A wholly new file. Its entire content is one hunk, which makes it very
    /// cheap coverage — see the concentration warning in `guide::lint`.
    pub is_new: bool,
    pub from: Option<String>,
    pub similarity: Option<u8>,
    pub lang: &'static str,
    pub adds: usize,
    pub dels: usize,
    pub hunks: Vec<Hunk>,
    /// Unchanged lines collapsed after the last hunk.
    pub gap_after: Option<Row>,
}

impl FileChange {
    /// The default, un-organized rendering: every hunk in file order, with the
    /// collapsed runs between them. This is what you get with no agent.
    pub fn default_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for h in &self.hunks {
            rows.extend(h.gap_before.clone());
            rows.extend(h.rows.iter().cloned());
        }
        rows.extend(self.gap_after.clone());
        rows
    }
}
