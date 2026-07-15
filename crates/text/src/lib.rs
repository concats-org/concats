//! Line tables, the line diff, and the stable hash — one implementation each,
//! for every crate above.
//!
//! One implementation because the results are stored and compared later: a
//! line table sits on every blob, the diff decides which CRDT operations get
//! minted, and the hash is persisted and compared across processes. A second
//! implementation of any of them would be a second answer to the same
//! question — a comment found in one process and not in the next.
//!
//! Nothing here knows about git, documents, or screens.

// NOTE: a line table is `u32` throughout the workspace — every blob keeps one —
// so `usize -> u32` here is the format rather than an oversight. A file where it
// would truncate is four gigabytes of source.
#![allow(clippy::cast_possible_truncation)]

use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput};

/// Byte offset of each line start, with a trailing sentinel at `text.len()` so
/// a line's range is uniform for the last line as well.
///
/// `u32` because this is also what every blob keeps, and one line table beats
/// two that can disagree about the sentinel.
#[must_use]
pub fn line_starts(text: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    starts.extend(
        text.bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| i as u32 + 1),
    );
    // Seeded with 0, so `last` is always there; the sentinel is only missing
    // when the text does not already end on a newline.
    if starts.last().copied().unwrap_or_default() as usize != text.len() {
        starts.push(text.len() as u32);
    }
    starts
}

/// The 0-based line `byte` falls on, given that text's [`line_starts`].
///
/// A byte exactly on a line start is on that line, and one past the end of the
/// text is on the empty line the trailing newline opened.
#[must_use]
pub fn line_of(starts: &[u32], byte: usize) -> usize {
    match starts.binary_search(&(byte as u32)) {
        Ok(line) => line,
        Err(line) => line - 1,
    }
}

/// Narrow a replacement to the part that actually differs.
///
/// The diff is line-granular, so a line edited in place arrives as "delete this
/// whole line, insert that whole line", and that deletes every cursor anchored
/// into it. A comment on a heading would detach the moment someone corrected a
/// word in it — the conversation would be lost instead of resolved.
///
/// Trimming the common prefix and suffix turns that into an insert, or a small
/// replacement, inside the line, which anchors ride. It also mints fewer
/// operations and gives an incremental reparse less to redo. It is
/// deterministic, so it is part of the format like the hunk order: both sides
/// must narrow identically.
fn shrink(range: Range<usize>, old: &str, ins: &str) -> (Range<usize>, String) {
    let del = &old[range.clone()];
    let prefix = del
        .chars()
        .zip(ins.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();
    // Measured over what the prefix left, so the two never overlap.
    let suffix = del[prefix..]
        .chars()
        .rev()
        .zip(ins[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();
    (
        range.start + prefix..range.end - suffix,
        ins[prefix..ins.len() - suffix].to_string(),
    )
}

/// The byte-range replacements taking `old` to `new`, highest offset first.
///
/// The order is part of the wire format: applying the same hunk list front to
/// back mints different CRDT operation ids, and two machines that disagree
/// about it end up with histories that cannot be reconciled. Back to front also
/// means no hunk shifts the offsets of the next one, which applying several
/// edits to one syntax tree needs as well.
#[must_use]
pub fn replacements(old: &str, new: &str) -> Vec<(Range<usize>, String)> {
    let input = InternedInput::new(old, new);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    let (old_starts, new_starts) = (line_starts(old), line_starts(new));
    let at = |starts: &[u32], line: u32| starts[line as usize] as usize;
    let mut out: Vec<(Range<usize>, String)> = diff
        .hunks()
        .map(|h| {
            let del = at(&old_starts, h.before.start)..at(&old_starts, h.before.end);
            let ins = &new[at(&new_starts, h.after.start)..at(&new_starts, h.after.end)];
            shrink(del, old, ins)
        })
        .collect();
    out.reverse();
    out
}

/// FNV-1a, the workspace's stable hash.
///
/// Not `DefaultHasher`: these hashes are persisted and compared across
/// processes and machines, and `DefaultHasher`'s output is not promised to stay
/// stable across Rust releases.
#[must_use]
pub fn fnv1a(bytes: &[u8]) -> u64 {
    fnv1a_seed(0xcbf2_9ce4_8422_2325, bytes)
}

/// The chaining form, so a caller can fold several fields into one hash.
#[must_use]
pub fn fnv1a_seed(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_table_ends_on_one_sentinel_however_the_text_ends() {
        assert_eq!(line_starts("a\nb\n"), vec![0, 2, 4]);
        assert_eq!(line_starts("a\nb"), vec![0, 2, 3]);
        assert_eq!(line_starts(""), vec![0]);
    }

    #[test]
    fn a_byte_on_a_line_start_is_on_that_line() {
        let starts = line_starts("a\nb\n");
        assert_eq!(line_of(&starts, 0), 0);
        assert_eq!(line_of(&starts, 1), 0);
        assert_eq!(line_of(&starts, 2), 1);
        // Past the last newline: the empty line it opened, not one beyond the
        // table — a duplicate sentinel here would name a line nobody has.
        assert_eq!(line_of(&starts, 4), 2);
    }

    /// A line edited in place must come back as a change inside the line, not
    /// as a delete and reinsert of the whole line. The latter takes every
    /// cursor anchored into it; that is how a comment on a heading disappears
    /// the moment someone corrects a word in it.
    #[test]
    fn editing_within_a_line_is_a_narrow_change() {
        assert_eq!(
            replacements(
                "# Intro\n## Getting Started\nsome prose\n",
                "# Intro\n## Getting Started, Quickly\nsome prose\n"
            ),
            vec![(26..26, ", Quickly".to_string())],
            "a pure insert inside the line, not a line replacement"
        );
    }

    #[test]
    fn shrinking_keeps_a_replacement_that_shares_no_edges_whole() {
        // Nothing in common, so there is nothing to narrow and the hunk stands.
        assert_eq!(
            replacements("aaa\n", "bbb\n"),
            vec![(0..3, "bbb".to_string())],
            "only the shared trailing newline comes off"
        );
    }

    #[test]
    fn shrinking_is_safe_across_multibyte_characters() {
        let old = "let s = \"héllo wörld\";\n";
        let new = "let s = \"héllo, wörld\";\n";
        let (range, ins) = replacements(old, new).pop().expect("one hunk");
        assert!(old.is_char_boundary(range.start) && old.is_char_boundary(range.end));
        assert_eq!(ins, ",");
    }

    #[test]
    fn hunks_come_back_highest_offset_first() {
        let ranges: Vec<_> = replacements("a\nb\nc\n", "A\nb\nC\n")
            .into_iter()
            .map(|(range, _)| range.start)
            .collect();
        assert!(
            ranges.windows(2).all(|w| w[0] > w[1]),
            "back to front, so no hunk shifts the next one: {ranges:?}"
        );
    }

    /// The hash is persisted, so its value is part of the format. Pinned to a
    /// literal rather than to a second implementation, which would drift with it.
    #[test]
    fn the_stable_hash_stays_stable() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"concats"), 0x3f09_03f6_bdbf_7b2a);
        assert_eq!(
            fnv1a(b"concats"),
            fnv1a_seed(fnv1a(b"con"), b"cats"),
            "chaining folds fields into the same hash as one call"
        );
    }
}
