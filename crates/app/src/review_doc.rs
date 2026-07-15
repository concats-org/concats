//! The pane's review document: the row streams a dock tab renders, the caret
//! and selection over them, and the comment being written.
//!
//! [`ReviewDoc`] is the app's state, not the diff's. A diff has blobs and rows;
//! a pane has four streams of them, a caret, folded cards and a composer. The
//! content comes from [`concats_diff`]; how it is laid out is decided here.
//!
//! Nothing here touches a widget or `Cx`. The widgets in `main.rs` call these
//! functions, and they only touch document data and the process-wide
//! `docs`/`stores` state.

use std::collections::HashSet;

use concats_diff::{stage::StageFile, Blob, CollapsedEnd, LineKind, LoadStats, Row, Side};
use concats_review::store::{self, Comment};
use gix::ObjectId;

/// The text caret: a byte position on one line of one blob.
///
/// `byte` counts into that line's text, not into the blob. Those are the units
/// `DiffLine` answers hit tests in, so a click becomes a caret with no
/// conversion in between.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Caret {
    pub blob: u32,
    pub line: u32,
    pub byte: u32,
}

/// An in-progress comment selection. Two-sided so it can cross a hunk's
/// deleted→added boundary (GitHub's L→R ranges): deleted lines anchor to the
/// old blob (the `old` side), added and context lines to the new one.
#[derive(Clone, Copy)]
pub struct Compose {
    pub old: Option<Side>,
    pub new: Option<Side>,
}

/// What the open composer will post. One field, not two: a gutter drag started
/// while a reply is open replaces the target, and with two fields five places
/// would have to remember to clear the other one.
#[derive(Clone, Copy)]
pub enum Composing {
    /// A new comment on the selected lines.
    Lines(Compose),
    /// A reply into an existing thread, named by the thread's root id.
    Reply(u64),
}

/// One of the pane's document streams. Each dock tab renders one stream;
/// `ReviewDoc::tab` tracks the stream that owns the composer/current gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    /// The agent's guide: its layout of the diff.
    #[default]
    Guide,
    /// The classic file diff: path order, nothing but the changes.
    Files,
    /// Session transcripts with the commit(s) each turn produced.
    Sessions,
    /// The range organized by commit: message, then that commit's own diff.
    Commits,
    /// Every comment thread, each with a few lines of context around the code
    /// it is about — the tracking view: what was said, where it lives now, and
    /// whether anyone has answered.
    Comments,
    /// One open file, whole, at the range's head — picked in the file browser.
    /// You can comment on it like on a diff, because a comment anchors to a
    /// blob oid rather than to a hunk; the file need not have changed at all.
    ///
    /// There is one of these per open file, unlike the four fixed streams, so
    /// the variant says which: the raw id of the dock tab showing it. The tab
    /// is the identity. That is how the list inside it finds its own stream, by
    /// walking up the widget tree — the same way a terminal pane finds its
    /// session.
    ///
    /// A raw `u64` rather than makepad's `LiveId`, so the document stays a
    /// plain value. The widgets wrap it back where they need one.
    File(u64),
}

#[derive(Clone, Default)]
pub struct ReviewDoc {
    /// The Guide tab: the agent's guide.
    pub guide_rows: Vec<Row>,
    /// The Files tab: the same FileChanges in plain path order.
    pub files_rows: Vec<Row>,
    /// The Sessions tab: transcripts of the concats sessions linked to this
    /// range, interleaved with the per-commit diffs each turn produced. All
    /// streams index the same blob table, so lazy highlighting and review state
    /// (keyed by blob oid) work the same in each.
    pub sessions_rows: Vec<Row>,
    /// The Commits tab: the range organized by commit, oldest first.
    pub commits_rows: Vec<Row>,
    /// The Comments tab: one card per thread cluster, context included. Rebuilt
    /// by [`splice_comments`] whenever the comment list changes, so it never
    /// lags behind what the splice placed.
    pub comments_rows: Vec<Row>,
    /// Whether any comments are recorded for this repo — the Comments tab and
    /// its status-bar button exist only when there is something to track.
    pub has_comments: bool,
    /// The open File tabs, in the order they were opened — one per file, like
    /// an editor. Each renders over the same blob table as every other stream.
    pub files_open: Vec<FileView>,
    pub tab: Tab,
    /// Whether an agent's guide exists for this range. Per the design, the
    /// Guide tab is hidden otherwise.
    pub has_guide: bool,
    /// Whether any recorded sessions link to this range (the placeholder
    /// explainer does not count). Hidden tab otherwise.
    pub has_sessions: bool,
    /// Whether the range has more than one commit — with one, the Commits tab
    /// would just duplicate the File Diff, so it doesn't exist.
    pub has_commits: bool,
    /// What this pane has loaded, for the header chrome (dir name, base…head).
    pub repo: String,
    pub base: String,
    pub head: String,
    /// Candidate refs for the diff picker: branches, tags, then HEAD~N
    /// presets. Filled on load from the repo's refs.
    pub refs: Vec<String>,
    /// blob index -> repo path, for comment records and composer labels.
    pub blob_paths: std::collections::HashMap<u32, String>,
    /// Every blob path of the tree at the head, sorted — what the file browser
    /// lists. A changed path missing from here was deleted at the head; that is
    /// how the browser can dot a folder red without listing what went.
    pub tree: Vec<String>,
    /// The paths this range creates. Subset of `tree`; the browser dots these
    /// differently from the ones it merely edits.
    pub added: HashSet<String>,
    /// The repo's .git dir — the review store's identity.
    pub git_dir: Option<std::path::PathBuf>,
    /// Set on a WORKTREE load: the working directory. `Some` is what marks
    /// this pane's review as a worktree review (enables "stage seen hunks",
    /// drives the staleness poll).
    pub workdir: Option<std::path::PathBuf>,
    /// WORKTREE loads: the per-file payload "stage seen hunks" works from.
    pub stage: Vec<StageFile>,
    /// The loaded range's resolved endpoints — what a submitted guide must
    /// match to be picked up for this pane.
    pub merge_base_oid: Option<ObjectId>,
    pub head_oid: Option<ObjectId>,
    /// An explicit `--guide` from startup. While set, it wins over any
    /// submitted guide (local iteration beats the store); the diff picker
    /// clears it — picking a different range invalidates the guide.
    pub guide_path: Option<String>,
    /// `created_at` of the submitted guide this load applied, when one was.
    /// The poll compares against it, so a load that already used the newest
    /// guide is never re-triggered.
    pub applied_guide_at: Option<u64>,
    /// An in-progress comment: lines grown by dragging the gutter's + (or by
    /// further gutter clicks on the same file while the composer is open), or
    /// a reply into an existing thread.
    pub compose: Option<Composing>,
    /// The stream row the gesture started on — the drag's fixed end and the
    /// reference row for mapping drag distance to lines, or the comment row
    /// whose Reply was pressed.
    pub compose_anchor: usize,
    /// The composer's text, mirrored on every keystroke so the virtualized
    /// list can recreate the input without losing the draft.
    pub compose_draft: String,
    /// One-shot: focus the composer's input on its next draw.
    pub compose_focus: bool,
    /// Every changed line of this range, as review-state keys — the
    /// denominator of the status bar's progress. Built once by the loader,
    /// off the UI thread, because a tick box would otherwise rebuild it.
    pub changed_keys: HashSet<(ObjectId, u32)>,
    /// File cards folded shut by the caret in their header, by path. A card is
    /// the same file in every stream, so folding one folds it everywhere; the
    /// set survives a reload, like the seen ticks it sits next to.
    pub folded: HashSet<String>,
    /// File cards showing every comment recorded against them, by path —
    /// including the ones this range cannot place, which are otherwise
    /// invisible. Same scope and lifetime as `folded`.
    pub show_all_comments: HashSet<String>,
    /// Thread roots the last comment splice found a line for. A card header
    /// counts what is missing from this set instead of deciding for itself
    /// whether a thread is outdated. The splice already decided; two answers to
    /// the one question is how a header ends up offering a thread that is
    /// already on screen.
    pub placed_threads: HashSet<u64>,
    /// Where the text caret sits, in blob coordinates rather than row indices.
    /// A row index names a position in one stream's current shape, and every
    /// resplice (a comment landing, a collapsed run revealed, the composer
    /// opening) renumbers it. `(blob, line, byte)` survives all of that and
    /// means the same position in every stream that shows the line.
    pub caret: Option<Caret>,
    /// The fixed end of a selection, when there is one; `caret` is the moving
    /// end. `None` means the caret is a plain insertion point.
    ///
    /// The selection is document state, not the list widget's, because an edit
    /// has to know about it: replacing the selected text is the primitive every
    /// other editing operation builds on, and a range only the renderer knows
    /// about cannot be replaced. The widget still owns the pointer gesture and
    /// the painting; this is what the gesture resolves to.
    pub selection_anchor: Option<Caret>,
    pub blobs: Vec<Blob>,
    pub stats: LoadStats,
    pub generation: u64,
    /// Bumped whenever a row stream changes shape outside a load — today that
    /// is only the composer being spliced in or stripped out. A renderer that
    /// caches anything by row index (the review list's card boundaries, its
    /// fold mapping, the virtualized list's per-entry heights) is stale the
    /// moment a row lands mid-stream, and `generation` cannot say so: it marks
    /// a landed load, and the composer moves between loads.
    pub rows_rev: u64,
    pub error: Option<String>,
    pub loading: bool,
}

impl ReviewDoc {
    /// The row stream of one specific tab. The dock renders every stream in
    /// its own list, so rendering accesses by tab; `active()` remains for the
    /// composer/gesture path, which follows `self.tab`.
    pub fn stream(&self, tab: Tab) -> &[Row] {
        match tab {
            Tab::Guide => &self.guide_rows,
            Tab::Files => &self.files_rows,
            Tab::Sessions => &self.sessions_rows,
            Tab::Commits => &self.commits_rows,
            Tab::Comments => &self.comments_rows,
            // A tab the document no longer has a file for — closed, or
            // restored from a saved layout this range never filled — reads as
            // empty rather than as someone else's stream.
            Tab::File(tab) => self.file(tab).map_or(&[], |f| &f.rows),
        }
    }

    pub fn file(&self, tab: u64) -> Option<&FileView> {
        self.files_open.iter().find(|f| f.tab == tab)
    }

    /// `None` for a File tab with no file behind it — a stream that does not
    /// exist cannot be mutated, and silently mutating a substitute would put
    /// the edit somewhere the user is not looking.
    pub fn stream_mut(&mut self, tab: Tab) -> Option<&mut Vec<Row>> {
        match tab {
            Tab::Guide => Some(&mut self.guide_rows),
            Tab::Files => Some(&mut self.files_rows),
            Tab::Sessions => Some(&mut self.sessions_rows),
            Tab::Commits => Some(&mut self.commits_rows),
            Tab::Comments => Some(&mut self.comments_rows),
            Tab::File(tab) => self
                .files_open
                .iter_mut()
                .find(|f| f.tab == tab)
                .map(|f| &mut f.rows),
        }
    }

    /// Run `f` over every row stream the document has, with the blob table and
    /// the per-path comment reveals they are read against.
    ///
    /// The passes that must reach all streams — comment splicing, composer
    /// stripping — go through here, so a new stream cannot be forgotten by one
    /// of them. Passing the shared inputs in is what lets the streams be
    /// borrowed mutably at the same time.
    ///
    /// The fourth argument is the file a stream is about, which only a File tab
    /// has: its rows carry no `FileHeader` (the path lives in the dock tab), so
    /// it is the one stream whose path cannot be read off its own rows.
    pub fn for_each_stream(
        &mut self,
        mut f: impl FnMut(&mut Vec<Row>, &[Blob], &HashSet<String>, Option<&str>, Tab),
    ) {
        let Self {
            guide_rows,
            files_rows,
            sessions_rows,
            commits_rows,
            comments_rows,
            files_open,
            blobs,
            show_all_comments,
            ..
        } = self;
        for (stream, tab) in [
            (guide_rows, Tab::Guide),
            (files_rows, Tab::Files),
            (sessions_rows, Tab::Sessions),
            (commits_rows, Tab::Commits),
            (comments_rows, Tab::Comments),
        ] {
            f(stream, blobs, show_all_comments, None, tab);
        }
        for view in files_open.iter_mut() {
            f(
                &mut view.rows,
                blobs,
                show_all_comments,
                Some(&view.path),
                Tab::File(view.tab),
            );
        }
    }

    /// The row stream the gesture/composer state points at.
    pub fn active(&self) -> &[Row] {
        self.stream(self.tab)
    }

    pub fn active_mut(&mut self) -> Option<&mut Vec<Row>> {
        self.stream_mut(self.tab)
    }
}

/// One file open in its own tab: what it is, and its rows.
#[derive(Clone)]
pub struct FileView {
    /// Raw id of the dock tab showing it — the identity, see [`Tab::File`].
    pub tab: u64,
    pub path: String,
    pub rows: Vec<Row>,
    /// The two sides this view was lowered from, kept so typing into it can
    /// lower it again without going back to disk. The head side is the buffer
    /// being typed into; re-reading it from disk would throw the edit away.
    pub base: Option<u32>,
    pub head: u32,
    /// A line above the content, when it needs one. A repo file does not: it is
    /// an editor, and what the file is and which revision it is at belong in
    /// the tab title, not in a caption over the text. The settings keep one,
    /// because the themes you can pick have nowhere else to be listed.
    pub heading: Option<String>,
}

// ---------------------------------------------------------------------------
// Selection geometry — the pure core of the comment gesture. These read the
// row stream and the two-sided `Compose`; the imperative shell (main.rs) owns
// the locking, redraws, and store writes.
// ---------------------------------------------------------------------------

/// A caret as an absolute byte offset into its blob.
fn offset_of(d: &ReviewDoc, at: Caret) -> Option<usize> {
    let blob = d.blobs.get(at.blob as usize)?;
    Some(*blob.line_starts.get(at.line as usize)? as usize + at.byte as usize)
}

/// The selected byte range, as `(blob, from, to)` with `from < to`.
///
/// `None` when there is no selection, when it is empty, or when its two ends
/// sit on different blobs. A range spanning the old and new sides of a diff is
/// fine to read, but it cannot be replaced: the text between its ends exists in
/// neither blob.
pub fn selection(d: &ReviewDoc) -> Option<(u32, usize, usize)> {
    let (head, anchor) = (d.caret?, d.selection_anchor?);
    if head.blob != anchor.blob {
        return None;
    }
    let (at, from) = (offset_of(d, head)?, offset_of(d, anchor)?);
    (at != from).then(|| (head.blob, at.min(from), at.max(from)))
}

/// The part of the selection on one line, as offsets into that line.
///
/// This is what a row needs to paint: rows are drawn one line at a time and
/// know nothing about the range spanning them. A line wholly inside the range
/// reports its whole width, the first and last report a part, a line outside
/// reports nothing.
pub fn selection_on(d: &ReviewDoc, blob: u32, line: u32) -> Option<(usize, usize)> {
    let (selected, from, to) = selection(d)?;
    if selected != blob {
        return None;
    }
    let b = &d.blobs[blob as usize];
    let start = *b.line_starts.get(line as usize)? as usize;
    // The line's text without its newline: a row draws no more than that, and a
    // range running past it paints to the end and stops.
    let end = start + b.line_text(line as usize).len();
    (from <= end && to >= start).then(|| (from.max(start) - start, to.min(end) - start))
}

/// Put the caret at an absolute byte offset in `blob`, with nothing selected.
fn caret_to(d: &mut ReviewDoc, blob: u32, at: usize) {
    let b = &d.blobs[blob as usize];
    let line = b.line_of(at);
    d.caret = Some(Caret {
        blob,
        line: line as u32,
        byte: at.saturating_sub(b.line_starts[line] as usize) as u32,
    });
    d.selection_anchor = None;
}

/// Replace the selection with `insert`, leaving the caret after it.
///
/// The primitive: deleting a selection is replacing it with nothing, typing over
/// one is replacing it with a character, and pasting is replacing it with the
/// clipboard. `false` when there is nothing selected, so the caller falls
/// through to its own single-position behaviour.
pub fn replace_selection(d: &mut ReviewDoc, insert: &str) -> bool {
    let Some((blob, from, to)) = selection(d) else {
        return false;
    };
    if !d.blobs[blob as usize].editable() {
        return false;
    }
    d.blobs[blob as usize].edit(from..to, insert);
    caret_to(d, blob, from + insert.len());
    true
}

/// Apply one edit at the caret and leave the caret after what went in.
///
/// The one gate on editing: the blob has to name a file to write back to. A git
/// object does not (its bytes exist only in the object database), so a caret on
/// a deleted-side row, or anywhere in a commit range, silently takes no text.
/// `back` deletes that many bytes before the caret instead — that is backspace.
pub fn type_at(d: &mut ReviewDoc, insert: &str, back: usize) -> bool {
    // A selection is what the edit lands on: typing replaces it, and backspace
    // takes the whole of it rather than one byte off its end. `back` is the
    // single-position behaviour and does not apply when a range is selected.
    if replace_selection(d, insert) {
        return true;
    }
    let Some(caret) = d.caret else {
        return false;
    };
    let blob = &mut d.blobs[caret.blob as usize];
    if !blob.editable() {
        return false;
    }
    let start = blob.line_starts[caret.line as usize] as usize + caret.byte as usize;
    // Backspace at column 0 joins this line onto the one above, so the range
    // reaches back past a newline rather than stopping at the line's edge.
    let from = start.saturating_sub(back);
    if from == start && insert.is_empty() {
        return false;
    }
    blob.edit(from..start, insert);
    caret_to(d, caret.blob, from + insert.len());
    true
}

/// Which way a caret motion goes. The row stream is the only thing that knows
/// what line is "above" another — lines of two blobs interleave in a diff, and
/// prose sits between them — so motion walks rows, not line numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Up,
    Down,
}

/// The stream index of the code row a caret sits on.
pub fn caret_row(rows: &[Row], caret: Caret) -> Option<usize> {
    rows.iter().position(
        |r| matches!(r, Row::Code { blob, line, .. } if *blob == caret.blob && *line == caret.line),
    )
}

/// The code row one step up or down from `row`, skipping the chrome between
/// them (hunk bars, comments, card caps). Stops at the first row that is not
/// code — a caret does not jump from the end of one file into the next.
pub fn step_row(rows: &[Row], row: usize, step: Step) -> Option<usize> {
    let mut idx = row;
    loop {
        idx = match step {
            Step::Up => idx.checked_sub(1)?,
            Step::Down => idx + 1,
        };
        match rows.get(idx)? {
            Row::Code { .. } => return Some(idx),
            // Chrome a caret passes straight through: none of it is text.
            Row::HunkBar { .. } | Row::Comment { .. } | Row::Spacer => {}
            _ => return None,
        }
    }
}

/// Fold a row span into the two-sided compose selection: deleted rows grow the
/// old side, added and context rows the new side.
pub fn derive_compose(rows: &[Row], lo: usize, hi: usize) -> Option<Compose> {
    let mut c = Compose {
        old: None,
        new: None,
    };
    for r in &rows[lo..=hi.min(rows.len().saturating_sub(1))] {
        if let Row::Code {
            kind, blob, line, ..
        } = r
        {
            let side = match kind {
                LineKind::Del => &mut c.old,
                _ => &mut c.new,
            };
            match side {
                Some(s) if s.blob == *blob => {
                    s.start = s.start.min(*line);
                    s.end = s.end.max(*line);
                }
                None => {
                    *side = Some(Side {
                        blob: *blob,
                        start: *line,
                        end: *line,
                    })
                }
                _ => {}
            }
        }
    }
    if c.old.is_none() && c.new.is_none() {
        None
    } else {
        Some(c)
    }
}

/// The lines a selection comments on: the new side when there is one — the
/// comment renders below its last line — otherwise the old side. A selection
/// dragged across a hunk's deleted→added boundary anchors on the added lines:
/// one anchor per comment, like GitHub's `line`/`side`. `None` for an empty
/// selection.
pub fn comment_anchor(c: Compose) -> Option<Side> {
    c.new.or(c.old)
}

pub(crate) fn blob_label(d: &ReviewDoc, blob: u32) -> String {
    d.blob_paths
        .get(&blob)
        .cloned()
        .unwrap_or_else(|| format!("blob {}", &d.blobs[blob as usize].oid.to_string()[..10]))
}

/// The status bar's text — transient states only. The steady state is empty:
/// the load stats moved out of the chrome (`bench` prints them, and F3 still
/// overlays the live graph), and the review's one number is the progress bar
/// next to it.
pub(crate) fn status_line(d: &ReviewDoc) -> String {
    if let Some(e) = &d.error {
        return format!("error: {e}");
    }
    if d.loading {
        return "loading…".into();
    }
    if d.files_rows.is_empty() {
        return "click the repo name to open a repository".into();
    }
    String::new()
}

/// Every changed line of the range, as review-state keys. Taken from the File
/// Diff stream because it covers the range exactly once, so a line counts once
/// however many views render it. The keys are `(blob oid, line)`, the same
/// content the tick boxes write.
pub(crate) fn changed_keys(d: &ReviewDoc) -> std::collections::HashSet<store::LineKey> {
    d.files_rows
        .iter()
        .filter_map(|r| match r {
            Row::HunkBar { old, new } => Some(store::hunk_keys(*old, *new, &d.blobs)),
            _ => None,
        })
        .flatten()
        .collect()
}

/// How much of the range is ticked seen: `(seen, total)`. The denominator was
/// built by the loader, so this is one hash lookup per changed line.
pub(crate) fn seen_progress(d: &ReviewDoc, st: &crate::service::ReviewState) -> (usize, usize) {
    let seen = d
        .changed_keys
        .iter()
        .filter(|k| st.seen.contains(k))
        .count();
    (seen, d.changed_keys.len())
}

/// Hand every comment on an editable file to that file's text, as a cursor
/// pair.
///
/// A comment that carries cursors — minted when it was made, here or by the
/// CLI in the same document — is adopted as it is. One that does not is held by
/// the lines it names on the blob it names, once. Either way the document
/// carries it from then on: the run stretches and shrinks with the text, and
/// only deleting the text detaches it. Resolving a conversation stays a
/// decision, not an accident.
///
/// Only editable blobs: a git blob is immutable, so there is nothing for a
/// cursor to ride, and the line it names is exact.
///
/// Returns the pairs it minted for comments that had none — older ones — so
/// the caller can hand them to the store.
fn hold_comments(d: &mut ReviewDoc, comments: &[store::Comment]) -> Vec<(u64, store::Cursors)> {
    let mut minted = Vec::new();
    let editable: Vec<(u32, String)> = d
        .blobs
        .iter()
        .enumerate()
        .filter(|(_, b)| b.editable())
        .filter_map(|(i, _)| Some((i as u32, d.blob_paths.get(&(i as u32))?.clone())))
        .collect();
    for (index, path) in editable {
        let fresh: Vec<&store::Comment> = comments
            .iter()
            .filter(|c| c.path == path && !d.blobs[index as usize].holds(c.id))
            .collect();
        for c in fresh {
            let blob = &mut d.blobs[index as usize];
            let adopted = match &c.cursors {
                Some((from, to)) => blob.adopt(c.id, from, to),
                None => false,
            };
            if !adopted {
                if let Some((from, to)) = store::run_in(blob, c) {
                    blob.hold(c.id, from, to);
                    if c.cursors.is_none() {
                        minted.extend(blob.cursors_of(c.id).map(|pair| (c.id, pair)));
                    }
                }
            }
        }
    }
    minted
}

/// Splice stored comments below their anchor lines in every stream, then close
/// each file card. Both steps strip their previous output first, so this is
/// idempotent and reruns after every comment add/delete; an empty `comments`
/// slice just re-caps the cards (what `build_review` wants with no store yet).
///
/// Pure: returns the cursor pairs the buffers minted along the way, for
/// `load::resplice_comments` to store.
pub(crate) fn splice_comments(
    d: &mut ReviewDoc,
    comments: &[store::Comment],
) -> Vec<(u64, store::Cursors)> {
    let minted = hold_comments(d, comments);
    // The Comments tab is rebuilt here rather than patched: which threads
    // exist, where their content lives now and who answered last all change
    // with the comment list, and this is the one place that list arrives.
    let (comments_rows, outdated_paths) = comments_stream(&d.blobs, &d.blob_paths, comments);
    d.comments_rows = comments_rows;
    d.has_comments = !comments.is_empty();
    // Union over the streams: every stream reads the same blob table, so a
    // thread that places in one places in every stream that shows its file.
    let mut placed = std::collections::HashSet::new();
    d.for_each_stream(|rows, blobs, show_all, about, tab| {
        // The Comments tab always shows every thread — an outdated conversation
        // is what it exists to keep track of — so its cards reveal outdated
        // threads without asking the per-card toggles.
        let show = if tab == Tab::Comments {
            &outdated_paths
        } else {
            show_all
        };
        placed.extend(store::inject_comments(rows, blobs, comments, show, about));
        finalize_cards(rows);
    });
    d.placed_threads = placed;
    minted
}

/// Lines of context shown either side of a thread's code in the Comments tab —
/// the same margin a standard hunk keeps.
const COMMENT_CONTEXT: u32 = 3;

/// Where a thread's content sits right now, for the Comments tab: the blob
/// index and the inclusive line run. `members` is the thread newest-first, so a
/// reply written on the lines a fix moved to is what places it.
///
/// Mirrors the order of `store`'s placement (a live buffer holding the thread
/// wins, then the blob a comment names). Kept alongside rather than shared
/// because this needs the
/// run and the splice needs only the render line. If the two ever answer
/// differently, the splice is the authority: a thread this misplaces still
/// lands under whatever line the splice chose, inside these context rows.
fn thread_run(
    members: &[&store::Comment],
    blobs: &[Blob],
    path_blobs: &[u32],
) -> Option<(u32, u32, u32)> {
    for comment in members {
        let width = comment.anchor.end.saturating_sub(comment.anchor.start);
        for &i in path_blobs {
            let blob = &blobs[i as usize];
            if blob.holds(comment.id) {
                let last = blob.held_line(comment.id)?;
                return Some((i, last.saturating_sub(width), last));
            }
        }
    }
    members.iter().find_map(|comment| {
        let own = path_blobs
            .iter()
            .find(|&&i| blobs[i as usize].oid == comment.anchor.blob);
        own.into_iter()
            .chain(
                path_blobs
                    .iter()
                    .filter(|&&i| blobs[i as usize].oid != comment.anchor.blob),
            )
            .find_map(|&i| {
                let (from, to) = store::run_in(&blobs[i as usize], comment)?;
                Some((i, from, to))
            })
    })
}

/// The Comments tab's rows: every thread as a card of its code with
/// [`COMMENT_CONTEXT`] lines either side, overlapping threads sharing one card,
/// and the threads whose content is not in this range listed last per file.
/// Returns the rows and the paths whose outdated threads the injection must
/// flush.
///
/// Only the context rows are built here. The comment rows are spliced in by the
/// same `inject_comments` pass every stream gets, so this tab cannot disagree
/// with the diff about where a thread sits.
fn comments_stream(
    blobs: &[Blob],
    blob_paths: &std::collections::HashMap<u32, String>,
    comments: &[store::Comment],
) -> (Vec<Row>, HashSet<String>) {
    let mut rows = vec![Row::Title {
        text: "# Comments".into(),
    }];
    let mut outdated_paths: HashSet<String> = HashSet::new();
    if comments.is_empty() {
        rows.push(Row::Prose {
            md: "No comments recorded for this repo yet — click a line number to leave one, or `concats comments add` from the terminal."
                .into(),
        });
        return (rows, outdated_paths);
    }

    // path -> blob indices, ascending — the order the loader interned them.
    let mut of_path: std::collections::HashMap<&str, Vec<u32>> = std::collections::HashMap::new();
    for (blob, path) in blob_paths {
        of_path.entry(path.as_str()).or_default().push(*blob);
    }
    for list in of_path.values_mut() {
        list.sort_unstable();
    }

    // One meta line per thread: where it sits, and who has answered.
    let meta = |root: &store::Comment| -> String {
        let thread = store::thread_key(root);
        let replies: Vec<&store::Comment> = comments
            .iter()
            .filter(|c| c.parent == Some(thread))
            .collect();
        let who = root.author.as_deref().unwrap_or("unattributed");
        match replies.iter().max_by_key(|c| c.id) {
            None => format!("**unanswered** — {who}"),
            Some(last) => format!(
                "{} repl{}, last by **{}**",
                replies.len(),
                if replies.len() == 1 { "y" } else { "ies" },
                last.author.as_deref().unwrap_or("unattributed"),
            ),
        }
    };

    // Roots in reading order: by path, then by where the anchor was left.
    let mut roots: Vec<&store::Comment> = comments.iter().filter(|c| c.parent.is_none()).collect();
    roots.sort_by_key(|c| (c.path.clone(), c.anchor.start, c.id));

    let mut unanswered = 0usize;
    let mut outdated = 0usize;
    let mut cards: Vec<Row> = Vec::new();
    let mut at = 0usize;
    while at < roots.len() {
        let path = roots[at].path.as_str();
        let group_end = roots[at..]
            .iter()
            .position(|c| c.path != path)
            .map_or(roots.len(), |n| at + n);
        let group = &roots[at..group_end];
        at = group_end;

        let path_blobs = of_path.get(path).map(Vec::as_slice).unwrap_or(&[]);
        // Locate each thread, then merge threads whose context overlaps into
        // one card — the same line must not render in two cards, or the splice
        // would attach every thread at both occurrences.
        let mut located: Vec<(&store::Comment, u32, u32, u32)> = Vec::new();
        let mut unplaced: Vec<&store::Comment> = Vec::new();
        for root in group {
            // The thread newest-first: the root, then its replies, reversed.
            let mut members: Vec<&store::Comment> = comments
                .iter()
                .filter(|c| c.id == root.id || c.parent == Some(root.id))
                .collect();
            members.reverse();
            match thread_run(&members, blobs, path_blobs) {
                Some((blob, from, to)) => located.push((root, blob, from, to)),
                None => unplaced.push(root),
            }
        }
        located.sort_by_key(|(c, blob, from, _)| (*blob, *from, c.id));
        unanswered += group
            .iter()
            .filter(|c| !comments.iter().any(|r| r.parent == Some(c.id)))
            .count();

        let mut i = 0usize;
        while i < located.len() {
            let (_, blob, from, mut to) = located[i];
            let line_count = blobs[blob as usize].line_count() as u32;
            let ctx_from = from.saturating_sub(COMMENT_CONTEXT);
            let mut members = vec![located[i].0];
            let mut j = i + 1;
            while j < located.len() {
                let (c, b, f, t) = located[j];
                let joined = b == blob && f.saturating_sub(COMMENT_CONTEXT) <= to + COMMENT_CONTEXT;
                if !joined {
                    break;
                }
                to = to.max(t);
                members.push(c);
                j += 1;
            }
            i = j;
            let ctx_to = (to + COMMENT_CONTEXT).min(line_count.saturating_sub(1));
            let lines: Vec<String> = members
                .iter()
                .map(|c| {
                    format!(
                        "`{path}:{}–{}` · {}",
                        c.anchor.start + 1,
                        c.anchor.end + 1,
                        meta(c)
                    )
                })
                .collect();
            cards.push(Row::Prose {
                md: lines.join("  \n"),
            });
            cards.push(Row::FileHeader {
                path: path.to_string(),
                lang: "plain",
                adds: 0,
                dels: 0,
                from: None,
                similarity: None,
            });
            for line in ctx_from..=ctx_to {
                cards.push(Row::Code {
                    kind: LineKind::Context,
                    old_no: None,
                    new_no: Some(line + 1),
                    blob,
                    line,
                });
            }
        }

        if !unplaced.is_empty() {
            outdated += unplaced.len();
            outdated_paths.insert(path.to_string());
            let lines: Vec<String> = unplaced
                .iter()
                .map(|c| {
                    format!(
                        "`{path}:{}–{}` · {} · **content not in this range** — thread below",
                        c.anchor.start + 1,
                        c.anchor.end + 1,
                        meta(c)
                    )
                })
                .collect();
            cards.push(Row::Prose {
                md: lines.join("  \n"),
            });
            cards.push(Row::FileHeader {
                path: path.to_string(),
                lang: "plain",
                adds: 0,
                dels: 0,
                from: None,
                similarity: None,
            });
        }
    }

    rows.push(Row::Prose {
        md: format!(
            "**{} thread(s)** · {} unanswered · {} with content not in this range. Reply from any card; a reply with a new location moves its whole thread.",
            roots.len(),
            unanswered,
            outdated,
        ),
    });
    rows.extend(cards);
    (rows, outdated_paths)
}

/// Close every file card: bracket the code a `FileHeader` opened with 10pt of
/// air and insert a `CardEnd` after its last row. Idempotent — strips its own
/// previous output first — because comment injection reruns it after every
/// comment add/delete.
///
/// The air goes inside a collapsed run that opens or closes the card, per the
/// design: the run's band sits against the header (or the cap), and the padding
/// between it and the code.
pub(crate) fn finalize_cards(rows: &mut Vec<Row>) {
    rows.retain(|r| !matches!(r, Row::CardEnd | Row::Spacer));
    let mut out: Vec<Row> = Vec::with_capacity(rows.len() + 16);
    let mut in_card = false;
    // Set when a card opens, cleared by the first row that takes the padding:
    // a leading collapsed run goes above it, anything else below.
    let mut opening = false;
    for row in rows.drain(..) {
        let is_card_row = matches!(
            row,
            Row::Code { .. }
                | Row::HunkBar { .. }
                | Row::Collapsed { .. }
                | Row::Removed { .. }
                | Row::Comment { .. }
                | Row::Composer
        );
        if in_card && !is_card_row {
            close_card(&mut out, opening);
            in_card = false;
            opening = false;
        }
        if in_card && opening && !matches!(row, Row::Collapsed { .. }) {
            out.push(Row::Spacer);
            opening = false;
        }
        if matches!(row, Row::FileHeader { .. }) {
            in_card = true;
            opening = true;
        }
        out.push(row);
    }
    if in_card {
        close_card(&mut out, opening);
    }
    *rows = out;
}

/// Cap the card `out` ends with, padding it unless it never got any content
/// (`opening` still set — a 100%-similar rename has no code to give air to).
fn close_card(out: &mut Vec<Row>, opening: bool) {
    if !opening {
        // A trailing collapsed run keeps the cap company; the air goes above it.
        let at = match out.last() {
            Some(Row::Collapsed { .. }) => out.len() - 1,
            _ => out.len(),
        };
        out.insert(at, Row::Spacer);
    }
    out.push(Row::CardEnd);
}

/// The review-state keys of one file card: every changed line of every hunk
/// between this `FileHeader` and the card's end. This is what the header's
/// viewed tick box reads and toggles.
pub(crate) fn card_keys(rows: &[Row], header_idx: usize, blobs: &[Blob]) -> Vec<store::LineKey> {
    let mut keys = Vec::new();
    if !matches!(rows.get(header_idx), Some(Row::FileHeader { .. })) {
        return keys;
    }
    for row in &rows[header_idx + 1..] {
        match row {
            Row::HunkBar { old, new } => keys.extend(store::hunk_keys(*old, *new, blobs)),
            Row::Code { .. }
            | Row::Collapsed { .. }
            | Row::Removed { .. }
            | Row::Spacer
            | Row::Comment { .. }
            | Row::Composer => {}
            _ => break,
        }
    }
    keys
}

pub(crate) fn stream_has_composer(rows: &[Row]) -> bool {
    rows.iter().any(|r| matches!(r, Row::Composer))
}

/// Remove the composer row from every stream (at most one exists).
pub(crate) fn strip_composer(d: &mut ReviewDoc) {
    let mut removed = false;
    d.for_each_stream(|rows, _, _, _, _| {
        let before = rows.len();
        rows.retain(|r| !matches!(r, Row::Composer));
        removed |= rows.len() != before;
    });
    // Only when a row actually went: this is called speculatively on paths that
    // may have nothing to strip, and a revision that moves without the stream
    // moving costs every list a rebuild and a scroll re-anchor.
    if removed {
        d.rows_rev += 1;
    }
}

/// Splice the composer into the active stream, directly below the last line
/// of the compose range — the new side's end when one exists (it renders
/// last within a hunk), the old side's otherwise — and below any comments
/// already anchored there. When the same line renders in several places, the
/// occurrence nearest to where the drag started wins.
///
/// A reply needs no search: `compose_anchor` is the comment row that was
/// pressed, so the composer goes below the last row of that thread —
/// `inject_comments` emits a thread's rows contiguously.
pub(crate) fn splice_composer(d: &mut ReviewDoc) {
    strip_composer(d);
    let Some(c) = d.compose else {
        return;
    };
    let anchor = d.compose_anchor;
    let at = match c {
        Composing::Lines(c) => {
            let Some(side) = c.new.or(c.old) else {
                return;
            };
            let (blob, e) = (side.blob, side.end);
            let anchor = anchor as i64;
            let mut best: Option<usize> = None;
            for (i, r) in d.active().iter().enumerate() {
                if let Row::Code { blob: b, line, .. } = r {
                    if *b == blob && *line == e {
                        let closer = best.is_none_or(|prev| {
                            (i as i64 - anchor).abs() < (prev as i64 - anchor).abs()
                        });
                        if closer {
                            best = Some(i);
                        }
                    }
                }
            }
            let Some(mut at) = best else {
                d.compose = None;
                return;
            };
            while matches!(d.active().get(at + 1), Some(Row::Comment { .. })) {
                at += 1;
            }
            at
        }
        Composing::Reply(root) => {
            // A resplice between the click and here can have moved the rows
            // out from under the anchor. Dropping the composer beats putting
            // it somewhere the reviewer did not point at.
            if !matches!(d.active().get(anchor), Some(Row::Comment { .. })) {
                d.compose = None;
                return;
            }
            let mut at = anchor;
            while matches!(
                d.active().get(at + 1),
                Some(Row::Comment { id, parent, .. }) if parent.unwrap_or(*id) == root
            ) {
                at += 1;
            }
            at
        }
    };
    let Some(rows) = d.active_mut() else {
        return;
    };
    rows.insert(at + 1, Row::Composer);
    d.rows_rev += 1;
}

/// Lines revealed by one click on a collapsed run's chevron — GitHub's step. A
/// run shorter than two steps goes in one click instead, so expanding never
/// leaves a stub of three lines behind that needs a second click to clear.
const EXPAND_STEP: u32 = 20;

/// Reveal lines from one end of a collapsed run, in place: they become context
/// rows against the code they join, and the `Skipped` row keeps whatever stays
/// hidden — or goes, when the run is exhausted. Row indices below the run shift,
/// so this announces the new shape like the composer's splice does.
pub(crate) fn expand_collapsed(d: &mut ReviewDoc, tab: Tab, row: usize, end: CollapsedEnd) {
    let Some(&Row::Collapsed {
        blob,
        old_start,
        new_start,
        count,
    }) = d.stream(tab).get(row)
    else {
        return;
    };
    let take = if count < EXPAND_STEP * 2 {
        count
    } else {
        EXPAND_STEP
    };
    let hidden = count - take;
    // The head's lines come off the front of the run, the tail's off the back.
    let first = match end {
        CollapsedEnd::Head => 0,
        CollapsedEnd::Tail => hidden,
    };
    let revealed = (first..first + take).map(|k| Row::Code {
        kind: LineKind::Context,
        old_no: Some(old_start + k + 1),
        new_no: Some(new_start + k + 1),
        blob,
        line: new_start + k,
    });
    // What is left of the run, if anything: taking from the head moves its
    // start, taking from the tail only shortens it.
    let rest = (hidden > 0).then(|| match end {
        CollapsedEnd::Head => Row::Collapsed {
            blob,
            old_start: old_start + take,
            new_start: new_start + take,
            count: hidden,
        },
        CollapsedEnd::Tail => Row::Collapsed {
            blob,
            old_start,
            new_start,
            count: hidden,
        },
    });
    // Revealed lines sit on the side of the indicator they were taken from, so
    // the remaining run stays between the two blocks of code it separates.
    let mut out = Vec::with_capacity(take as usize + 1);
    match end {
        CollapsedEnd::Head => {
            out.extend(revealed);
            out.extend(rest);
        }
        CollapsedEnd::Tail => {
            out.extend(rest);
            out.extend(revealed);
        }
    }
    let grew = out.len() - 1;
    let Some(rows) = d.stream_mut(tab) else {
        return;
    };
    rows.splice(row..row + 1, out);
    // The composer's anchor is a row index into its stream, so rows inserted
    // above it make it name a different row now. The composer row itself rides
    // along with its neighbours; only the index needs correcting.
    if tab == d.tab && d.compose_anchor > row {
        d.compose_anchor += grew;
    }
    d.rows_rev += 1;
}

/// Reveal what a `Removed` marker stands for: its lines, as del rows, in its
/// place. One-way, like expanding a collapsed run — a reload puts the marker
/// back.
pub(crate) fn reveal_removed(d: &mut ReviewDoc, tab: Tab, row: usize) {
    let Some(&Row::Removed { blob, start, end }) = d.stream(tab).get(row) else {
        return;
    };
    let revealed: Vec<Row> = (start..=end)
        .map(|line| Row::Code {
            kind: LineKind::Del,
            old_no: Some(line + 1),
            new_no: None,
            blob,
            line,
        })
        .collect();
    let grew = revealed.len() - 1;
    let Some(rows) = d.stream_mut(tab) else {
        return;
    };
    rows.splice(row..row + 1, revealed);
    // The composer's anchor is a row index, so rows inserted above it make it
    // name a different row — the same correction `expand_collapsed` makes.
    if tab == d.tab && d.compose_anchor > row {
        d.compose_anchor += grew;
    }
    d.rows_rev += 1;
}

/// The composer's heading, GitHub-style: `L` marks old-side (deleted) lines,
/// `R`-less plain numbers the new side; a boundary-crossing range shows both
/// ends ("lines L63 to 68"). A reply names who it answers, which is the only
/// thing about a reply the reviewer cannot already see.
pub(crate) fn compose_title(d: &ReviewDoc, comments: &[Comment]) -> String {
    let Some(c) = d.compose else {
        return "Add a comment".into();
    };
    let c = match c {
        Composing::Lines(c) => c,
        Composing::Reply(root) => {
            return match comments
                .iter()
                .find(|c| c.id == root)
                .and_then(|c| c.author.as_deref())
            {
                Some(author) => format!("Reply to {author}"),
                None => "Reply".into(),
            };
        }
    };
    let Some(side) = c.new.or(c.old) else {
        return "Add a comment".into();
    };
    let path = blob_label(d, side.blob);
    match (c.old, c.new) {
        (Some(o), Some(n)) => {
            format!(
                "Add a comment on {path} lines L{}–{}",
                o.start + 1,
                n.end + 1
            )
        }
        (Some(o), None) if o.start == o.end => {
            format!("Add a comment on {path} line L{}", o.start + 1)
        }
        (Some(o), None) => {
            format!(
                "Add a comment on {path} lines L{}–L{}",
                o.start + 1,
                o.end + 1
            )
        }
        (None, Some(n)) if n.start == n.end => {
            format!("Add a comment on {path} line {}", n.start + 1)
        }
        (None, Some(n)) => {
            format!(
                "Add a comment on {path} lines {}–{}",
                n.start + 1,
                n.end + 1
            )
        }
        (None, None) => "Add a comment".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use concats_diff::{Blob, LineKind, Side};
    use gix::ObjectId;

    use super::*;

    fn oid(n: u8) -> ObjectId {
        ObjectId::from_hex(format!("{n:040x}").as_bytes()).expect("valid hex")
    }

    #[test]
    fn seen_progress_counts_each_changed_line_once() {
        let mut st = crate::service::ReviewState::default();
        // Two hunks over the same blob: three changed lines in total.
        let d = ReviewDoc {
            blobs: vec![Blob::new(oid(1), "rs".into(), "a\nb\nc\n".into())],
            files_rows: vec![
                Row::HunkBar {
                    old: None,
                    new: Some(Side {
                        blob: 0,
                        start: 0,
                        end: 1,
                    }),
                },
                Row::HunkBar {
                    old: None,
                    new: Some(Side {
                        blob: 0,
                        start: 2,
                        end: 2,
                    }),
                },
            ],
            ..Default::default()
        };
        let d = ReviewDoc {
            changed_keys: changed_keys(&d),
            ..d
        };
        assert_eq!(seen_progress(&d, &st), (0, 3));
        std::sync::Arc::make_mut(&mut st.seen).extend([(oid(1), 0), (oid(1), 2)]);
        assert_eq!(seen_progress(&d, &st), (2, 3));
        // Context lines are not part of the tally — only what the tick boxes
        // write — so a fully ticked range reads 100%.
        std::sync::Arc::make_mut(&mut st.seen).insert((oid(1), 1));
        assert_eq!(seen_progress(&d, &st), (3, 3));
    }

    /// A stream with one collapsed run of `count` lines between two code rows.
    /// The two sides start 10 apart, so a revealed line that read its numbering
    /// off the wrong side would be off by exactly that.
    fn doc_with_run(count: u32) -> ReviewDoc {
        let ctx = |line| Row::Code {
            kind: LineKind::Context,
            old_no: Some(line),
            new_no: Some(line + 10),
            blob: 0,
            line: line + 10,
        };
        ReviewDoc {
            files_rows: vec![
                ctx(10),
                Row::Collapsed {
                    blob: 0,
                    old_start: 10,
                    new_start: 20,
                    count,
                },
                ctx(200),
            ],
            tab: Tab::Files,
            ..Default::default()
        }
    }

    fn skipped_at(rows: &[Row], at: usize) -> (u32, u32, u32) {
        match rows[at] {
            Row::Collapsed {
                old_start,
                new_start,
                count,
                ..
            } => (old_start, new_start, count),
            _ => panic!("row {at} is not a collapsed run"),
        }
    }

    fn numbers_at(rows: &[Row], at: usize) -> (u32, u32, u32) {
        match rows[at] {
            Row::Code {
                kind: LineKind::Context,
                old_no: Some(old),
                new_no: Some(new),
                line,
                ..
            } => (old, new, line),
            _ => panic!("row {at} is not a context line"),
        }
    }

    /// The card's 10pt of air brackets its code, not its rows: a collapsed run
    /// that opens or closes the card sits outside the padding, against the
    /// header or the cap, the way the design draws it.
    #[test]
    fn card_air_goes_inside_a_run_that_brackets_the_card() {
        let run = || Row::Collapsed {
            blob: 0,
            old_start: 0,
            new_start: 0,
            count: 9,
        };
        let mut rows = vec![
            Row::FileHeader {
                path: "a.rs".into(),
                lang: "rust",
                adds: 1,
                dels: 0,
                from: None,
                similarity: None,
            },
            run(),
            Row::HunkBar {
                old: None,
                new: None,
            },
            Row::Code {
                kind: LineKind::Add,
                old_no: None,
                new_no: Some(1),
                blob: 0,
                line: 0,
            },
            run(),
        ];
        finalize_cards(&mut rows);
        let shape: Vec<&str> = rows
            .iter()
            .map(|r| match r {
                Row::FileHeader { .. } => "header",
                Row::Collapsed { .. } => "run",
                Row::Spacer => "air",
                Row::HunkBar { .. } => "hunk",
                Row::Code { .. } => "code",
                Row::CardEnd => "end",
                _ => "?",
            })
            .collect();
        assert_eq!(
            shape,
            ["header", "run", "air", "hunk", "code", "air", "run", "end"]
        );

        // Idempotent: comment injection reruns this after every add and delete.
        let once = rows.clone();
        finalize_cards(&mut rows);
        assert_eq!(rows.len(), once.len());
    }

    /// A 100%-similar rename has no code in its card, so there is nothing to
    /// give air to.
    #[test]
    fn a_card_without_code_gets_no_air() {
        let mut rows = vec![Row::FileHeader {
            path: "a.rs".into(),
            lang: "rust",
            adds: 0,
            dels: 0,
            from: Some("b.rs".into()),
            similarity: Some(100),
        }];
        finalize_cards(&mut rows);
        assert!(!rows.iter().any(|r| matches!(r, Row::Spacer)));
        assert!(matches!(rows[1], Row::CardEnd));
    }

    /// The head of a run joins the code above it, so its first hidden lines are
    /// the ones revealed and what stays hidden starts further down the file.
    #[test]
    fn expanding_the_head_reveals_the_first_hidden_lines() {
        let mut d = doc_with_run(50);
        let at_rest = d.rows_rev;
        expand_collapsed(&mut d, Tab::Files, 1, CollapsedEnd::Head);
        assert!(
            d.rows_rev > at_rest,
            "inserting rows shifts every index after"
        );

        // 20 lines, from the top of the run, numbered on both sides.
        assert_eq!(numbers_at(&d.files_rows, 1), (11, 21, 20));
        assert_eq!(numbers_at(&d.files_rows, 20), (30, 40, 39));
        // …and the run keeps the rest, starting where the revealed lines stopped.
        assert_eq!(skipped_at(&d.files_rows, 21), (30, 40, 30));

        // A remainder under two steps goes in one click, and the run with it.
        expand_collapsed(&mut d, Tab::Files, 21, CollapsedEnd::Head);
        assert!(!d
            .files_rows
            .iter()
            .any(|r| matches!(r, Row::Collapsed { .. })));
        assert_eq!(numbers_at(&d.files_rows, 21), (31, 41, 40));
        assert_eq!(numbers_at(&d.files_rows, 50), (60, 70, 69));
    }

    /// The tail leads into the code below, so the last hidden lines are the
    /// ones revealed — and they land after the run, which keeps its head.
    #[test]
    fn expanding_the_tail_reveals_the_last_hidden_lines() {
        let mut d = doc_with_run(50);
        expand_collapsed(&mut d, Tab::Files, 1, CollapsedEnd::Tail);

        // The run stays put, shortened…
        assert_eq!(skipped_at(&d.files_rows, 1), (10, 20, 30));
        // …and the revealed lines are the 20 that abut the code below.
        assert_eq!(numbers_at(&d.files_rows, 2), (41, 51, 50));
        assert_eq!(numbers_at(&d.files_rows, 21), (60, 70, 69));
        assert_eq!(numbers_at(&d.files_rows, 22), (200, 210, 210));
    }

    /// The composer inserts a row mid-stream and shifts every row index after
    /// it. Renderers cache by row index (card boundaries, fold mapping, the
    /// virtualized list's heights), so the shape change has to be announced, or
    /// those caches quietly describe the wrong rows. That is what once stopped
    /// every file header below an open composer from sticking.
    #[test]
    fn composer_splice_announces_the_shape_change() {
        let code = |line| Row::Code {
            kind: LineKind::Add,
            old_no: None,
            new_no: Some(line),
            blob: 0,
            line,
        };
        let mut d = ReviewDoc {
            files_rows: vec![code(0), code(1)],
            tab: Tab::Files,
            compose: Some(Composing::Lines(Compose {
                old: None,
                new: Some(Side {
                    blob: 0,
                    start: 0,
                    end: 0,
                }),
            })),
            ..Default::default()
        };

        let at_rest = d.rows_rev;
        splice_composer(&mut d);
        assert!(stream_has_composer(&d.files_rows));
        assert!(
            d.rows_rev > at_rest,
            "splicing must announce the new stream shape"
        );

        // …and the row really did move: the second code row is now third.
        assert!(matches!(d.files_rows[1], Row::Composer));
        assert!(matches!(d.files_rows[2], Row::Code { line: 1, .. }));

        let spliced = d.rows_rev;
        strip_composer(&mut d);
        assert!(!stream_has_composer(&d.files_rows));
        assert!(d.rows_rev > spliced, "stripping shifts the rows back");

        // A strip with nothing to strip must not bump it: a revision that moves
        // on its own costs every list a rebuild and a scroll re-anchor.
        let stripped = d.rows_rev;
        strip_composer(&mut d);
        assert_eq!(d.rows_rev, stripped);
    }

    fn blob(text: &str) -> Blob {
        Blob::new(oid(1), "rs".into(), text.into())
    }

    fn doc_with(text: &str, editable: bool) -> ReviewDoc {
        let mut b = blob(text);
        if editable {
            b.origin = Some("/tmp/a.rs".into());
        }
        ReviewDoc {
            blobs: vec![b],
            caret: Some(Caret {
                blob: 0,
                line: 1,
                byte: 1,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn typing_lands_at_the_caret_and_carries_it_along() {
        let mut d = doc_with("ab\ncd\n", true);
        assert!(type_at(&mut d, "X", 0));
        assert_eq!(d.blobs[0].text, "ab\ncXd\n");
        assert_eq!(
            d.caret,
            Some(Caret {
                blob: 0,
                line: 1,
                byte: 2
            })
        );
    }

    #[test]
    fn backspace_at_the_start_of_a_line_joins_it_to_the_one_above() {
        let mut d = doc_with("ab\ncd\n", true);
        d.caret = Some(Caret {
            blob: 0,
            line: 1,
            byte: 0,
        });
        assert!(type_at(&mut d, "", 1));
        assert_eq!(d.blobs[0].text, "abcd\n");
        assert_eq!(
            d.caret,
            Some(Caret {
                blob: 0,
                line: 0,
                byte: 2
            })
        );
    }

    #[test]
    fn a_newline_puts_the_caret_at_the_start_of_the_line_it_made() {
        let mut d = doc_with("ab\ncd\n", true);
        assert!(type_at(&mut d, "\n", 0));
        assert_eq!(d.blobs[0].text, "ab\nc\nd\n");
        assert_eq!(
            d.caret,
            Some(Caret {
                blob: 0,
                line: 2,
                byte: 0
            })
        );
    }

    /// The one gate on editing: a blob with no file behind it is read-only.
    /// That is what makes a commit range and every deleted-side row immutable
    /// with no check at the call sites.
    #[test]
    fn a_blob_with_no_file_behind_it_takes_no_text() {
        let mut d = doc_with("ab\ncd\n", false);
        assert!(!type_at(&mut d, "X", 0));
        assert_eq!(d.blobs[0].text, "ab\ncd\n");
        assert!(!d.blobs[0].dirty());
    }

    #[test]
    fn caret_motion_walks_code_rows_and_steps_over_the_chrome_between_them() {
        let rows = vec![
            Row::FileHeader {
                path: "a.rs".into(),
                lang: "rust",
                adds: 0,
                dels: 0,
                from: None,
                similarity: None,
            },
            code(LineKind::Context, 0, 0),
            Row::Comment {
                id: 1,
                parent: None,
                body: String::new(),
                meta: String::new(),
            },
            Row::HunkBar {
                old: None,
                new: None,
            },
            code(LineKind::Add, 0, 1),
            Row::CardEnd,
        ];
        assert_eq!(
            step_row(&rows, 1, Step::Down),
            Some(4),
            "over comment + bar"
        );
        assert_eq!(step_row(&rows, 4, Step::Up), Some(1));
        // A caret does not walk out of its file into the chrome of the next.
        assert_eq!(step_row(&rows, 4, Step::Down), None);
        assert_eq!(step_row(&rows, 1, Step::Up), None);

        let caret = Caret {
            blob: 0,
            line: 1,
            byte: 0,
        };
        assert_eq!(caret_row(&rows, caret), Some(4));
    }

    /// A buffer with a caret at `head` and, when given, an anchor at `tail` —
    /// both as `(line, byte)` on the one editable blob.
    fn with_selection(text: &str, head: (u32, u32), tail: Option<(u32, u32)>) -> ReviewDoc {
        let mut blob = blob(text);
        blob.origin = Some(std::path::PathBuf::from("/repo/a.rs"));
        let at = |(line, byte): (u32, u32)| Caret {
            blob: 0,
            line,
            byte,
        };
        ReviewDoc {
            blobs: vec![blob],
            caret: Some(at(head)),
            selection_anchor: tail.map(at),
            ..Default::default()
        }
    }

    #[test]
    fn a_selection_is_the_byte_range_between_its_two_ends() {
        // "b" on the second line, both ways round: which end moved is not what
        // decides the range.
        let forward = with_selection("aa\nbb\ncc\n", (1, 2), Some((1, 0)));
        assert_eq!(selection(&forward), Some((0, 3, 5)));
        let backward = with_selection("aa\nbb\ncc\n", (1, 0), Some((1, 2)));
        assert_eq!(selection(&backward), Some((0, 3, 5)));
        // Across lines.
        let across = with_selection("aa\nbb\ncc\n", (2, 1), Some((0, 1)));
        assert_eq!(selection(&across), Some((0, 1, 7)));
        // No anchor, or an empty range, is not a selection.
        assert_eq!(selection(&with_selection("aa\n", (0, 1), None)), None);
        assert_eq!(
            selection(&with_selection("aa\n", (0, 1), Some((0, 1)))),
            None
        );
    }

    /// The thing that could not be done before: select text and delete it.
    #[test]
    fn typing_over_a_selection_replaces_the_whole_of_it() {
        // Bytes 4..8 of `let x = 1;` are `x = `, so this replaces the name and
        // the assignment with one word.
        let mut d = with_selection("let x = 1;\nlet y = 2;\n", (0, 8), Some((0, 4)));
        assert!(type_at(&mut d, "value", 0));
        assert_eq!(d.blobs[0].text, "let value1;\nlet y = 2;\n");
        assert_eq!(
            d.caret,
            Some(Caret {
                blob: 0,
                line: 0,
                byte: 9
            }),
            "and leaves the caret after what went in"
        );
        assert_eq!(d.selection_anchor, None, "the selection is consumed");
    }

    #[test]
    fn backspace_over_a_selection_takes_the_selection_and_not_one_byte() {
        let mut d = with_selection("let x = 1;\nlet y = 2;\n", (1, 10), Some((0, 0)));
        // `back` is the single-position behaviour and must not apply here.
        assert!(type_at(&mut d, "", 1));
        assert_eq!(d.blobs[0].text, "\n");
        assert_eq!(
            d.caret,
            Some(Caret {
                blob: 0,
                line: 0,
                byte: 0
            })
        );
    }

    #[test]
    fn a_selection_spanning_two_blobs_is_readable_but_not_replaceable() {
        // The old and new sides of a hunk: the text between the ends does not
        // exist in either blob, so there is no range to replace.
        let mut d = with_selection("aa\nbb\n", (0, 0), None);
        d.blobs.push(blob("cc\n"));
        d.selection_anchor = Some(Caret {
            blob: 1,
            line: 0,
            byte: 2,
        });
        assert_eq!(selection(&d), None);
        assert!(!replace_selection(&mut d, "x"), "nothing to replace");
    }

    #[test]
    fn a_read_only_blob_refuses_to_have_its_selection_replaced() {
        let mut d = with_selection("aa\nbb\n", (0, 2), Some((0, 0)));
        d.blobs[0].origin = None;
        assert!(!replace_selection(&mut d, "x"));
        assert_eq!(d.blobs[0].text, "aa\nbb\n");
    }

    fn code(kind: LineKind, blob: u32, line: u32) -> Row {
        Row::Code {
            kind,
            old_no: None,
            new_no: None,
            blob,
            line,
        }
    }

    #[test]
    fn derive_compose_folds_del_to_old_and_add_context_to_new() {
        let rows = vec![
            code(LineKind::Del, 0, 5),
            code(LineKind::Add, 1, 10),
            code(LineKind::Context, 1, 11),
        ];
        let c = derive_compose(&rows, 0, 2).unwrap();
        assert_eq!(
            c.old,
            Some(Side {
                blob: 0,
                start: 5,
                end: 5
            })
        );
        assert_eq!(
            c.new,
            Some(Side {
                blob: 1,
                start: 10,
                end: 11
            })
        );
    }

    #[test]
    fn derive_compose_is_none_without_code_rows() {
        let skipped = Row::Collapsed {
            blob: 0,
            old_start: 0,
            new_start: 0,
            count: 3,
        };
        assert!(derive_compose(&[skipped], 0, 0).is_none());
    }

    #[test]
    fn comment_anchor_prefers_the_new_side() {
        let a = Side {
            blob: 0,
            start: 1,
            end: 2,
        };
        let b = Side {
            blob: 1,
            start: 3,
            end: 4,
        };
        assert_eq!(
            comment_anchor(Compose {
                old: Some(a),
                new: Some(b)
            }),
            Some(b)
        );
        assert_eq!(
            comment_anchor(Compose {
                old: Some(a),
                new: None
            }),
            Some(a)
        );
        assert_eq!(
            comment_anchor(Compose {
                old: None,
                new: None
            }),
            None
        );
    }

    /// One thread on `a.rs`, so the file's card can be checked end to end.
    fn stored(id: u64, path: &str, blob: ObjectId, start: u32, end: u32) -> Comment {
        Comment {
            id,
            path: path.into(),
            anchor: store::Anchor { blob, start, end },
            body: "b".into(),
            author: Some("ada".into()),
            created_at: 0,
            parent: None,
            external: None,
            cursors: None,
        }
    }

    #[test]
    fn comments_tab_frames_a_thread_in_the_context_a_hunk_gets() {
        let text: String = (0..20).map(|n| format!("line {n}\n")).collect();
        let blobs = vec![Blob::new(oid(1), "rs".into(), text)];
        let blob_paths = HashMap::from([(0u32, "a.rs".to_string())]);
        let comments = vec![stored(1, "a.rs", oid(1), 9, 10)];
        let (rows, outdated) = comments_stream(&blobs, &blob_paths, &comments);
        assert!(outdated.is_empty());
        assert!(matches!(&rows[0], Row::Title { .. }));
        let Row::Prose { md } = &rows[1] else {
            panic!("row 1 is not the summary");
        };
        assert!(md.contains("**1 thread(s)** · 1 unanswered · 0 with content"));
        let Row::Prose { md } = &rows[2] else {
            panic!("row 2 is not the thread meta");
        };
        assert!(md.contains("`a.rs:10–11`"), "meta was: {md}");
        assert!(md.contains("**unanswered** — ada"));
        assert!(matches!(&rows[3], Row::FileHeader { path, .. } if path == "a.rs"));
        // COMMENT_CONTEXT lines either side of the 9..=10 anchor: 6..=13.
        let code: Vec<(u32, Option<u32>)> = rows[4..]
            .iter()
            .map(|r| match r {
                Row::Code {
                    kind: LineKind::Context,
                    old_no: None,
                    blob: 0,
                    line,
                    new_no,
                } => (*line, *new_no),
                _ => panic!("expected only context lines after the header"),
            })
            .collect();
        let expect: Vec<(u32, Option<u32>)> = (6..=13).map(|n| (n, Some(n + 1))).collect();
        assert_eq!(code, expect);
    }

    #[test]
    fn threads_with_overlapping_context_share_a_card() {
        let text: String = (0..20).map(|n| format!("line {n}\n")).collect();
        let blobs = vec![Blob::new(oid(1), "rs".into(), text)];
        let blob_paths = HashMap::from([(0u32, "a.rs".to_string())]);
        let reply = Comment {
            id: 3,
            parent: Some(2),
            author: Some("bob".into()),
            ..stored(3, "a.rs", oid(1), 8, 8)
        };
        let comments = vec![
            stored(1, "a.rs", oid(1), 5, 5),
            stored(2, "a.rs", oid(1), 8, 8),
            reply,
        ];
        let (rows, _) = comments_stream(&blobs, &blob_paths, &comments);
        let headers = rows
            .iter()
            .filter(|r| matches!(r, Row::FileHeader { .. }))
            .count();
        assert_eq!(headers, 1, "overlapping threads must not split into cards");
        let Row::Prose { md } = &rows[2] else {
            panic!("row 2 is not the thread meta");
        };
        assert!(md.contains("`a.rs:6–6` · **unanswered** — ada"));
        assert!(md.contains("`a.rs:9–9` · 1 reply, last by **bob**"));
        // The merged card runs from 5-3 to 8+3, once.
        let lines: Vec<u32> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Code { line, .. } => Some(*line),
                _ => None,
            })
            .collect();
        let expect: Vec<u32> = (2..=11).collect();
        assert_eq!(lines, expect);
    }

    #[test]
    fn a_thread_left_behind_by_this_range_is_listed_for_the_flush() {
        // The anchor names a blob this range does not carry, and no blob of
        // the path exists to re-find it in: the thread is content-outdated.
        let comments = vec![stored(1, "gone.rs", oid(9), 4, 4)];
        let (rows, outdated) = comments_stream(&[], &HashMap::new(), &comments);
        assert_eq!(outdated, HashSet::from(["gone.rs".to_string()]));
        let Row::Prose { md } = &rows[1] else {
            panic!("row 1 is not the summary");
        };
        assert!(md.contains("1 with content not in this range"));
        assert!(matches!(&rows[2], Row::Prose { md } if md.contains("content not in this range")));
        // A bare card and no code: the injection flushes the thread under it.
        assert!(matches!(&rows[3], Row::FileHeader { path, .. } if path == "gone.rs"));
        assert!(!rows.iter().any(|r| matches!(r, Row::Code { .. })));
    }

    /// A comment written before cursors existed is held from its exact lines,
    /// and the splice hands back the pair it minted so the store can keep it.
    /// Another process — a fresh document over the same bytes — adopts that
    /// pair instead of minting again.
    #[test]
    fn the_splice_mints_cursors_for_a_comment_that_has_none() {
        let text: String = (0..20).map(|n| format!("line {n}\n")).collect();
        let editable = || {
            let mut b = Blob::new(oid(1), "rs".into(), text.clone());
            b.origin = Some("/tmp/a.rs".into());
            ReviewDoc {
                blobs: vec![b],
                blob_paths: HashMap::from([(0u32, "a.rs".to_string())]),
                ..Default::default()
            }
        };
        let mut d = editable();
        let bare = vec![stored(1, "a.rs", oid(1), 9, 10)];
        let minted = splice_comments(&mut d, &bare);
        assert_eq!(minted.len(), 1);
        assert_eq!(minted[0].0, 1);
        assert!(d.blobs[0].holds(1));
        assert!(splice_comments(&mut d, &bare).is_empty(), "held already");

        let mut elsewhere = editable();
        let carried = vec![Comment {
            cursors: Some(minted[0].1.clone()),
            ..stored(1, "a.rs", oid(1), 9, 10)
        }];
        assert!(
            splice_comments(&mut elsewhere, &carried).is_empty(),
            "adopted"
        );
        assert!(elsewhere.blobs[0].holds(1));
    }
}
