//! Staging seen hunks back into the index — `git add -p` driven by the ticks.
//!
//! Only a WORKTREE review can be written back: its new side is a file, not a
//! git object. Only fully seen hunks reach the index. A file whose bytes moved
//! since the load is skipped whole, never half-staged: a tick records what was
//! read, and the index must not carry bytes nobody read.

use std::{collections::HashSet, path::Path};

use concats_sync::hash_object;
use concats_text::{fnv1a, fnv1a_seed};
use gix::{ObjectId, Repository, bstr::BStr};

use crate::{
    Error,
    load::{open_repo, worktree_status},
};

/// One file of a WORKTREE load, as `stage_seen` needs it: the exact endpoint
/// oids the review was computed against, and each hunk's line ranges.
#[derive(Clone)]
pub struct StageFile {
    pub path: String,
    /// Rename source — the index entry staging removes.
    pub from: Option<String>,
    /// Old-side blob (index or base content). None = untracked.
    pub old: Option<ObjectId>,
    /// The worktree content's blob hash at load time. None = deleted.
    pub new: Option<ObjectId>,
    /// Per hunk: (1-based old start, dels, 1-based new start, adds) — the
    /// same numbers as `Hunk`, which is also where the seen keys come from.
    pub hunks: Vec<(u32, usize, u32, usize)>,
}

#[derive(Default)]
pub struct StageReport {
    /// Files whose index entry changed.
    pub files: usize,
    /// Seen hunks staged across them.
    pub hunks: usize,
    /// "path — why" for everything left alone.
    pub skipped: Vec<String>,
}

/// Stage the fully seen hunks. Content-addressing is the guard: a file is
/// touched only if its worktree bytes still hash to the reviewed blob and its
/// index entry is still the old side the hunks were diffed against. A partly
/// seen file is rebuilt from the old blob plus its seen hunks and gets zeroed
/// stat fields, like `git apply --cached`, so git re-verifies it by content.
pub fn stage_seen(
    workdir: &Path,
    files: &[StageFile],
    seen: &HashSet<(ObjectId, u32)>,
) -> Result<StageReport, Error> {
    let repo = open_repo(workdir)?;
    let mut index = repo.open_index().unwrap_or_else(|_| {
        gix::index::File::from_state(
            gix::index::State::new(repo.object_hash()),
            repo.index_path(),
        )
    });
    let mut rep = StageReport::default();
    let mut touched = false;

    for f in files {
        // The unit is the hunk, like its tick box: it stages when every one of
        // its changed lines is seen, and not at all otherwise. Files do stage
        // partially — any subset of their hunks — see `blend`.
        let take: Vec<bool> = f
            .hunks
            .iter()
            .map(|&(old_start, dels, new_start, adds)| {
                dels + adds > 0
                    && (0..dels as u32).all(|l| {
                        f.old
                            .is_some_and(|o| seen.contains(&(o, old_start - 1 + l)))
                    })
                    && (0..adds as u32).all(|l| {
                        f.new
                            .is_some_and(|n| seen.contains(&(n, new_start - 1 + l)))
                    })
            })
            .collect();
        let picked = take.iter().filter(|t| **t).count();
        if picked == 0 {
            continue;
        }

        let entry_path = f.from.as_deref().unwrap_or(&f.path);
        let entry = index.entry_by_path(BStr::new(entry_path));
        let entry_oid = entry.map(|e| e.id);
        let entry_mode = entry
            .map(|e| e.mode)
            .unwrap_or(gix::index::entry::Mode::FILE);

        match f.new {
            None => {
                if entry_oid != f.old {
                    rep.skipped
                        .push(format!("{} — index changed since load", f.path));
                    continue;
                }
                unstage_file(&mut index, entry_path);
            }
            Some(new_oid) => {
                let disk = std::fs::read(workdir.join(&f.path)).ok();
                if disk.as_deref().map(hash_object) != Some(new_oid) {
                    rep.skipped
                        .push(format!("{} — changed on disk since load", f.path));
                    continue;
                }
                if f.old.is_some() && entry_oid != f.old {
                    rep.skipped
                        .push(format!("{} — index changed since load", f.path));
                    continue;
                }
                if picked == f.hunks.len() {
                    // Everything seen — the entry becomes the worktree file
                    // as-is, with fresh stat, exactly like `git add`.
                    if let Some(from) = &f.from {
                        unstage_file(&mut index, from);
                    }
                    stage_file(&repo, workdir, &mut index, &f.path)?;
                } else {
                    let Some(old_oid) = f.old else {
                        rep.skipped.push(format!(
                            "{} — an untracked file stages whole; mark all its hunks seen",
                            f.path
                        ));
                        continue;
                    };
                    let old_bytes = repo
                        .find_blob(old_oid)
                        .map_err(|e| Error::git("blob", e))?
                        .take_data();
                    let blended =
                        blend(&old_bytes, disk.as_deref().unwrap_or(&[]), &f.hunks, &take);
                    let oid = repo
                        .write_blob(&blended)
                        .map_err(|e| Error::git("write blob", e))?
                        .detach();
                    if let Some(from) = &f.from {
                        unstage_file(&mut index, from);
                    }
                    // Zeroed stat (as `git apply --cached` leaves it), so git
                    // re-verifies the entry by content.
                    let stat = gix::index::entry::Stat {
                        size: blended.len() as u32,
                        ..Default::default()
                    };
                    match index.entry_index_by_path(BStr::new(f.path.as_str())) {
                        Ok(i) => {
                            let e = &mut index.entries_mut()[i];
                            e.stat = stat;
                            e.id = oid;
                            e.mode = entry_mode;
                        }
                        Err(_) => {
                            index.dangerously_push_entry(
                                stat,
                                oid,
                                gix::index::entry::Flags::empty(),
                                entry_mode,
                                BStr::new(f.path.as_str()),
                            );
                            index.sort_entries();
                        }
                    }
                }
            }
        }
        rep.files += 1;
        rep.hunks += picked;
        touched = true;
    }

    if touched {
        index
            .write(gix::index::write::Options::default())
            .map_err(|e| Error::git("write index", e))?;
    }
    Ok(rep)
}

/// Rebuild a file's index content from the old side plus the taken hunks.
/// Byte-exact: lines keep their newlines (imara's tokens carry them too), so
/// untaken parts stay the old bytes and taken parts are the worktree's.
fn blend(old: &[u8], new: &[u8], hunks: &[(u32, usize, u32, usize)], take: &[bool]) -> Vec<u8> {
    let old_l: Vec<&[u8]> = old.split_inclusive(|c| *c == b'\n').collect();
    let new_l: Vec<&[u8]> = new.split_inclusive(|c| *c == b'\n').collect();
    let mut out = Vec::with_capacity(old.len().max(new.len()));
    let mut at = 0usize; // 0-based cursor into old_l; untaken hunks just flow past
    for (i, &(old_start, dels, new_start, adds)) in hunks.iter().enumerate() {
        if !take[i] {
            continue;
        }
        while at < old_start as usize - 1 {
            out.extend_from_slice(old_l[at]);
            at += 1;
        }
        for l in 0..adds {
            out.extend_from_slice(new_l[new_start as usize - 1 + l]);
        }
        at += dels;
    }
    while at < old_l.len() {
        out.extend_from_slice(old_l[at]);
        at += 1;
    }
    out
}

/// A cheap staleness probe for a WORKTREE review, polled by the GUI: the
/// status entries with each file's (mtime, len), plus the index file's own
/// stamp. Any edit, add, delete, or stage moves it. Steady state is a stat
/// walk — content is only read for stat-suspect files (the ones already
/// modified, i.e. the review's own file set). 0 = unreadable repo.
pub fn worktree_fingerprint(workdir: &Path) -> u64 {
    let Ok(repo) = gix::open(workdir) else {
        return 0;
    };
    let Ok(mut entries) = worktree_status(&repo) else {
        return 0;
    };
    // Seeded from the empty hash so the FNV offset basis has one home.
    let mut fp: u64 = fnv1a(b"");
    if let Ok(m) = std::fs::metadata(repo.git_dir().join("index")) {
        fp = stamp(fp, &m);
    }
    entries.sort();
    for (path, status) in entries {
        fp = fnv1a_seed(fp, path.as_bytes());
        fp = fnv1a_seed(fp, &[status]);
        if let Ok(m) = std::fs::metadata(workdir.join(&path)) {
            fp = stamp(fp, &m);
        }
    }
    fp
}

fn stamp(fp: u64, m: &std::fs::Metadata) -> u64 {
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    fnv1a_seed(fnv1a_seed(fp, &mtime.to_le_bytes()), &m.len().to_le_bytes())
}

/// `git add <path>`: write the worktree bytes as a blob and point the index
/// entry at it, with fresh stat.
pub(crate) fn stage_file(
    repo: &Repository,
    workdir: &Path,
    index: &mut gix::index::File,
    path: &str,
) -> Result<(), Error> {
    let file = workdir.join(path);
    let io = |source| Error::Io {
        path: file.clone(),
        source,
    };
    let bytes = std::fs::read(&file).map_err(io)?;
    let oid = repo
        .write_blob(&bytes)
        .map_err(|e| Error::git("write blob", e))?
        .detach();
    let meta = gix::index::fs::Metadata::from_path_no_follow(&file).map_err(io)?;
    let stat = gix::index::entry::Stat::from_fs(&meta).map_err(|e| Error::git("stat", e))?;
    let mode = if meta.is_executable() {
        gix::index::entry::Mode::FILE_EXECUTABLE
    } else {
        gix::index::entry::Mode::FILE
    };
    match index.entry_index_by_path(BStr::new(path)) {
        Ok(i) => {
            let e = &mut index.entries_mut()[i];
            e.stat = stat;
            e.id = oid;
            e.mode = mode;
        }
        Err(_) => {
            index.dangerously_push_entry(
                stat,
                oid,
                gix::index::entry::Flags::empty(),
                mode,
                BStr::new(path),
            );
            index.sort_entries();
        }
    }
    Ok(())
}

/// Drop `path`'s entry from the index, if present.
fn unstage_file(index: &mut gix::index::File, path: &str) {
    index.remove_entries(|_, entry_path, _| entry_path == BStr::new(path));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        fixture::{add, hunk_seen_keys, init_repo},
        load::{INDEX_REV, WORKTREE_REV, load},
    };

    #[test]
    fn stage_seen_stages_only_the_ticked_hunks() {
        let (_tmp, root) = init_repo();
        let orig = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n";
        add(&root, "a.txt", orig);
        let edited = "L1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nL10\n";
        std::fs::write(root.join("a.txt"), edited).unwrap();

        let loaded = load(&root, INDEX_REV, WORKTREE_REV).unwrap();
        let f = loaded.stage.iter().find(|s| s.path == "a.txt").unwrap();
        assert_eq!(f.hunks.len(), 2, "two edits far apart = two hunks");

        // Tick only the first hunk.
        let seen: HashSet<(ObjectId, u32)> = hunk_seen_keys(f, 0).into_iter().collect();
        let rep = stage_seen(&root, &loaded.stage, &seen).unwrap();
        assert_eq!((rep.files, rep.hunks), (1, 1));
        assert!(rep.skipped.is_empty());

        // The index now holds old content + hunk 1 only, as a real blob.
        let repo = gix::open(&root).unwrap();
        let index = repo.open_index().unwrap();
        let e = index.entry_by_path(BStr::new("a.txt")).unwrap();
        let staged = repo.find_blob(e.id).unwrap().take_data();
        assert_eq!(
            String::from_utf8(staged).unwrap(),
            "L1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n"
        );
        // Smudged stat: git must re-verify this entry by content.
        assert_eq!((e.stat.mtime.secs, e.stat.size as usize), (0, orig.len()));

        // Reloading the worktree diff leaves exactly the unticked hunk.
        let reloaded = load(&root, INDEX_REV, WORKTREE_REV).unwrap();
        let f2 = reloaded.stage.iter().find(|s| s.path == "a.txt").unwrap();
        assert_eq!(f2.hunks.len(), 1);

        // Tick the rest: the file stages whole, with fresh stat.
        let seen: HashSet<(ObjectId, u32)> = hunk_seen_keys(f2, 0).into_iter().collect();
        let rep = stage_seen(&root, &reloaded.stage, &seen).unwrap();
        assert_eq!((rep.files, rep.hunks), (1, 1));
        let repo = gix::open(&root).unwrap();
        let index = repo.open_index().unwrap();
        let e = index.entry_by_path(BStr::new("a.txt")).unwrap();
        assert_eq!(
            String::from_utf8(repo.find_blob(e.id).unwrap().take_data()).unwrap(),
            edited
        );
        assert_ne!(e.stat.mtime.secs, 0);
        // Nothing left to review.
        let done = load(&root, INDEX_REV, WORKTREE_REV).unwrap();
        assert!(done.files.is_empty());
    }

    #[test]
    fn stage_seen_handles_untracked_and_deleted() {
        let (_tmp, root) = init_repo();
        add(&root, "gone.txt", "bye\n");
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::write(root.join("new.txt"), "hi\nthere\n").unwrap();

        let loaded = load(&root, INDEX_REV, WORKTREE_REV).unwrap();
        let mut seen: HashSet<(ObjectId, u32)> = HashSet::new();
        for f in &loaded.stage {
            for h in 0..f.hunks.len() {
                seen.extend(hunk_seen_keys(f, h));
            }
        }
        let rep = stage_seen(&root, &loaded.stage, &seen).unwrap();
        assert_eq!(rep.files, 2);
        assert!(rep.skipped.is_empty());

        let repo = gix::open(&root).unwrap();
        let index = repo.open_index().unwrap();
        assert!(index.entry_by_path(BStr::new("gone.txt")).is_none());
        assert!(index.entry_by_path(BStr::new("new.txt")).is_some());
        // The staged-everything worktree diff is empty.
        assert!(
            load(&root, INDEX_REV, WORKTREE_REV)
                .unwrap()
                .files
                .is_empty()
        );
    }

    #[test]
    fn stage_seen_refuses_stale_content() {
        let (_tmp, root) = init_repo();
        add(&root, "a.txt", "one\n");
        std::fs::write(root.join("a.txt"), "two\n").unwrap();

        let loaded = load(&root, INDEX_REV, WORKTREE_REV).unwrap();
        let f = loaded.stage.iter().find(|s| s.path == "a.txt").unwrap();
        let seen: HashSet<(ObjectId, u32)> = hunk_seen_keys(f, 0).into_iter().collect();

        // The file moves on after the review snapshot…
        std::fs::write(root.join("a.txt"), "three\n").unwrap();
        let rep = stage_seen(&root, &loaded.stage, &seen).unwrap();
        // …so nothing is staged, loudly.
        assert_eq!((rep.files, rep.hunks), (0, 0));
        assert_eq!(rep.skipped.len(), 1);
        assert!(rep.skipped[0].contains("changed on disk"));
        let repo = gix::open(&root).unwrap();
        let index = repo.open_index().unwrap();
        let e = index.entry_by_path(BStr::new("a.txt")).unwrap();
        assert_eq!(e.id, hash_object(b"one\n"));
    }

    #[test]
    fn blend_reconstructs_partial_content() {
        let old = b"a\nb\nc\n";
        let new = b"A\nb\nC\n";
        // Two single-line replacements.
        let hunks = [(1u32, 1usize, 1u32, 1usize), (3, 1, 3, 1)];
        assert_eq!(blend(old, new, &hunks, &[true, false]), b"A\nb\nc\n");
        assert_eq!(blend(old, new, &hunks, &[false, true]), b"a\nb\nC\n");
        assert_eq!(blend(old, new, &hunks, &[true, true]), new);
        assert_eq!(blend(old, new, &hunks, &[false, false]), old);

        // Pure insertion at EOF, and a last line without trailing newline.
        let old = b"a\nb";
        let new = b"a\nb\nc";
        // "b" (no newline) becomes "b\n" + "c": one hunk replacing the last line.
        let hunks = [(2u32, 1usize, 2u32, 2usize)];
        assert_eq!(blend(old, new, &hunks, &[true]), new);
        assert_eq!(blend(old, new, &hunks, &[false]), old);
    }

    #[test]
    fn worktree_fingerprint_moves_on_edits_and_staging() {
        let (_tmp, root) = init_repo();
        add(&root, "a.txt", "one\n");
        let fp0 = worktree_fingerprint(&root);
        assert_ne!(fp0, 0);

        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        let fp1 = worktree_fingerprint(&root);
        assert_ne!(fp1, fp0);

        // Staging moves it too — the poll reloads after "stage seen hunks".
        add(&root, "a.txt", "two\n");
        let fp2 = worktree_fingerprint(&root);
        assert_ne!(fp2, fp1);
    }
}
