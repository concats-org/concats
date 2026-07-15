//! A file's content at one revision, and the buffer it becomes when typed into.
//!
//! A blob is held once and referenced by many rows. The first version had
//! `Row::Code { text: String, spans: Vec<Span> }`; on an 850-file diff (502k
//! rows, 1.1M lines) that hit 2.0 GB RSS, because every line's text and span
//! vector was cloned out of the blob into a row. A code row is now a reference,
//! `(blob, line)`, 16 bytes; the text, the line table and the spans live here,
//! once.
//!
//! Everything that changes the text goes through the CRDT document
//! ([`concats_sync`]) first and the rendered `String` second. So a concurrent
//! writer — an agent, the terminal, `git checkout` — merges instead of
//! overwriting, and every cursor into the text rides the edit.

use std::{collections::HashMap, ops::Range, path::PathBuf};

use concats_sync as document;
use concats_syntax::{LineSpans, Span};
use concats_text::{line_of, line_starts};
use gix::ObjectId;
use loro::{Frontiers, LoroDoc, cursor::Cursor};

/// One applied edit, kept so it can be undone: `inserted` went in between
/// `from` and `to` and replaced `removed`.
///
/// The two ends are cursors, not byte offsets. An undo entry lives on after the
/// state it was recorded in: an agent writes the file, or another instance's
/// edit arrives. A byte offset recorded before that would name different text
/// afterwards, and undo would eat a line it never wrote. Cursors follow those
/// edits.
#[derive(Clone)]
pub struct Edit {
    from: Cursor,
    to: Cursor,
    pub removed: String,
    pub inserted: String,
}

/// A file's content at one revision, held once and referenced by many rows.
pub struct Blob {
    pub oid: ObjectId,
    pub ext: String,
    pub text: String,
    /// Byte offset of each line start; `len() == line_count + 1`.
    pub line_starts: Vec<u32>,
    /// Filled lazily off the UI thread after a line from this blob is visible.
    pub spans: Option<LineSpans>,
    /// The `rev` the spans were computed against. An edit leaves the spans in
    /// place — blanking them would flash the whole file grey between keystrokes
    /// — and moves `rev` past this, which marks them stale.
    pub spans_rev: u64,
    /// Where an edit would be written. `None` is a git object — read-only,
    /// because the only place its bytes exist is the object database.
    pub origin: Option<PathBuf>,
    /// Bumped by every edit. Highlighting is requested per blob and memoized,
    /// keyed by generation; an edit must not bump that, or a landed-load path
    /// would fire mid-typing. So edits get their own counter.
    pub edit_rev: u64,
    pub undo: Vec<Edit>,
    pub redo: Vec<Edit>,
    /// The CRDT document behind this text, once there is one.
    ///
    /// Built lazily, on the first edit or when a comment needs a position that
    /// survives one, so an 850-file diff builds none at all. `text` is this
    /// document checked out at its current version: the renderer keeps reading
    /// a plain `String`, and every mutation goes through the document first so
    /// concurrent writers merge instead of overwriting.
    ///
    /// Cloning a `Blob` forks the document (see `Clone` below): a buffer
    /// carried across a reload keeps its history and its cursors, and stays
    /// independent.
    pub doc: Option<LoroDoc>,
    /// The version whose bytes are on disk — what an external write imports
    /// onto, and what a save moves forward. Everything between it and the
    /// document's current version is unsaved local typing.
    pub disk: Frontiers,
    /// The stretch of text each comment is about, as a cursor pair, by comment
    /// id.
    ///
    /// This is what lets a conversation survive its line being edited, not only
    /// moved: the run grows and shrinks with the text and only goes away when
    /// the text does. A comment on a worktree file is minted one of these when
    /// it is made — here, or by the CLI in the same document — and the pair
    /// travels with the comment; a comment without one is held by the lines it
    /// names on the blob it names. Resolving a thread stays a decision, not an
    /// accident.
    ///
    /// A range rather than a point, and the range includes the trailing
    /// newline. A single position would detach as soon as the character it
    /// named was deleted, so editing the front of a line would behave
    /// differently from editing its middle.
    pub held: HashMap<u64, (Cursor, Cursor)>,
    /// Whether the next insert may coalesce into the last undo entry.
    group_open: bool,
}

impl Clone for Blob {
    /// A cloned blob is an independent buffer, so its document is forked rather
    /// than shared.
    ///
    /// Written out because the derive would be wrong: Loro's own `Clone` is a
    /// reference clone, so two blobs would alias one document while keeping
    /// separate `text`, and the first edit through either would put that blob's
    /// text and document out of step. Everything else here rests on those two
    /// staying in step.
    fn clone(&self) -> Self {
        Self {
            oid: self.oid,
            ext: self.ext.clone(),
            text: self.text.clone(),
            line_starts: self.line_starts.clone(),
            spans: self.spans.clone(),
            spans_rev: self.spans_rev,
            origin: self.origin.clone(),
            edit_rev: self.edit_rev,
            undo: self.undo.clone(),
            redo: self.redo.clone(),
            doc: self.doc.as_ref().map(LoroDoc::fork),
            disk: self.disk.clone(),
            // A fork keeps the operation ids of everything already in the
            // history, so cursors minted against the original still resolve.
            held: self.held.clone(),
            group_open: self.group_open,
        }
    }
}

impl Blob {
    pub fn new(oid: ObjectId, ext: String, text: String) -> Self {
        let line_starts = line_starts(&text);
        Self {
            oid,
            ext,
            text,
            line_starts,
            spans: None,
            spans_rev: 0,
            origin: None,
            edit_rev: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            doc: None,
            disk: Frontiers::default(),
            held: HashMap::new(),
            group_open: false,
        }
    }

    /// Whether there is typing here that the file on disk does not have.
    ///
    /// Derived rather than flagged: everything past the disk version is unsaved
    /// by definition, so this cannot fall out of step with the text.
    pub fn dirty(&self) -> bool {
        self.doc
            .as_ref()
            .is_some_and(|doc| doc.oplog_frontiers() != self.disk)
    }

    /// Whether the colours on screen were computed for text that has since
    /// been typed over.
    pub fn spans_stale(&self) -> bool {
        self.spans.is_none() || self.spans_rev != self.edit_rev
    }

    /// Where a line of the text as last read from disk sits now. `None` once
    /// an edit swallowed it — the caller renders that thread as outdated.
    pub fn anchor_line(&self, line: u32) -> Option<u32> {
        // No document, so nothing has been typed: the live line table is the
        // one the anchor was written against.
        let Some(doc) = &self.doc else {
            return ((line as usize) < self.line_count()).then_some(line);
        };
        // A cursor placed in the disk version and read in the current one. The
        // line has to be addressed in the text it was numbered against; the
        // fork gives us that text.
        let old = doc.fork_at(&self.disk).ok()?;
        let from = *line_starts(&document::text(&old)).get(line as usize)? as usize;
        let cursor = document::cursor_at(&old, from)?;
        // `held` is what we are after: a cursor whose line was typed away
        // slides onto the neighbour rather than failing, and a thread about
        // deleted code must not land on whatever ended up next to it. A line
        // edited in place still holds — its first character is still there —
        // and that tells a prefixed line from a deleted one.
        let (byte, held) = document::resolve(doc, &cursor)?;
        held.then(|| self.line_of(byte) as u32)
    }

    /// The text is now what is on disk, under `oid`. Moves the disk version up
    /// to what is in the buffer, so nothing reads as unsaved any more and
    /// `intern` stops keeping this buffer apart from the diff's copy of the
    /// same file.
    pub fn saved(&mut self, oid: ObjectId) {
        self.oid = oid;
        if let Some(doc) = &self.doc {
            self.disk = doc.oplog_frontiers();
        }
        self.group_open = false;
    }

    /// Merge what is now on disk into this buffer.
    ///
    /// This is what the design is for: an agent, the terminal or a `git
    /// checkout` writing the file does not replace what is being typed here,
    /// and does not wait for a save to win. Both edits land, and every cursor —
    /// the caret, a selection, a comment's anchor — keeps its place across the
    /// merge.
    pub fn merge_disk(&mut self, bytes: &str, oid: ObjectId) {
        // `oid` is the disk state; `self.oid` is the last one we took in. Equal
        // means nothing was written since. A worktree review re-reads about
        // once a second, so this is the common case and must not cost a
        // re-highlight of the file.
        if oid == self.oid {
            return;
        }
        let doc = self
            .doc
            .get_or_insert_with(|| document::open(&self.text).0)
            .clone();
        self.disk = document::import(&doc, &self.disk, bytes);
        self.oid = oid;
        // A merge can touch anywhere, so the incremental splice that keeps
        // typing cheap does not apply: take the merged text whole and reparse.
        self.text = document::text(&doc);
        self.line_starts = line_starts(&self.text);
        self.spans = None;
        self.edit_rev += 1;
        // Whatever run of typing was open ended when someone else's write
        // arrived; coalescing across it would undo two authors at once.
        self.group_open = false;
    }

    pub fn editable(&self) -> bool {
        self.origin.is_some()
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len().saturating_sub(1)
    }

    /// The line a byte offset falls on, clamped to the last line.
    pub fn line_of(&self, byte: usize) -> usize {
        self.line_starts
            .partition_point(|s| *s as usize <= byte)
            .saturating_sub(1)
            .min(self.line_count().saturating_sub(1))
    }

    fn line_range(&self, i: usize) -> Range<usize> {
        if i + 1 >= self.line_starts.len() {
            return 0..0;
        }
        let s = self.line_starts[i] as usize;
        let mut e = self.line_starts[i + 1] as usize;
        // trim the trailing newline (and \r) — rows render one visual line
        if e > s && self.text.as_bytes()[e - 1] == b'\n' {
            e -= 1;
        }
        if e > s && self.text.as_bytes()[e - 1] == b'\r' {
            e -= 1;
        }
        s..e
    }

    pub fn line_text(&self, i: usize) -> &str {
        let r = self.line_range(i);
        self.text.get(r).unwrap_or("")
    }

    pub fn line_spans(&self, i: usize) -> &[Span] {
        match &self.spans {
            Some(s) => s.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            None => &[],
        }
    }

    pub fn holds(&self, comment: u64) -> bool {
        self.held.contains_key(&comment)
    }

    /// A cursor pair over the lines `from..=to` of `text`, a checkout of this
    /// buffer's document at some version. Through the trailing newline, so a
    /// blank line's run is not empty and a one-line run only collapses when the
    /// line itself is deleted.
    fn cursor_pair(text: &LoroDoc, from: u32, to: u32) -> Option<(Cursor, Cursor)> {
        let checkout = document::text(text);
        let starts = line_starts(&checkout);
        let start = *starts.get(from as usize)? as usize;
        let end = starts
            .get(to as usize + 1)
            .map_or(checkout.len(), |offset| *offset as usize);
        Some((
            document::cursor_at(text, start)?,
            document::cursor_at(text, end)?,
        ))
    }

    /// Take hold of the lines `from..=to` on behalf of `comment`, so that from
    /// now on the text carries the conversation. The lines are the disk
    /// version's — the text a comment's anchor names — so the cursors are minted
    /// there and read in the live text: typing above the run does not shift
    /// what it holds.
    ///
    /// Builds the document if there is not one yet: a comment is the other reason
    /// a path needs one, alongside being typed into.
    pub fn hold(&mut self, comment: u64, from: u32, to: u32) {
        if let Some(pair) = self.pair_on_disk(from, to) {
            self.held.insert(comment, pair);
        }
    }

    /// A cursor pair over lines `from..=to` of the disk version.
    fn pair_on_disk(&mut self, from: u32, to: u32) -> Option<(Cursor, Cursor)> {
        let doc = self.open_doc();
        let disk = doc.fork_at(&self.disk).ok()?;
        Self::cursor_pair(&disk, from, to)
    }

    /// The cursor pair for lines `from..=to` of the disk version, encoded for
    /// the store: what the CLI mints for a comment it makes on a worktree file,
    /// whose lines are the file's.
    pub fn cursors_on_disk(&mut self, from: u32, to: u32) -> Option<(Vec<u8>, Vec<u8>)> {
        let (from, to) = self.pair_on_disk(from, to)?;
        Some((document::encode_cursor(&from), document::encode_cursor(&to)))
    }

    /// The cursor pair for lines `from..=to` of the text as it is now, encoded
    /// for the store: what a comment made on this buffer carries from the start,
    /// so it never has to be re-found.
    pub fn cursors_at(&mut self, from: u32, to: u32) -> Option<(Vec<u8>, Vec<u8>)> {
        let doc = self.open_doc();
        let (from, to) = Self::cursor_pair(&doc, from, to)?;
        Some((document::encode_cursor(&from), document::encode_cursor(&to)))
    }

    /// The held pair of `comment`, encoded for the store.
    pub fn cursors_of(&self, comment: u64) -> Option<(Vec<u8>, Vec<u8>)> {
        let (from, to) = self.held.get(&comment)?;
        Some((document::encode_cursor(from), document::encode_cursor(to)))
    }

    /// Hold `comment` by a cursor pair minted elsewhere — in the app on another
    /// run, or by the CLI — in this same document. Refused when the pair does
    /// not resolve here: the cursors name operations this document does not
    /// have, so the comment's own lines are the better answer.
    pub fn adopt(&mut self, comment: u64, from: &[u8], to: &[u8]) -> bool {
        let Some(doc) = self.doc.as_ref() else {
            return false;
        };
        let (Some(from), Some(to)) = (document::decode_cursor(from), document::decode_cursor(to))
        else {
            return false;
        };
        if document::resolve(doc, &from).is_none() || document::resolve(doc, &to).is_none() {
            return false;
        }
        self.held.insert(comment, (from, to));
        true
    }

    /// Where each line of version `from` sits in version `to`, for the lines
    /// whose text is the same in both. A cursor at the line's start, minted in
    /// `from` and read in `to`, says where the line went; equal bytes say it is
    /// still that line. An edited line is not in the map — a changed line is
    /// un-reviewed by definition — and neither is a deleted one. This is what
    /// carries seen ticks across a save or an outside write.
    pub fn line_moves(&self, from: &Frontiers, to: &Frontiers) -> HashMap<u32, u32> {
        let mut moves = HashMap::new();
        let Some(doc) = &self.doc else {
            return moves;
        };
        let (Ok(before), Ok(after)) = (doc.fork_at(from), doc.fork_at(to)) else {
            return moves;
        };
        let (text_a, text_b) = (document::text(&before), document::text(&after));
        let (starts_a, starts_b) = (line_starts(&text_a), line_starts(&text_b));
        fn line<'a>(text: &'a str, starts: &[u32], i: usize) -> &'a str {
            let (s, e) = (starts[i] as usize, starts[i + 1] as usize);
            text[s..e].trim_end_matches('\n')
        }
        for i in 0..starts_a.len().saturating_sub(1) {
            let Some(cursor) = document::cursor_at(&before, starts_a[i] as usize) else {
                continue;
            };
            let Some((byte, held)) = document::resolve(&after, &cursor) else {
                continue;
            };
            let at = line_of(&starts_b, byte);
            if held
                && at + 1 < starts_b.len()
                && line(&text_a, &starts_a, i) == line(&text_b, &starts_b, at)
            {
                moves.insert(i as u32, at as u32);
            }
        }
        moves
    }

    /// The line a held comment renders under: its run's last line, GitHub's
    /// convention. `None` once the run is empty, which is the one thing that
    /// detaches a conversation — the text it was about is gone.
    pub fn held_line(&self, comment: u64) -> Option<u32> {
        let doc = self.doc.as_ref()?;
        let (from, to) = self.held.get(&comment)?;
        let (from, to) = (document::byte_of(doc, from)?, document::byte_of(doc, to)?);
        // Both ends slid onto the same point: every character between them was
        // deleted. Editing the run leaves it non-empty, however much changes.
        (to > from).then(|| self.line_of(to - 1) as u32)
    }

    /// The lines a held comment's run covers in the disk version — what the
    /// file has, not what is being typed — as `(first, last)`. `None` once the
    /// run is empty there.
    pub fn held_lines_on_disk(&self, comment: u64) -> Option<(u32, u32)> {
        let doc = self.doc.as_ref()?;
        let (from, to) = self.held.get(&comment)?;
        let disk = doc.fork_at(&self.disk).ok()?;
        let (from, to) = (
            document::byte_of(&disk, from)?,
            document::byte_of(&disk, to)?,
        );
        if to <= from {
            return None;
        }
        let starts = line_starts(&document::text(&disk));
        let line_of = |byte: usize| {
            starts
                .partition_point(|s| *s as usize <= byte)
                .saturating_sub(1)
        };
        Some((line_of(from) as u32, line_of(to - 1) as u32))
    }

    /// This buffer's document and disk version, encoded for the between-runs
    /// cache. `None` when there is no document to keep.
    pub fn saved_state(&self) -> Option<document::Saved> {
        let doc = self.doc.as_ref()?;
        Some(document::Saved {
            snapshot: document::snapshot(doc)?,
            disk: document::encode_version(&self.disk),
        })
    }

    /// Adopt a cached document, then fold in whatever was written to the file
    /// while it was gone — so a comment whose line was edited between runs is
    /// carried by its cursors once they are adopted into this document.
    ///
    /// Refuses when the cache does not describe this text: a snapshot that
    /// cannot be read, or a disk version that is not in it, would put positions
    /// on content this buffer never had.
    pub fn restore_state(&mut self, saved: &document::Saved, oid: ObjectId) -> bool {
        let Some(doc) = document::restore(&saved.snapshot) else {
            return false;
        };
        let Some(disk) = document::decode_version(&saved.disk) else {
            return false;
        };
        if document::text_at(&doc, &disk).is_none() {
            eprintln!("warning: cached buffer does not contain its own disk version");
            return false;
        }
        let bytes = self.text.clone();
        self.doc = Some(doc);
        self.disk = disk;
        // The file as it is now, imported onto what it was when we last looked.
        // `merge_disk` compares oids to decide whether anything changed, so the
        // blob has to carry the cached disk oid when it is called.
        self.oid = document::text_at(self.doc.as_ref().expect("just set"), &self.disk)
            .map_or(self.oid, |was| document::hash_object(was.as_bytes()));
        self.merge_disk(&bytes, oid);
        // `merge_disk` returns early when the file has not moved since the cache
        // — but the document may still hold typing that was never saved, which
        // the fresh read off disk does not have. The document is the buffer, so
        // it decides what is on screen either way.
        let merged = document::text(self.doc.as_ref().expect("just set"));
        if self.text != merged {
            self.text = merged;
            self.line_starts = line_starts(&self.text);
            self.spans = None;
            self.edit_rev += 1;
        }
        true
    }

    /// This text's document, built on demand.
    ///
    /// A blob only becomes a document when something needs one — the first
    /// keystroke, or a comment wanting a position that survives typing — so a
    /// diff of hundreds of files builds none. Returns a reference clone, which
    /// is a handle to the same document and not a copy of it.
    pub fn open_doc(&mut self) -> LoroDoc {
        if self.doc.is_none() {
            let (doc, disk) = document::open(&self.text);
            self.doc = Some(doc);
            self.disk = disk;
        }
        self.doc.clone().expect("just built")
    }

    /// Replace `range` with `insert` as an authored edit, recording the undo
    /// entry.
    ///
    /// Consecutive single-line inserts that carry on where the last one left
    /// off coalesce into one undo entry, so a typing run undoes as a run.
    /// [`Blob::break_group`] ends that run at a caret move or a save.
    pub fn edit(&mut self, range: Range<usize>, insert: &str) {
        let doc = self.open_doc();
        // Decided before the edit lands, while `prev.to` still resolves against
        // the text the last insert finished in.
        let continues = self.group_open
            && !insert.contains('\n')
            && range.is_empty()
            && self.undo.last().is_some_and(|prev| {
                prev.removed.is_empty() && document::byte_of(&doc, &prev.to) == Some(range.start)
            });

        // The document first, then the checkout the renderer reads.
        document::edit(&doc, range.clone(), insert);
        let removed = self.splice(range.clone(), insert);
        self.redo.clear();
        let (start, end) = (range.start, range.start + insert.len());

        match (continues, self.undo.last_mut()) {
            (true, Some(prev)) => {
                prev.inserted.push_str(insert);
                if let Some(to) = document::cursor_at(&doc, end) {
                    prev.to = to;
                }
            }
            // NOTE: an unplaceable cursor means the document and the text have
            // parted company, which no undo entry can describe. Dropping the
            // entry loses one undo step; applying it would corrupt the text.
            _ => match (
                document::cursor_at(&doc, start),
                document::cursor_at(&doc, end),
            ) {
                (Some(from), Some(to)) => self.undo.push(Edit {
                    from,
                    to,
                    removed,
                    inserted: insert.to_string(),
                }),
                _ => eprintln!("warning: cannot anchor an undo entry; step dropped"),
            },
        }
        self.group_open = true;
    }

    /// End the current typing run, so the next insert starts a fresh undo
    /// entry even if it lands where the last one finished.
    pub fn break_group(&mut self) {
        self.group_open = false;
    }

    /// Undo the last edit. Returns where the restored text now ends, so the
    /// caller can put the caret back where the typing started.
    pub fn undo(&mut self) -> Option<usize> {
        let edit = self.undo.pop()?;
        self.invert(edit, true)
    }

    pub fn redo(&mut self) -> Option<usize> {
        let edit = self.redo.pop()?;
        self.invert(edit, false)
    }

    /// Apply an entry backwards and file its inverse on the opposite stack.
    ///
    /// The entry's own run is found by resolving its cursors, so an undo still
    /// lands on the right text after someone else's write moved it. `None` when
    /// that run is no longer there to undo.
    fn invert(&mut self, edit: Edit, undoing: bool) -> Option<usize> {
        let doc = self.doc.clone()?;
        let from = document::byte_of(&doc, &edit.from)?;
        let to = document::byte_of(&doc, &edit.to)?;
        if from > to || to > self.text.len() {
            return None;
        }
        document::edit(&doc, from..to, &edit.removed);
        let removed = self.splice(from..to, &edit.removed);
        self.group_open = false;
        let end = from + edit.removed.len();
        let back = Edit {
            from: document::cursor_at(&doc, from)?,
            to: document::cursor_at(&doc, end)?,
            removed,
            inserted: edit.removed,
        };
        if undoing {
            self.redo.push(back);
        } else {
            self.undo.push(back);
        }
        Some(end)
    }

    /// The edit itself: splice the text, patch `line_starts` and `spans` around
    /// the change rather than rebuilding them, and bump `rev`. Returns what came
    /// out, which is what an undo entry has to put back.
    fn splice(&mut self, range: Range<usize>, insert: &str) -> String {
        let (start, end) = (range.start, range.end);
        let first = self.line_of(start);
        let last = self.line_of(end);
        let delta = insert.len() as i64 - (end - start) as i64;
        // `new` appends `text.len()` only when the text does not end on a
        // newline — otherwise the last newline's own entry already sits there.
        let had_sentinel = !self.text.is_empty() && !self.text.ends_with('\n');
        // Offsets one past each newline the replacement brings in — the same
        // thing `line_starts` is built from.
        let fresh: Vec<u32> = insert
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| (start + i + 1) as u32)
            .collect();
        let removed = self.text[range.clone()].to_string();
        self.text.replace_range(range, insert);

        // A line start at or before the edit is untouched, one inside it is
        // gone, and one past it moves by the byte delta. The trailing sentinel
        // is dropped first and re-derived after: whether the text needs one is
        // a property of its last byte, and this edit may have changed it.
        if had_sentinel {
            self.line_starts.pop();
        }
        let keep = self.line_starts.partition_point(|s| *s as usize <= start);
        let past = self.line_starts.partition_point(|s| *s as usize <= end);
        let tail: Vec<u32> = self.line_starts[past..]
            .iter()
            .map(|s| (*s as i64 + delta) as u32)
            .collect();
        let fresh_lines = fresh.len();
        self.line_starts.truncate(keep);
        self.line_starts.extend(fresh);
        self.line_starts.extend(tail);
        if self.line_starts.last().copied().unwrap_or(0) as usize != self.text.len() {
            self.line_starts.push(self.text.len() as u32);
        }

        // Keep the colours on screen until the reparse lands, and accept that
        // they are stale for a frame or two. They are computed off-thread over
        // the whole file, so anything dropped here shows as a flash of
        // unhighlighted code on every keystroke. That used to happen two ways,
        // and it was the worst thing about typing in this editor:
        //
        // - a line-count mismatch dropped `spans` entirely and greyed the whole
        //   file. It is clamped now: what exists is patched, the rest is left.
        // - an edit inside one line blanked that line. Its old spans are close
        //   enough until the reparse arrives; a token whose colour ends a
        //   character early is nothing next to the line going plain and coming
        //   back.
        //
        // Only an edit that changes the line structure still blanks, and only
        // the lines it spans: there the old spans describe different lines, so
        // keeping them would colour the wrong text.
        if let Some(spans) = &mut self.spans {
            let structural = fresh_lines > 0 || last > first;
            let first = first.min(spans.len());
            let last = last.min(spans.len().saturating_sub(1));
            if structural && first <= last {
                spans.splice(first..last + 1, vec![Vec::new(); fresh_lines + 1]);
            }
        }

        self.edit_rev += 1;
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u8) -> ObjectId {
        ObjectId::from_hex(format!("{n:040x}").as_bytes()).expect("valid hex")
    }

    fn blob(text: &str) -> Blob {
        Blob::new(oid(1), "rs".into(), text.into())
    }

    /// The one hand-rolled data structure here: an edit patches `line_starts`
    /// around the change instead of rescanning, so every shape of edit has to
    /// land on exactly what a fresh scan of the same text would have produced.
    #[test]
    fn edit_patches_line_starts_like_a_fresh_scan() {
        let cases = [
            ("a\nb\nc\n", 2..2, "X"),      // insert mid-line
            ("a\nb\nc\n", 2..2, "\n"),     // insert a newline
            ("a\nb\nc\n", 0..0, "X"),      // insert at byte 0
            ("a\nb\nc\n", 6..6, "x"),      // append past a trailing newline
            ("a\nb", 3..3, "\n"),          // append a trailing newline
            ("a\nb", 2..3, ""),            // delete to the end of the text
            ("a\nb\nc\n", 1..5, ""),       // delete across lines
            ("a\nb\nc\n", 0..6, ""),       // delete everything
            ("", 0..0, "a"),               // type into an empty buffer
            ("a\r\nb\r\n", 3..3, "Z"),     // CRLF
            ("a\nb\nc\n", 2..4, "X\nY\n"), // replace a line with two
        ];
        for (text, range, insert) in cases {
            let mut b = blob(text);
            b.edit(range.clone(), insert);
            let mut expected = text.to_string();
            expected.replace_range(range.clone(), insert);
            assert_eq!(b.text, expected, "text for {text:?} {range:?} {insert:?}");
            assert_eq!(
                b.line_starts,
                blob(&expected).line_starts,
                "line_starts for {text:?} {range:?} {insert:?}"
            );
            // The rendered lines have to agree too — that is what rows read.
            let lines: Vec<_> = (0..b.line_count()).map(|i| b.line_text(i)).collect();
            let fresh = blob(&expected);
            let want: Vec<_> = (0..fresh.line_count())
                .map(|i| fresh.line_text(i))
                .collect();
            assert_eq!(lines, want, "lines for {text:?} {range:?} {insert:?}");
        }
    }

    #[test]
    fn undo_redo_round_trips_to_the_original_text() {
        let mut b = blob("a\nb\nc\n");
        b.edit(2..4, "XY\n");
        b.break_group();
        b.edit(0..1, "");
        assert_eq!(b.text, "\nXY\nc\n");
        b.undo();
        b.undo();
        assert_eq!(b.text, "a\nb\nc\n");
        assert_eq!(b.line_starts, blob("a\nb\nc\n").line_starts);
        b.redo();
        b.redo();
        assert_eq!(b.text, "\nXY\nc\n");
    }

    #[test]
    fn typing_coalesces_into_one_undo_step_until_a_newline() {
        let mut b = blob("a\n");
        for (i, ch) in ["h", "i", "!"].iter().enumerate() {
            b.edit(1 + i..1 + i, ch);
        }
        assert_eq!(b.text, "ahi!\n");
        assert_eq!(b.undo.len(), 1);
        b.undo();
        assert_eq!(b.text, "a\n");

        // A newline breaks the run: it is a place you would want to stop.
        let mut b = blob("a\n");
        b.edit(1..1, "h");
        b.edit(2..2, "\n");
        assert_eq!(b.undo.len(), 2);
    }

    #[test]
    fn break_group_starts_a_fresh_undo_entry_where_the_last_one_ended() {
        let mut b = blob("a\n");
        b.edit(1..1, "h");
        b.break_group();
        b.edit(2..2, "i");
        assert_eq!(b.undo.len(), 2);
    }

    /// An anchor names a line of the text as it was read. These two edits look
    /// the same in line numbers — both are "an edit at line 1" — and have
    /// opposite effects on the line the anchor sits on.
    #[test]
    fn an_anchor_tells_a_prefixed_line_from_a_line_pushed_down() {
        let mut prefixed = blob("a\nb\nc\n");
        prefixed.edit(2..2, "X");
        assert_eq!(
            prefixed.anchor_line(1),
            Some(1),
            "'b' became 'Xb', in place"
        );
        assert_eq!(prefixed.anchor_line(2), Some(2));

        let mut pushed = blob("a\nb\nc\n");
        pushed.edit(2..2, "\n");
        assert_eq!(pushed.anchor_line(1), Some(2), "'b' moved down a line");
        assert_eq!(pushed.anchor_line(2), Some(3));
    }

    #[test]
    fn an_anchor_on_deleted_text_has_no_line_left() {
        let mut b = blob("a\nb\nc\n");
        b.edit(2..6, "");
        assert_eq!(b.anchor_line(0), Some(0));
        assert_eq!(b.anchor_line(1), None);
        assert_eq!(b.anchor_line(2), None);
    }

    #[test]
    fn an_untouched_blob_anchors_a_line_on_itself() {
        let b = blob("a\nb\nc\n");
        assert_eq!(b.anchor_line(2), Some(2));
        assert_eq!(b.anchor_line(3), None, "past the end of the file");
        assert!(!b.dirty());
    }

    #[test]
    fn an_edit_bumps_the_rev_and_reads_as_unsaved() {
        let mut b = blob("a\nb\nc\n");
        b.edit(2..4, "X\nY\n");
        assert_eq!(b.edit_rev, 1);
        assert_eq!(b.text, "a\nX\nY\nc\n");
        assert!(b.dirty(), "past the disk version");
        b.saved(oid(2));
        assert!(!b.dirty(), "the disk version caught up");
    }

    /// A thread must survive its line being edited, not only moved. Anything
    /// less turns "someone improved this line" into "the conversation about it
    /// disappeared", and resolving a thread becomes an accident.
    #[test]
    fn a_held_thread_survives_arbitrary_editing_of_its_own_line() {
        let mut b = blob("# Intro\n## Getting Started\nsome prose\n");
        b.hold(7, 1, 1);
        assert_eq!(b.held_line(7), Some(1));

        // A space: the case that used to be the only one that worked.
        let at = b.line_starts[2] as usize - 1;
        b.edit(at..at, " ");
        assert_eq!(b.held_line(7), Some(1));
        // A character appended — this is what used to detach it.
        let at = b.line_starts[2] as usize - 1;
        b.edit(at..at, "!");
        assert_eq!(b.held_line(7), Some(1), "appending a character keeps it");
        // A character deleted from the middle of the heading.
        let at = b.text.find("Getting").expect("still there");
        b.edit(at..at + 1, "");
        assert_eq!(b.held_line(7), Some(1), "deleting a character keeps it");
        // Typing at the very front of the line, which a single-point anchor
        // would have lost.
        let at = b.line_starts[1] as usize;
        b.edit(at..at, "> ");
        assert_eq!(b.held_line(7), Some(1), "editing the front keeps it");
        // And a line inserted above still carries it down.
        b.edit(0..0, "// header\n");
        assert_eq!(b.held_line(7), Some(2));
    }

    #[test]
    fn a_held_thread_detaches_only_when_its_text_is_deleted() {
        let mut b = blob("# Intro\n## Getting Started\nsome prose\n");
        b.hold(7, 1, 1);
        let (from, to) = (b.line_starts[1] as usize, b.line_starts[2] as usize);
        b.edit(from..to, "");
        assert_eq!(
            b.held_line(7),
            None,
            "the text it was about is gone, so the thread is"
        );
    }

    /// The case that started this: an agent rewrites the very line a comment is
    /// on. A line-granular import would delete and reinsert the line, taking the
    /// anchor with it — `shrink` is what keeps the edit *inside* the run.
    #[test]
    fn a_held_thread_survives_an_external_write_that_rewrites_its_line() {
        let mut b = blob("# Intro\n## Getting Started\nsome prose\n");
        b.hold(7, 1, 1);
        b.merge_disk("# Intro\n## Getting Started, Quickly\nsome prose\n", oid(3));
        assert_eq!(b.held_line(7), Some(1), "the rewritten line kept it");

        // And a line added above by another external write carries it down.
        b.merge_disk(
            "// top\n# Intro\n## Getting Started, Quickly\nsome prose\n",
            oid(4),
        );
        assert_eq!(b.held_line(7), Some(2));
    }

    /// The last hole: the app is closed when the file is edited, so there is no
    /// document in memory to ride the change. The cached one can, and that is
    /// why we cache it. The cursors travel with the comment and are adopted
    /// into the restored document.
    #[test]
    fn a_cached_buffer_carries_its_comments_through_an_edit_made_while_closed() {
        let mut before = blob("# Intro\n## Getting Started\nsome prose\n");
        before.hold(7, 1, 1);
        let (from, to) = before.cursors_of(7).expect("held");
        let saved = before.saved_state().expect("a document to cache");

        // A new process: the file is read fresh, already reworded.
        let mut after = blob("# Intro\n## Getting Started, Quickly\nsome prose\n");
        assert!(after.restore_state(&saved, oid(2)));
        assert!(
            after.adopt(7, &from, &to),
            "the pair resolves in the restored document"
        );
        assert_eq!(
            after.held_line(7),
            Some(1),
            "the thread came back onto its reworded line"
        );
        // A document that never had those operations refuses the pair.
        let mut stranger = blob("# Intro\n## Getting Started, Quickly\nsome prose\n");
        stranger.open_doc();
        assert!(!stranger.adopt(7, &from, &to));
        assert_eq!(
            after.text,
            "# Intro\n## Getting Started, Quickly\nsome prose\n"
        );
        assert!(!after.dirty(), "nothing unsaved: the file is what it is");
    }

    #[test]
    fn a_cached_buffer_brings_back_typing_that_was_never_saved() {
        let mut before = blob("fn a() {}\n");
        before.edit(0..0, "// unsaved\n");
        assert!(before.dirty());
        let saved = before.saved_state().expect("a document to cache");

        // The file on disk never had the typed line.
        let mut after = blob("fn a() {}\n");
        assert!(after.restore_state(&saved, oid(1)));
        assert!(after.text.contains("// unsaved"), "the typing came back");
        assert!(after.dirty(), "and is still unsaved");
    }

    #[test]
    fn a_cache_that_does_not_describe_this_text_is_refused() {
        let mut b = blob("fn a() {}\n");
        let junk = document::Saved {
            snapshot: vec![0, 1, 2, 3],
            disk: Vec::new(),
        };
        assert!(!b.restore_state(&junk, oid(2)));
        assert_eq!(b.text, "fn a() {}\n", "and the buffer is left alone");
        assert!(b.doc.is_none());
    }

    /// Ticks follow lines that only moved; an edited or deleted line drops its
    /// tick, because a changed line is un-reviewed.
    #[test]
    fn line_moves_carry_untouched_lines_and_drop_edited_and_deleted_ones() {
        let mut b = blob("a\nb\nc\nd\n");
        let from = b.open_doc().oplog_frontiers();
        b.edit(0..0, "x\n"); // `a`, `b`, `c`, `d` all move down one
        b.edit(6..7, "C"); // `c` (now line 3) is edited in place
        b.edit(8..10, ""); // `d` is deleted
        assert_eq!(b.text, "x\na\nb\nC\n");
        let to = b.doc.as_ref().unwrap().oplog_frontiers();
        let moves = b.line_moves(&from, &to);
        assert_eq!(moves.get(&0), Some(&1), "`a` moved down one");
        assert_eq!(moves.get(&1), Some(&2), "`b` moved down one");
        assert_eq!(moves.get(&2), None, "`c` was edited: un-reviewed");
        assert_eq!(moves.get(&3), None, "`d` is gone");
        assert!(b.line_moves(&to, &from).is_empty() || moves.len() == 2);
    }

    #[test]
    fn a_blank_line_can_hold_a_thread() {
        // The run covers the newline, so a line with no text of its own is still
        // a non-empty range rather than an instantly-detached one.
        let mut b = blob("a\n\nb\n");
        b.hold(1, 1, 1);
        assert_eq!(b.held_line(1), Some(1));
    }

    /// The invariant everything here rests on: the document and the string the
    /// renderer reads are the same text. They are maintained separately — the
    /// document takes operations, the string is spliced incrementally so typing
    /// does not re-highlight the file — so if this breaks, nothing else here
    /// holds.
    #[test]
    fn the_buffer_and_its_document_hold_the_same_text() {
        let mut b = blob("fn a() {\n    x();\n}\n");
        for (range, insert) in [
            (8..8, "\n    // one"),
            (0..0, "// top\n"),
            (10..14, ""),
            (5..5, "héllo ✅"),
        ] {
            b.edit(range, insert);
            assert_eq!(
                b.text,
                document::text(b.doc.as_ref().expect("a document by now")),
                "diverged after an edit"
            );
        }
    }

    #[test]
    fn an_external_write_merges_with_unsaved_typing_instead_of_replacing_it() {
        let mut b = blob("fn a() {}\nfn b() {}\n");
        b.edit(0..0, "// mine\n");
        // An agent rewrites the file on disk while that line is unsaved.
        b.merge_disk("fn a() {}\nfn NEW() {}\nfn b() {}\n", oid(2));
        assert!(b.text.contains("// mine"), "typing survived the write");
        assert!(b.text.contains("fn NEW"), "the write survived the typing");
        assert!(b.dirty(), "still unsaved: the typed line is not on disk");
        // And the line table was rebuilt for the merged text, not the old one.
        assert_eq!(b.line_count(), 4);
        assert_eq!(b.line_text(0), "// mine");
    }

    #[test]
    fn undo_after_an_external_write_takes_back_only_what_was_typed_here() {
        let mut b = blob("fn a() {}\nfn b() {}\n");
        b.edit(0..0, "// mine\n");
        b.merge_disk("fn a() {}\nfn NEW() {}\nfn b() {}\n", oid(2));
        b.undo();
        assert!(
            b.text.contains("fn NEW"),
            "undo must not eat an agent's work"
        );
        assert!(!b.text.contains("// mine"), "the typed line went back out");
    }

    /// What the retired byte-shift log existed for: a stored anchor names a
    /// line of the text as it was read, and typing above it must carry it down.
    #[test]
    fn a_disk_line_pushed_down_by_typing_resolves_to_its_new_number() {
        let mut b = blob("fn a() {}\nfn b() {}\n");
        b.edit(0..0, "// one\n// two\n");
        assert_eq!(b.anchor_line(0), Some(2), "'fn a' moved down two");
        assert_eq!(b.anchor_line(1), Some(3), "'fn b' moved down two");
        // And a line merely prefixed stays where it is — indistinguishable from
        // the above in line numbers, opposite in effect.
        let mut prefixed = blob("fn a() {}\nfn b() {}\n");
        prefixed.edit(10..10, "    ");
        assert_eq!(prefixed.anchor_line(1), Some(1));
    }

    #[test]
    fn an_edit_keeps_the_colors_of_the_line_it_is_typed_into() {
        let mark = || {
            vec![Span {
                start: 0,
                end: 1,
                hl: None,
            }]
        };
        let coloured = |text: &str, lines: usize| {
            let mut b = blob(text);
            b.spans = Some(vec![mark(); lines]);
            b
        };

        // Typing inside a line keeps every line's colours, the edited one
        // included: they are a frame or two stale, and blanking them would read
        // as the file flashing plain on every keystroke.
        let mut b = coloured("a\nb\nc\n", 3);
        b.edit(2..2, "X");
        let spans = b.spans.as_ref().expect("colours kept");
        assert!(spans.iter().all(|s| !s.is_empty()), "nothing went plain");

        // An edit that changes the line structure is different: the old spans
        // describe different lines afterwards, so the ones it spans do blank.
        let mut b = coloured("a\nb\nc\n", 3);
        b.edit(2..2, "\n");
        let spans = b.spans.as_ref().expect("colours kept");
        assert!(!spans[0].is_empty(), "the line above keeps its colors");
        assert!(spans[1].is_empty(), "the split line waits for the reparse");
        assert!(!spans.last().expect("a last line").is_empty());

        // And a span table shorter than the text — an edit past what has been
        // highlighted so far — patches what exists instead of dropping the lot.
        let mut b = coloured("a\nb\nc\n", 1);
        b.edit(4..4, "Z");
        assert!(
            b.spans.as_ref().is_some_and(|s| !s[0].is_empty()),
            "the whole file must not go plain because one line was missing"
        );
    }
}
