//! A file's text as a versioned, mergeable document — one CRDT per file.
//!
//! A file being edited has more than one writer: you typing, an agent in the
//! built-in terminal, the CLI, `git checkout`. As a `String` those writers can
//! only take turns. Whoever saves last wins, and everything anchored into the
//! text (a comment, the caret, a selection) is anchored to a byte offset that
//! the other writer's edit quietly invalidates.
//!
//! As a document they merge, and positions ride the operations instead of
//! counting bytes:
//!
//! - **version** — a frontier in the document's history, tagged with the git
//!   blob oid whose bytes it reproduces. Every git-side revision of the file
//!   we need to address is one of these.
//! - **cursor** — a position anchored to an operation id. Stable across every
//!   edit before or after it; a comment, the caret and each end of a
//!   selection store one of these instead of a line and a byte.
//!
//! ## Two kinds of operation
//!
//! *Authored* ops are someone typing here; they are the only thing in a
//! document that cannot be recomputed. *Import* ops are derived from bytes we
//! already have — a blob, a worktree write — and [`import`] mints them
//! deterministically: the same parent version and the same bytes give
//! byte-identical operations on any machine. So the git-derived part of a
//! document's history is a pure function of git history: a cache, where a miss
//! costs time, not correctness.
//!
//! Determinism needs four things, and each is part of the format:
//!
//! 1. a fixed diff — histogram + `postprocess_lines`;
//! 2. a fixed order of application — highest offset first;
//! 3. a peer id derived from the import's identity, parent version included;
//! 4. a stable hash to derive it with.
//!
//! All four come from [`concats_text`]. That is why they are a crate rather
//! than a module: two implementations of the diff would be two histories that
//! cannot be reconciled.
//!
//! Import ops are minted in a fork at the parent version and replayed into the
//! live document as remote updates; the live document's peer id never changes.
//! Two reasons. An undo scope binds to the peer id, and Loro warns that
//! changing it mid-life disrupts undo grouping. And forking at the parent is
//! what makes the ops causally correct: insert positions have to be computed
//! against the parent's text. The payoff: an external write arrives as a remote
//! op, so it is on nobody's undo stack — which is what ⌘Z should do after an
//! agent writes the file under you.

// NOTE: `pedantic` is off here. The code came from a crate that never ran under
// it, and turning it on means 300+ findings — mostly `must_use_candidate`,
// numeric casts and missing `# Errors` docs. Worth doing, but not inside a
// move, where it would hide the move. `all`, `style` and `complexity` are on,
// as everywhere in the workspace.
#![allow(clippy::pedantic, clippy::cognitive_complexity)]

use std::ops::Range;

use concats_text::{fnv1a, replacements};
use gix::ObjectId;
/// A version of a document: a frontier in its history. Named here so a caller
/// can hold one without depending on Loro itself.
pub use loro::Frontiers as Version;
use loro::{
    ExportMode, Frontiers, LoroDoc,
    cursor::{Cursor, PosType, Side},
};

/// The document's one text container. Writer and reader must name the same
/// container, so it is named once.
const TEXT: &str = "text";

/// git's blob hash of `bytes`, without writing the object.
///
/// It lives here because this is how a document version is named: every
/// git-side revision of a file is a version, and the oid is the name both sides
/// use for it.
pub fn hash_object(bytes: &[u8]) -> ObjectId {
    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, bytes)
        .expect("sha1 hashing is infallible")
}

/// The peer id an import's operations are minted under.
///
/// Derived from the import's full identity, the parent version and the content
/// tag, because both halves matter, in opposite directions:
///
/// - same parent, same bytes, two machines: the same peer id, so the two
///   imports are the same operations and merging them dedups instead of
///   inserting the text twice;
/// - same bytes, different parents: different peer ids, because the
///   operations differ, and two different operations sharing an id corrupts
///   the log.
fn import_peer(parent: &Frontiers, tag: &str) -> u64 {
    let mut buf = Vec::new();
    for id in parent.iter() {
        buf.extend_from_slice(&id.peer.to_le_bytes());
        buf.extend_from_slice(&id.counter.to_le_bytes());
    }
    // Not a byte any oid hex or counter can produce, so the parent list cannot
    // run into the tag and make two different identities hash the same.
    buf.push(0xff);
    buf.extend_from_slice(tag.as_bytes());
    // Loro reserves the top of the peer space for its own internal peers.
    fnv1a(&buf) & 0x00ff_ffff_ffff_ffff
}

/// A document holding `bytes` as its first version.
pub fn open(bytes: &str) -> (LoroDoc, Frontiers) {
    let doc = LoroDoc::new();
    let version = import(&doc, &Frontiers::default(), bytes);
    (doc, version)
}

/// Bring `bytes` into the document as a new version on top of `parent`.
///
/// This is the one way anything that is not a keystroke here enters: a blob at
/// either end of the range, a worktree file, an agent's write, the blob a GitHub
/// comment was written against. Returns the version reproducing `bytes`.
pub fn import(doc: &LoroDoc, parent: &Frontiers, bytes: &str) -> Frontiers {
    // The tag is the content's git oid, computed here rather than passed in, so
    // a caller cannot label an import with bytes it does not have.
    let tag = hash_object(bytes.as_bytes()).to_string();
    // NOTE: a parent that is not in this document's history means the caller
    // mixed up two files' versions; there is nothing to merge onto, so the
    // import is dropped rather than silently rebased onto the wrong text.
    let Ok(fork) = doc.fork_at(parent) else {
        eprintln!("warning: import parent is not in this document's history");
        return doc.oplog_frontiers();
    };
    if let Err(error) = fork.set_peer_id(import_peer(parent, &tag)) {
        eprintln!("warning: cannot set import peer id: {error}");
        return doc.oplog_frontiers();
    }
    let since = fork.oplog_vv();
    let text = fork.get_text(TEXT);
    let old = text.to_string();
    for (del, ins) in replacements(&old, bytes) {
        let at = del.start;
        // NOTE: the ranges come out of `hunks` over this same text, so they are
        // in bounds; a failure here is a Loro invariant break, logged rather
        // than dropped.
        if !del.is_empty()
            && let Err(error) = text.delete_utf8(at, del.len())
        {
            eprintln!("warning: cannot apply an import hunk: {error}");
        }
        if !ins.is_empty()
            && let Err(error) = text.insert_utf8(at, &ins)
        {
            eprintln!("warning: cannot apply an import hunk: {error}");
        }
    }
    fork.commit();
    let version = fork.oplog_frontiers();
    match fork.export(ExportMode::updates(&since)) {
        // Empty when the bytes already matched: the version is the parent, and
        // re-importing unchanged content is free.
        Ok(updates) if !updates.is_empty() => {
            if let Err(error) = doc.import(&updates) {
                eprintln!("warning: cannot apply import: {error}");
                return doc.oplog_frontiers();
            }
        }
        Ok(_) => {}
        Err(error) => eprintln!("warning: cannot encode import: {error}"),
    }
    version
}

/// Replace `range` with `insert` as an authored operation: someone typing here,
/// under this document's own peer id.
pub fn edit(doc: &LoroDoc, range: Range<usize>, insert: &str) {
    let text = doc.get_text(TEXT);
    // NOTE: callers pass ranges into the current text, so these are in bounds;
    // a failure is a Loro invariant break, logged rather than silently dropped.
    if !range.is_empty()
        && let Err(error) = text.delete_utf8(range.start, range.len())
    {
        eprintln!("warning: cannot apply an edit: {error}");
    }
    if !insert.is_empty()
        && let Err(error) = text.insert_utf8(range.start, insert)
    {
        eprintln!("warning: cannot apply an edit: {error}");
    }
    doc.commit();
}

pub fn text(doc: &LoroDoc) -> String {
    doc.get_text(TEXT).to_string()
}

/// A cursor at a byte offset, the unit every caller here already speaks.
///
/// The cursor anchors to the character at `byte` and follows it: text inserted
/// at exactly this offset lands before the anchor, so a position on a line
/// comes down with that line when something is inserted above it. (Measured,
/// not assumed: `Side` turns out not to change this for whole-line inserts, so
/// there is one of these rather than one per bias.)
pub fn cursor_at(doc: &LoroDoc, byte: usize) -> Option<Cursor> {
    let text = doc.get_text(TEXT);
    let at = text.convert_pos(byte, PosType::Bytes, PosType::Event)?;
    text.get_cursor(at, Side::Left)
}

/// Where a cursor sits in the document's current version, as a byte offset, and
/// whether the character it was anchored to is still there.
///
/// The second half matters because a cursor whose content was deleted does not
/// fail: Loro slides it onto the neighbour and says that it had to. So a
/// position on its own cannot tell "the code moved here" from "the code is gone
/// and this is what is next to it". `held` is that distinction. It stays true
/// across a line edited in place (still the same character) and goes false as
/// soon as the anchor itself is deleted.
pub fn resolve(doc: &LoroDoc, cursor: &Cursor) -> Option<(usize, bool)> {
    let found = doc.get_cursor_pos(cursor).ok()?;
    let byte = doc
        .get_text(TEXT)
        .convert_pos(found.current.pos, PosType::Event, PosType::Bytes)?;
    Some((byte, found.update.is_none()))
}

/// Where a cursor sits now, whether or not its content survived.
pub fn byte_of(doc: &LoroDoc, cursor: &Cursor) -> Option<usize> {
    Some(resolve(doc, cursor)?.0)
}

/// The bytes of an older version.
///
/// A fork rather than a `checkout`: checkout moves the document's own state back
/// and detaches it, and the buffer on screen must not lurch because something
/// asked what the file used to look like. The same fork is how a cursor resolves
/// against an older version — see the test below.
pub fn text_at(doc: &LoroDoc, version: &Frontiers) -> Option<String> {
    Some(text(&doc.fork_at(version).ok()?))
}

/// A buffer's document as it is kept between runs.
///
/// We cache it for the operation ids, not the content. The file is the
/// content; the ids a comment's cursors name exist only in a document. Without
/// the cache a conversation on a line edited while the app was closed has
/// nothing to hold it when the app comes back. Restoring the document and then
/// importing the file as it is now folds those edits in, with the cursors
/// riding them.
///
/// It also means unsaved typing survives a restart without ever being written
/// to the file.
pub struct Saved {
    pub snapshot: Vec<u8>,
    /// The version reproducing the bytes that were on disk, so a restore knows
    /// what to import the current file *onto*.
    pub disk: Vec<u8>,
}

pub fn snapshot(doc: &LoroDoc) -> Option<Vec<u8>> {
    doc.export(ExportMode::snapshot())
        .inspect_err(|error| eprintln!("warning: cannot snapshot a buffer: {error}"))
        .ok()
}

pub fn restore(bytes: &[u8]) -> Option<LoroDoc> {
    let doc = LoroDoc::new();
    doc.import(bytes)
        .inspect_err(|error| eprintln!("warning: cannot restore a buffer: {error}"))
        .ok()?;
    Some(doc)
}

/// A version as `(peer, counter)` pairs. Written out because Loro exposes
/// frontiers as ids rather than as bytes, and this has to round-trip through a
/// database column.
pub fn encode_version(version: &Frontiers) -> Vec<u8> {
    let mut out = Vec::new();
    for id in version.iter() {
        out.extend_from_slice(&id.peer.to_le_bytes());
        out.extend_from_slice(&id.counter.to_le_bytes());
    }
    out
}

pub fn decode_version(bytes: &[u8]) -> Option<Frontiers> {
    if !bytes.len().is_multiple_of(12) {
        return None;
    }
    Some(
        bytes
            .chunks_exact(12)
            .map(|id| loro::ID {
                peer: u64::from_le_bytes(id[..8].try_into().expect("8 bytes")),
                counter: i32::from_le_bytes(id[8..].try_into().expect("4 bytes")),
            })
            .collect(),
    )
}

pub fn encode_cursor(cursor: &Cursor) -> Vec<u8> {
    cursor.encode()
}

pub fn decode_cursor(bytes: &[u8]) -> Option<Cursor> {
    Cursor::decode(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = "fn a() {}\nfn b() {}\nfn c() {}\n";
    const V2: &str = "fn a() {}\nfn NEW() {}\nfn b() {}\nfn c() {}\n";

    /// Q2 of the RFC: is a deterministic import achievable with a settable peer
    /// id alone? Two replicas, the same imports, byte-identical histories.
    ///
    /// This is the test the RFC calls for by name. If it ever fails, the same
    /// comment resolves differently in two checkouts and two oplogs cannot be
    /// reconciled, so it fails loudly rather than drifting.
    #[test]
    fn the_same_imports_on_two_replicas_produce_identical_operations() {
        let mut versions = Vec::new();
        let replicas: Vec<LoroDoc> = (0..2)
            .map(|_| {
                let doc = LoroDoc::new();
                let mut at = Frontiers::default();
                for bytes in [V1, V2, "fn a() {}\n"] {
                    at = import(&doc, &at, bytes);
                }
                versions.push(at);
                doc
            })
            .collect();
        let bytes = |doc: &LoroDoc| doc.export(ExportMode::all_updates()).unwrap();
        assert_eq!(bytes(&replicas[0]), bytes(&replicas[1]));
        assert_eq!(versions[0], versions[1]);
        assert_eq!(text(&replicas[0]), "fn a() {}\n");
    }

    #[test]
    fn two_replicas_identical_imports_merge_into_one_history() {
        let (a, b) = (LoroDoc::new(), LoroDoc::new());
        import(&a, &Frontiers::default(), V1);
        import(&b, &Frontiers::default(), V1);
        let merged = LoroDoc::new();
        merged
            .import(&a.export(ExportMode::all_updates()).unwrap())
            .unwrap();
        merged
            .import(&b.export(ExportMode::all_updates()).unwrap())
            .unwrap();
        assert_eq!(text(&merged), V1, "the same import twice is not twice");
    }

    #[test]
    fn an_import_of_the_same_bytes_onto_a_different_parent_gets_its_own_peer() {
        let doc = LoroDoc::new();
        let one = import(&doc, &Frontiers::default(), V1);
        let two = import(&doc, &one, V2);
        assert_ne!(
            import_peer(&one, "same-tag"),
            import_peer(&two, "same-tag"),
            "distinct operations must never share a peer id"
        );
    }

    /// Q1 of the RFC: does cursor resolution hold against an older version?
    #[test]
    fn a_cursor_follows_content_forward_and_resolves_in_the_older_version() {
        let (doc, v1) = open(V1);
        let was = V1.find("fn b").unwrap();
        let cursor = cursor_at(&doc, was).unwrap();
        import(&doc, &v1, V2);
        assert_eq!(byte_of(&doc, &cursor), Some(V2.find("fn b").unwrap()));
        // …and in the version it was placed in, resolved on a fork so the live
        // buffer is not moved to answer the question.
        let older = doc.fork_at(&v1).unwrap();
        assert_eq!(byte_of(&older, &cursor), Some(was));
        assert!(
            !doc.is_detached(),
            "resolving history must not move the live buffer"
        );
    }

    #[test]
    fn a_cursor_survives_an_edit_typed_above_it() {
        let (doc, _) = open(V1);
        let cursor = cursor_at(&doc, V1.find("fn c").unwrap()).unwrap();
        edit(&doc, 0..0, "// header\n");
        assert_eq!(
            byte_of(&doc, &cursor),
            Some(text(&doc).find("fn c").unwrap())
        );
    }

    /// The finding that corrected the RFC: deleted content does not make a
    /// cursor unresolvable, so "outdated" cannot be read off a cursor.
    #[test]
    fn a_cursor_whose_content_is_deleted_slides_rather_than_failing() {
        let (doc, v1) = open(V1);
        let cursor = cursor_at(&doc, V1.find("fn b").unwrap()).unwrap();
        import(&doc, &v1, "fn a() {}\nfn c() {}\n");
        assert!(
            byte_of(&doc, &cursor).is_some(),
            "a slid cursor still resolves — content, not position, decides outdated"
        );
    }

    #[test]
    fn an_import_merges_with_typing_instead_of_replacing_it() {
        // The RFC's simultaneous-editing case: an agent writes the file while
        // there are unsaved edits in the buffer.
        let (doc, disk) = open(V1);
        edit(&doc, 0..0, "// mine\n");
        let agent = V2;
        import(&doc, &disk, agent);
        let merged = text(&doc);
        assert!(merged.contains("// mine"), "typing survived the write");
        assert!(merged.contains("fn NEW"), "the write survived the typing");
    }

    #[test]
    fn byte_offsets_round_trip_through_multibyte_text() {
        let (doc, _) = open("let s = \"héllo wörld ✅\";\nlet t = 1;\n");
        let at = text(&doc).find("let t").unwrap();
        let cursor = cursor_at(&doc, at).unwrap();
        assert_eq!(byte_of(&doc, &cursor), Some(at));
    }

    /// A line edited in place must import as a change inside the line, not as a
    /// delete and reinsert of the whole line. The latter takes every cursor
    /// anchored into it; that is how a comment on a heading disappears the
    /// moment someone corrects a word in it.
    #[test]
    fn editing_within_a_line_imports_as_a_narrow_change() {
        let old = "# Intro\n## Getting Started\nsome prose\n";
        let new = "# Intro\n## Getting Started, Quickly\nsome prose\n";
        let (doc, v1) = open(old);
        let at = old.find("## Getting").unwrap();
        let line = cursor_at(&doc, at).unwrap();
        let through = cursor_at(&doc, old.find("some prose").unwrap()).unwrap();
        import(&doc, &v1, new);
        assert_eq!(byte_of(&doc, &line), Some(at));
        assert!(
            resolve(&doc, &line).is_some_and(|(_, held)| held),
            "the anchored character was never deleted"
        );
        assert_eq!(
            byte_of(&doc, &through),
            Some(text(&doc).find("some prose").unwrap())
        );
    }

    #[test]
    fn a_version_round_trips_through_the_cache_encoding() {
        let (doc, v1) = open(V1);
        import(&doc, &v1, V2);
        let back = decode_version(&encode_version(&v1)).expect("decodes");
        assert_eq!(back, v1);
        assert_eq!(text_at(&doc, &back).as_deref(), Some(V1));
        assert_eq!(decode_version(&[]), Some(Frontiers::default()));
        assert_eq!(
            decode_version(&[0, 1, 2]),
            None,
            "a truncated id is refused"
        );
    }

    #[test]
    fn a_snapshot_round_trip_keeps_cursors_resolvable() {
        let (doc, _) = open(V1);
        let at = V1.find("fn b").unwrap();
        let cursor = cursor_at(&doc, at).expect("a cursor");
        let bytes = snapshot(&doc).expect("a snapshot");

        // A different process, so the cursor arrives through its own encoding.
        let back = restore(&bytes).expect("restores");
        let cursor = decode_cursor(&encode_cursor(&cursor)).expect("decodes");
        assert_eq!(byte_of(&back, &cursor), Some(at));
        assert_eq!(text(&back), V1);
    }

    #[test]
    fn the_disk_version_still_reads_back_after_typing() {
        let (doc, disk) = open(V1);
        edit(&doc, 0..0, "// mine\n");
        assert_eq!(text_at(&doc, &disk).as_deref(), Some(V1));
        assert_ne!(text(&doc), V1);
    }

    #[test]
    fn importing_unchanged_bytes_is_free_and_keeps_the_parent_version() {
        let (doc, v1) = open(V1);
        let again = import(&doc, &v1, V1);
        assert_eq!(again, v1, "no operations, so no new version");
        assert_eq!(text(&doc), V1);
    }
}
