//! git -> blobs + reference rows, with per-stage timing.
//!
//! `merge_base` + `diff_commits` give three-dot (`base...head`) semantics;
//! imara's histogram diff gives the line ops.
//!
//! NOTE: no highlighting happens here. It is the expensive part of a cold load,
//! and a reviewer opens a handful of the changed files, so blobs highlight
//! lazily on first draw — the loader leaves `Blob::spans` empty.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Instant,
};

use concats_languages::lang_for_ext;
use concats_sync::hash_object;
use concats_text::fnv1a;
use gix::{ObjectId, Repository};
use imara_diff::{Algorithm, Diff, InternedInput};

use crate::{Blob, Error, FileChange, Hunk, LineKind, LoadStats, Row, Side, stage::StageFile};

/// Unchanged lines kept either side of a change before collapsing.
const CONTEXT: usize = 3;

/// Sentinel revisions: `INDEX...WORKTREE` reviews the unstaged changes,
/// `<commit>...WORKTREE` (say `HEAD...WORKTREE`) everything uncommitted.
/// Neither endpoint is a commit. The new side is the worktree files themselves,
/// hashed with git's blob hash but never written, so seen state and comments
/// key on the oids the content will have once it is staged and committed —
/// review state survives the whole way.
pub const INDEX_REV: &str = "INDEX";
pub const WORKTREE_REV: &str = "WORKTREE";

pub struct Loaded {
    /// The resolved base: the merge base for a commit range, the base commit
    /// for a `<commit>...WORKTREE` load, `None` for `INDEX...WORKTREE`.
    pub merge_base: Option<ObjectId>,
    /// The resolved head commit; `None` for a WORKTREE load.
    pub head: Option<ObjectId>,
    /// The repo's common `.git` directory — the identity of the review store.
    ///
    /// The common dir rather than this worktree's own git dir, because
    /// everything keyed by it is keyed by content: a comment names a blob, a
    /// seen tick names a blob and a line, a guide names two commit oids. None
    /// of that belongs to one checkout. Linked worktrees share an object
    /// database, so a comment left on a blob in one checkout has to show up
    /// when the same blob appears in another; that is the premise of anchoring
    /// to content, and it breaks if each worktree gets its own database.
    ///
    /// For a main worktree the two paths are the same, so this only differs
    /// inside a `git worktree`. The per-worktree git dir is still used where it
    /// belongs — the index fingerprint the reload watches.
    pub git_dir: PathBuf,
    /// Set on a WORKTREE load: the working directory. `Some` is what marks a
    /// worktree review, and it is where "stage seen hunks" writes back to.
    pub workdir: Option<PathBuf>,
    pub files: Vec<FileChange>,
    pub blobs: Vec<Blob>,
    pub stats: LoadStats,
    /// WORKTREE loads only: the per-file data `stage_seen` needs.
    pub stage: Vec<StageFile>,
    /// Every blob path of the tree at the head, sorted — what the file browser
    /// lists, and the reason it can reach a file this range never touched. A
    /// WORKTREE load has no head commit, so this is the working tree instead.
    pub tree: Vec<String>,
}

/// Walk up from `start` until a `.git` shows up, so the app works from any cwd.
pub fn discover(start: &Path) -> Option<PathBuf> {
    let mut p = start.canonicalize().ok()?;
    loop {
        if p.join(".git").exists() {
            return Some(p);
        }
        if !p.pop() {
            return None;
        }
    }
}

pub fn load(repo_path: &Path, base_rev: &str, head_rev: &str) -> Result<Loaded, Error> {
    if head_rev.trim() == WORKTREE_REV {
        return load_worktree(repo_path, base_rev);
    }
    if base_rev.trim() == INDEX_REV || base_rev.trim() == WORKTREE_REV {
        return Err(Error::WorktreeOnly {
            rev: base_rev.trim().to_string(),
        });
    }

    let t_total = Instant::now();
    let mut st = LoadStats::default();

    let root = discover(repo_path).ok_or_else(|| Error::NoRepository(repo_path.to_path_buf()))?;

    let t = Instant::now();
    let repo = open_repo(&root)?;
    let head = resolve(&repo, head_rev)?;
    let base = resolve(&repo, base_rev)?;
    let merge_base = match repo.merge_base(base, head) {
        Ok(id) => id.detach(),
        Err(gix::repository::merge_base::Error::NotFound { .. }) => {
            return Err(Error::NoMergeBase {
                base: base_rev.to_string(),
                head: head_rev.to_string(),
            });
        }
        Err(e) => return Err(Error::git("merge_base", e)),
    };
    let changes = diff_commits(&repo, merge_base, head)?;
    st.git_ms += t.elapsed().as_secs_f64() * 1000.0;

    // --- rename detection ---------------------------------------------------
    // diff_commits reports a moved file as Add + Delete, which in a review
    // reads as +N/-N of pure noise. Git's diffcore-rename runs two passes; so
    // do we.
    let t = Instant::now();
    let changes = detect_renames(&repo, changes, None, &mut st)?;
    st.rename_ms += t.elapsed().as_secs_f64() * 1000.0;

    let (files, blobs) = lower(&repo, &changes, None, &mut st)?;

    let head_tree = repo
        .find_commit(head)
        .map_err(|e| Error::git("commit", e))?
        .tree_id()
        .map_err(|e| Error::git("tree", e))?
        .detach();
    let mut tree: Vec<String> = flatten_tree(&repo, head_tree)?.into_keys().collect();
    tree.sort();

    st.files = files.len();
    st.total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

    Ok(Loaded {
        merge_base: Some(merge_base),
        head: Some(head),
        git_dir: repo.common_dir().to_path_buf(),
        workdir: None,
        files,
        blobs,
        stats: st,
        stage: Vec::new(),
        tree,
    })
}

/// Load the worktree against the index (`INDEX...WORKTREE`) or a commit
/// (`HEAD...WORKTREE`). Status decides which paths changed; its ignore rules
/// keep node_modules and friends out of the untracked scan. The worktree bytes
/// go into an overlay keyed by their real git blob hash, so everything
/// downstream — lowering, seen state, comments — works on content-addressed
/// oids, the same as in a commit review.
fn load_worktree(repo_path: &Path, base_rev: &str) -> Result<Loaded, Error> {
    let t_total = Instant::now();
    let mut st = LoadStats::default();

    let root = discover(repo_path).ok_or_else(|| Error::NoRepository(repo_path.to_path_buf()))?;

    let t = Instant::now();
    let repo = open_repo(&root)?;
    // A repo before its first `git add` has no index file — that is an empty
    // index, not an error.
    let index = repo.index_or_empty().map_err(|e| Error::git("index", e))?;
    let index_of: HashMap<String, ObjectId> = index
        .entries()
        .iter()
        .filter(|e| e.stage() == gix::index::entry::Stage::Unconflicted)
        .map(|e| (e.path(&index).to_string(), e.id))
        .collect();

    // The old side: what the hunks' line numbers and del rows are measured
    // against — the index's blobs, or the base commit's tree.
    let base_is_index = base_rev.trim() == INDEX_REV;
    let mut base_commit = None;
    let base_of: HashMap<String, ObjectId> = if base_is_index {
        index_of.clone()
    } else {
        let oid = resolve(&repo, base_rev)?;
        let tree = repo
            .find_commit(oid)
            .map_err(|e| Error::git("commit", e))?
            .tree_id()
            .map_err(|e| Error::git("tree", e))?
            .detach();
        base_commit = Some(oid);
        flatten_tree(&repo, tree)?
    };

    // Status decides which paths changed: the worktree-vs-index scan (gix
    // handles ignore rules and the racy-git guard), plus, for a commit base,
    // whatever differs between the base tree and the index — the staged
    // changes.
    let mut paths: Vec<String> = worktree_status(&repo)?
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    if !base_is_index {
        paths.extend(
            base_of
                .keys()
                .chain(index_of.keys())
                .filter(|path| base_of.get(*path) != index_of.get(*path))
                .cloned(),
        );
    }
    paths.sort();
    paths.dedup();

    // Unlike a commit load, the new side here is arbitrary worktree content
    // that nothing has vetted. So check size and binaryness before reading,
    // hashing or cloning: a daemon's half-gigabyte scratch file should cost one
    // stat, not seconds of SHA1 per load. (Binary blobs from the object
    // database are still caught later, in the lowerer.)
    const MAX_WORKTREE_BYTES: u64 = 16 << 20;
    let mut overlay: HashMap<ObjectId, Vec<u8>> = HashMap::new();
    let mut changes: Vec<Change> = Vec::new();
    for path in paths {
        let old = base_of.get(&path).copied();
        let file = root.join(&path);
        let new = match std::fs::symlink_metadata(&file) {
            // Missing (or a broken symlink): deleted.
            Err(_) => None,
            // A symlink's target is outside our control — never read through it
            // into the review (a hostile repo could point a "changed" path at
            // ~/.ssh/…). Skip it, like a binary.
            Ok(m) if m.file_type().is_symlink() => {
                st.skipped_binary += 1;
                continue;
            }
            Ok(m) if m.len() > MAX_WORKTREE_BYTES => {
                st.skipped_binary += 1;
                continue;
            }
            Ok(_) => match std::fs::read(&file) {
                Ok(bytes) => {
                    if bytes.contains(&0) {
                        st.skipped_binary += 1;
                        continue;
                    }
                    Some((hash_object(&bytes), bytes))
                }
                Err(_) => None,
            },
        };
        match (old, new) {
            (None, None) => {}
            (Some(oid), None) => changes.push(Change::Deleted { path, oid }),
            (None, Some((oid, bytes))) => {
                overlay.insert(oid, bytes);
                changes.push(Change::Added { path, oid });
            }
            (Some(old_oid), Some((new_oid, bytes))) => {
                // Stat noise (touched but identical) is not a change.
                if old_oid == new_oid {
                    continue;
                }
                overlay.insert(new_oid, bytes);
                changes.push(Change::Modified {
                    path,
                    old_oid,
                    new_oid,
                });
            }
        }
    }
    st.git_ms += t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let changes = detect_renames(&repo, changes, Some(&overlay), &mut st)?;
    st.rename_ms += t.elapsed().as_secs_f64() * 1000.0;

    let (files, mut blobs) = lower(&repo, &changes, Some(&overlay), &mut st)?;

    // Every blob whose bytes came off the working tree names the file it came
    // from; that is what makes it editable. A base or deleted-side blob stays
    // read-only for the same reason: the only place its bytes exist is the
    // object database.
    let from_worktree: HashMap<ObjectId, &Path> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Added { path, oid } => Some((*oid, Path::new(path))),
            Change::Modified { path, new_oid, .. } => Some((*new_oid, Path::new(path))),
            Change::Renamed { path, new_oid, .. } => Some((*new_oid, Path::new(path))),
            Change::Deleted { .. } => None,
        })
        .collect();
    for blob in &mut blobs {
        if let Some(path) = from_worktree.get(&blob.oid) {
            blob.origin = Some(root.join(path));
        }
    }

    // What "stage seen hunks" needs to rebuild index content per file: the
    // exact endpoint oids of each file, and each hunk's line ranges. Files
    // the lowerer dropped (binary) have no FileChange and stay unstageable.
    let file_of: HashMap<&str, &FileChange> = files.iter().map(|f| (f.path.as_str(), f)).collect();
    let stage = changes
        .iter()
        .filter_map(|c| {
            let (path, from, old, new) = match c {
                Change::Added { path, oid } => (path, None, None, Some(*oid)),
                Change::Deleted { path, oid } => (path, None, Some(*oid), None),
                Change::Modified {
                    path,
                    old_oid,
                    new_oid,
                } => (path, None, Some(*old_oid), Some(*new_oid)),
                Change::Renamed {
                    from,
                    path,
                    old_oid,
                    new_oid,
                    ..
                } => (path, Some(from.clone()), Some(*old_oid), Some(*new_oid)),
            };
            let fc = file_of.get(path.as_str())?;
            Some(StageFile {
                path: path.clone(),
                from,
                old,
                new,
                hunks: fc
                    .hunks
                    .iter()
                    .map(|h| (h.old_start, h.dels, h.new_start, h.adds))
                    .collect(),
            })
        })
        .collect();

    // The working tree, from what this load already scanned: the index's
    // tracked paths, plus every path the diff adds (an untracked file is added
    // against the index), minus what it deletes or renames away.
    let gone: HashSet<&str> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Deleted { path, .. } => Some(path.as_str()),
            Change::Renamed { from, .. } => Some(from.as_str()),
            _ => None,
        })
        .collect();
    let mut tree: Vec<String> = index_of
        .keys()
        .cloned()
        .chain(changes.iter().filter_map(|c| match c {
            Change::Deleted { .. } => None,
            Change::Added { path, .. }
            | Change::Modified { path, .. }
            | Change::Renamed { path, .. } => Some(path.clone()),
        }))
        .filter(|p| !gone.contains(p.as_str()))
        .collect();
    tree.sort();
    tree.dedup();

    st.files = files.len();
    st.total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

    Ok(Loaded {
        merge_base: base_commit,
        head: None,
        git_dir: repo.common_dir().to_path_buf(),
        workdir: Some(root),
        files,
        blobs,
        stats: st,
        stage,
        tree,
    })
}

/// Lower a change list into FileChanges + blobs — the shared back half of
/// both loaders.
fn lower(
    repo: &Repository,
    changes: &[Change],
    overlay: Option<&HashMap<ObjectId, Vec<u8>>>,
    st: &mut LoadStats,
) -> Result<(Vec<FileChange>, Vec<Blob>), Error> {
    let mut files = Vec::new();
    let mut blobs: Vec<Blob> = Vec::new();
    // Dedup blobs across files by oid — content-addressed, so identical content
    // (a pure rename, a vendored file) is stored once.
    let mut blob_ix: HashMap<ObjectId, u32> = HashMap::new();
    // Hunk ids are unique across the whole review, not per file — an agent
    // references them globally.
    let mut next_hunk_id = 0usize;

    let mut low = FileLowerer {
        repo,
        blobs: &mut blobs,
        blob_ix: &mut blob_ix,
        next_hunk_id: &mut next_hunk_id,
        hunk_prefix: "h",
        overlay,
        st,
    };
    for ch in changes {
        let (from, similarity) = match ch {
            Change::Renamed {
                from, similarity, ..
            } => (Some(from.clone()), Some(*similarity)),
            _ => (None, None),
        };
        let id = format!("f{}", files.len());
        let path = ch.path().to_string();
        if let Some(fc) = low.file(id, path, from, similarity, ch.old_oid(), ch.new_oid())? {
            files.push(fc);
        }
    }
    Ok((files, blobs))
}

/// Document-wide lowering state: one blob table and one hunk counter, shared by
/// everything lowered into the same document. The review's files and the
/// Sessions tab's per-turn diffs both go through here, so blobs dedupe by oid
/// across the whole document and identical content renders identically.
pub struct FileLowerer<'a> {
    pub repo: &'a Repository,
    pub blobs: &'a mut Vec<Blob>,
    pub blob_ix: &'a mut HashMap<ObjectId, u32>,
    pub next_hunk_id: &'a mut usize,
    /// "h" for review hunks (the ids the manifest/linter hand to agents),
    /// "t" for session-turn hunks — distinct namespaces, no collisions.
    pub hunk_prefix: &'a str,
    /// WORKTREE loads: content for oids that exist nowhere in the object DB —
    /// the worktree files, keyed by their (unwritten) git blob hash.
    pub overlay: Option<&'a HashMap<ObjectId, Vec<u8>>>,
    pub st: &'a mut LoadStats,
}

impl FileLowerer<'_> {
    /// Diff `old_oid` → `new_oid` at `path` and lower the hunks into a
    /// self-contained, addressable `FileChange`. Each hunk carries its own
    /// context and a stable id, so an agent can select and reorder hunks by id
    /// without seeing the code inside them — or inventing any. Returns `None`
    /// for binary content (counted in the stats).
    pub fn file(
        &mut self,
        id: String,
        path: String,
        from: Option<String>,
        similarity: Option<u8>,
        old_oid: Option<ObjectId>,
        new_oid: Option<ObjectId>,
    ) -> Result<Option<FileChange>, Error> {
        let blobs = &mut *self.blobs;
        let blob_ix = &mut *self.blob_ix;
        let st = &mut *self.st;

        // A pure rename (identical content) has nothing to review. Collapse it
        // to a single row instead of emitting the file twice.
        if from.is_some() && old_oid == new_oid {
            return Ok(Some(FileChange {
                id,
                path,
                is_new: false,
                from,
                similarity,
                lang: lang_for_ext(""),
                adds: 0,
                dels: 0,
                hunks: Vec::new(),
                gap_after: None,
            }));
        }

        let t = Instant::now();
        let old_bytes = read(self.repo, self.overlay, old_oid)?;
        let new_bytes = read(self.repo, self.overlay, new_oid)?;
        st.git_ms += t.elapsed().as_secs_f64() * 1000.0;

        // NUL => binary. Gix has no binary detection here; this is ours.
        if old_bytes.contains(&0) || new_bytes.contains(&0) {
            st.skipped_binary += 1;
            return Ok(None);
        }
        st.bytes += old_bytes.len() + new_bytes.len();

        let ext = path.rsplit('.').next().unwrap_or("").to_string();
        let lang = lang_for_ext(&ext);

        let old_src = String::from_utf8_lossy(&old_bytes).into_owned();
        let new_src = String::from_utf8_lossy(&new_bytes).into_owned();

        // --- line diff: histogram, linear space ---
        let t = Instant::now();
        let input = InternedInput::new(old_src.as_str(), new_src.as_str());
        let mut diff = Diff::compute(Algorithm::Histogram, &input);
        // Slider adjustment + indent heuristics: moves hunk boundaries to where
        // a human would put them. This is most of why histogram diffs read better.
        diff.postprocess_lines(&input);
        let hunks: Vec<_> = diff.hunks().collect();
        st.diff_ms += t.elapsed().as_secs_f64() * 1000.0;

        let old_n = input.before.len();
        let new_n = input.after.len();

        // Intern each side's blob: the text is stored once, spans are filled
        // lazily.
        let mut intern = |oid: Option<ObjectId>, text: &str| -> Option<u32> {
            let oid = oid?;
            if let Some(&i) = blob_ix.get(&oid) {
                return Some(i);
            }
            let i = blobs.len() as u32;
            blobs.push(Blob::new(oid, ext.clone(), text.to_string()));
            blob_ix.insert(oid, i);
            Some(i)
        };
        let old_b = intern(old_oid, &old_src);
        let new_b = intern(new_oid, &new_src);

        let t = Instant::now();
        let (mut adds, mut dels) = (0usize, 0usize);
        let (mut old_at, mut new_at) = (0usize, 0usize);
        let mut out_hunks: Vec<Hunk> = Vec::new();

        let emit_ctx = |rows: &mut Vec<Row>, o: usize, n: usize| {
            if let Some(b) = new_b {
                rows.push(Row::Code {
                    kind: LineKind::Context,
                    old_no: Some(o as u32 + 1),
                    new_no: Some(n as u32 + 1),
                    blob: b,
                    line: n as u32,
                });
            }
        };

        for h in hunks.iter() {
            let hb = h.before.start as usize;
            let ha = h.after.start as usize;

            // Equal run before this hunk: keep `CONTEXT` lines, collapse the
            // rest.
            let gap = hb.saturating_sub(old_at);
            let lead = CONTEXT.min(gap);

            let mut rows: Vec<Row> = Vec::new();
            // The hunk's head row hosts the seen tick box. It is part of the
            // hunk's own rows, so every view that shows the hunk shows the box.
            // Each side is the run of changed lines as an inclusive `Side`; an
            // empty run (nothing changed on that side) is `None`.
            rows.push(Row::HunkBar {
                old: old_b
                    .filter(|_| h.before.end > h.before.start)
                    .map(|b| Side {
                        blob: b,
                        start: h.before.start,
                        end: h.before.end - 1,
                    }),
                new: new_b.filter(|_| h.after.end > h.after.start).map(|b| Side {
                    blob: b,
                    start: h.after.start,
                    end: h.after.end - 1,
                }),
            });
            for k in (gap - lead)..gap {
                emit_ctx(&mut rows, old_at + k, new_at + k);
            }

            let (mut h_adds, mut h_dels) = (0usize, 0usize);
            if let Some(b) = old_b {
                for l in h.before.clone() {
                    rows.push(Row::Code {
                        kind: LineKind::Del,
                        old_no: Some(l + 1),
                        new_no: None,
                        blob: b,
                        line: l,
                    });
                    h_dels += 1;
                }
            }
            if let Some(b) = new_b {
                for l in h.after.clone() {
                    rows.push(Row::Code {
                        kind: LineKind::Add,
                        old_no: None,
                        new_no: Some(l + 1),
                        blob: b,
                        line: l,
                    });
                    h_adds += 1;
                }
            }

            // Trailing context: the first `CONTEXT` lines of the following
            // equal run.
            let next_old = h.before.end as usize;
            let next_new = h.after.end as usize;
            let avail = old_n
                .saturating_sub(next_old)
                .min(new_n.saturating_sub(next_new));
            for k in 0..CONTEXT.min(avail) {
                emit_ctx(&mut rows, next_old + k, next_new + k);
            }

            // A one-line preview: enough for an agent to reason about the hunk
            // without being shipped the file.
            let preview = new_b
                .filter(|_| h_adds > 0)
                .map(|b| blobs[b as usize].line_text(ha).trim().to_string())
                .or_else(|| {
                    old_b.map(|b| {
                        blobs[b as usize]
                            .line_text(h.before.start as usize)
                            .trim()
                            .to_string()
                    })
                })
                .unwrap_or_default();

            adds += h_adds;
            dels += h_dels;
            out_hunks.push(Hunk {
                id: format!("{}{}", self.hunk_prefix, self.next_hunk_id),
                old_start: h.before.start + 1,
                new_start: h.after.start + 1,
                adds: h_adds,
                dels: h_dels,
                gap_before: collapsed_run(new_b, old_at, new_at, gap - lead),
                preview: preview.chars().take(90).collect(),
                rows,
            });
            *self.next_hunk_id += 1;

            old_at = next_old;
            new_at = next_new;
        }

        // Whatever unchanged tail is left after the last hunk's trailing
        // context. The tail starts past that context, so the same offset shifts
        // the collapsed run.
        let shown = if out_hunks.is_empty() { 0 } else { CONTEXT };
        let tail_gap = old_n
            .saturating_sub(old_at)
            .min(new_n.saturating_sub(new_at))
            .saturating_sub(shown);
        st.lower_ms += t.elapsed().as_secs_f64() * 1000.0;

        st.adds += adds;
        st.dels += dels;
        Ok(Some(FileChange {
            id,
            path,
            is_new: old_oid.is_none(),
            from,
            similarity,
            lang,
            adds,
            dels,
            hunks: out_hunks,
            gap_after: collapsed_run(new_b, old_at + shown, new_at + shown, tail_gap),
        }))
    }
}

/// The row standing in for a collapsed run of `count` unchanged lines, starting
/// at `old_start`/`new_start` (0-based, one per side). `None` for an empty run —
/// and for a file with no new side, which cannot have unchanged lines to collapse
/// in the first place.
fn collapsed_run(
    new_b: Option<u32>,
    old_start: usize,
    new_start: usize,
    count: usize,
) -> Option<Row> {
    Some(Row::Collapsed {
        blob: new_b.filter(|_| count > 0)?,
        old_start: old_start as u32,
        new_start: new_start as u32,
        count: count as u32,
    })
}

/// A tree change, after rename detection. Gix's tree change has no
/// `Renamed` variant; this is the shape a review actually needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Added {
        path: String,
        oid: ObjectId,
    },
    Deleted {
        path: String,
        oid: ObjectId,
    },
    Modified {
        path: String,
        old_oid: ObjectId,
        new_oid: ObjectId,
    },
    Renamed {
        from: String,
        path: String,
        old_oid: ObjectId,
        new_oid: ObjectId,
        /// 0..=100
        similarity: u8,
    },
}

impl Change {
    /// The path on the new side; for a rename, where the file went.
    pub fn path(&self) -> &str {
        match self {
            Change::Added { path, .. }
            | Change::Deleted { path, .. }
            | Change::Modified { path, .. }
            | Change::Renamed { path, .. } => path,
        }
    }

    /// The blob before, `None` when the path was created.
    pub fn old_oid(&self) -> Option<ObjectId> {
        match self {
            Change::Added { .. } => None,
            Change::Deleted { oid, .. } => Some(*oid),
            Change::Modified { old_oid, .. } | Change::Renamed { old_oid, .. } => Some(*old_oid),
        }
    }

    /// The blob after, `None` when the path was deleted.
    pub fn new_oid(&self) -> Option<ObjectId> {
        match self {
            Change::Deleted { .. } => None,
            Change::Added { oid, .. } => Some(*oid),
            Change::Modified { new_oid, .. } | Change::Renamed { new_oid, .. } => Some(*new_oid),
        }
    }
}

/// Below this, two files are unrelated. Git's default for `-M` is also 50%.
const RENAME_THRESHOLD: u8 = 50;
/// Bound the O(adds × dels) similarity matrix, as git's `diff.renameLimit` does.
const RENAME_LIMIT: usize = 1000;

/// Two passes, like git's diffcore-rename:
///
///   1. **Exact.** A delete and an add with the same blob oid are the same
///      content at a new path. Content-addressing makes this a hash lookup, no
///      similarity maths. It catches every pure move, which is most real
///      renames.
///
///   2. **Inexact.** For what is left, score each (delete, add) pair by line
///      similarity and take pairs above the threshold, best first. Git hashes
///      chunks ("spanhash"); we hash whole lines, which is simpler and close
///      enough for a review — a moved-and-edited file still scores well above
///      50%.
fn detect_renames(
    repo: &Repository,
    changes: Vec<Change>,
    overlay: Option<&HashMap<ObjectId, Vec<u8>>>,
    st: &mut LoadStats,
) -> Result<Vec<Change>, Error> {
    let mut out = Vec::new();
    let mut adds: Vec<(String, ObjectId)> = Vec::new();
    let mut dels: Vec<(String, ObjectId)> = Vec::new();

    for ch in changes {
        match ch {
            Change::Added { path, oid } => adds.push((path, oid)),
            Change::Deleted { path, oid } => dels.push((path, oid)),
            other => out.push(other),
        }
    }

    // --- pass 1: exact, by oid. Free. ---
    let mut del_by_oid: HashMap<ObjectId, Vec<usize>> = HashMap::new();
    for (i, (_, oid)) in dels.iter().enumerate() {
        del_by_oid.entry(*oid).or_default().push(i);
    }
    let mut del_taken = vec![false; dels.len()];
    let mut add_taken = vec![false; adds.len()];

    for (ai, (apath, aoid)) in adds.iter().enumerate() {
        if let Some(cands) = del_by_oid.get_mut(aoid) {
            while let Some(di) = cands.pop() {
                if del_taken[di] {
                    continue;
                }
                del_taken[di] = true;
                add_taken[ai] = true;
                out.push(Change::Renamed {
                    from: dels[di].0.clone(),
                    path: apath.clone(),
                    old_oid: dels[di].1,
                    new_oid: *aoid,
                    similarity: 100,
                });
                st.renames_exact += 1;
                break;
            }
        }
    }

    // --- pass 2: inexact, by line similarity ---
    let rem_a: Vec<usize> = (0..adds.len()).filter(|i| !add_taken[*i]).collect();
    let rem_d: Vec<usize> = (0..dels.len()).filter(|i| !del_taken[*i]).collect();

    if !rem_a.is_empty()
        && !rem_d.is_empty()
        && rem_a.len() * rem_d.len() <= RENAME_LIMIT * RENAME_LIMIT
    {
        // line-hash multisets, computed once per file
        let sig = |repo: &Repository, oid: &ObjectId| -> Option<HashMap<u64, u32>> {
            let bytes = match overlay.and_then(|m| m.get(oid)) {
                Some(b) => b.clone(),
                None => repo.find_blob(*oid).ok()?.take_data(),
            };
            if bytes.contains(&0) {
                return None; // binary
            }
            let mut m: HashMap<u64, u32> = HashMap::new();
            for line in bytes.split(|b| *b == b'\n') {
                *m.entry(fnv1a(line)).or_insert(0) += 1;
            }
            Some(m)
        };

        let mut a_sigs = HashMap::new();
        for &ai in &rem_a {
            if let Some(s) = sig(repo, &adds[ai].1) {
                a_sigs.insert(ai, s);
            }
        }
        let mut d_sigs = HashMap::new();
        for &di in &rem_d {
            if let Some(s) = sig(repo, &dels[di].1) {
                d_sigs.insert(di, s);
            }
        }

        // score every surviving pair, then take best-first
        let mut scored: Vec<(u8, usize, usize)> = Vec::new();
        for (&ai, asig) in &a_sigs {
            for (&di, dsig) in &d_sigs {
                let s = similarity(dsig, asig);
                if s >= RENAME_THRESHOLD {
                    scored.push((s, ai, di));
                }
            }
        }
        scored.sort_by(|x, y| y.0.cmp(&x.0));

        for (s, ai, di) in scored {
            if add_taken[ai] || del_taken[di] {
                continue;
            }
            add_taken[ai] = true;
            del_taken[di] = true;
            out.push(Change::Renamed {
                from: dels[di].0.clone(),
                path: adds[ai].0.clone(),
                old_oid: dels[di].1,
                new_oid: adds[ai].1,
                similarity: s,
            });
            st.renames_inexact += 1;
        }
    } else if !rem_a.is_empty() && !rem_d.is_empty() {
        st.rename_limit_hit = true;
    }

    for (i, (path, oid)) in adds.into_iter().enumerate() {
        if !add_taken[i] {
            out.push(Change::Added { path, oid });
        }
    }
    for (i, (path, oid)) in dels.into_iter().enumerate() {
        if !del_taken[i] {
            out.push(Change::Deleted { path, oid });
        }
    }

    Ok(out)
}

/// Multiset containment: shared lines / larger file. Same shape as git's
/// similarity index, and it lands in the same ballpark for review purposes.
fn similarity(a: &HashMap<u64, u32>, b: &HashMap<u64, u32>) -> u8 {
    let total_a: u32 = a.values().sum();
    let total_b: u32 = b.values().sum();
    let denom = total_a.max(total_b);
    if denom == 0 {
        return 0;
    }
    let mut common = 0u32;
    for (h, ca) in a {
        if let Some(cb) = b.get(h) {
            common += (*ca).min(*cb);
        }
    }
    ((common as f64 / denom as f64) * 100.0).round() as u8
}

fn read(
    repo: &Repository,
    overlay: Option<&HashMap<ObjectId, Vec<u8>>>,
    oid: Option<ObjectId>,
) -> Result<Vec<u8>, Error> {
    match oid {
        Some(o) => match overlay.and_then(|m| m.get(&o)) {
            Some(bytes) => Ok(bytes.clone()),
            None => Ok(repo
                .find_blob(o)
                .map_err(|e| Error::git("blob", e))?
                .take_data()),
        },
        None => Ok(Vec::new()),
    }
}

/// One file's whole content at the range's head: the blob out of `head`'s tree,
/// or, with no head commit, the working file itself.
///
/// The worktree bytes are hashed the way `load_worktree` hashes them, so a
/// comment left in the file view lands on the same oid the diff recorded and
/// one thread renders in both views.
pub fn read_at_head(
    repo_path: &Path,
    head: Option<ObjectId>,
    path: &str,
) -> Result<(ObjectId, Vec<u8>), Error> {
    let root = discover(repo_path).ok_or_else(|| Error::NoRepository(repo_path.to_path_buf()))?;
    let Some(head) = head else {
        let file = root.join(path);
        let bytes = std::fs::read(&file).map_err(|source| Error::Io { path: file, source })?;
        return Ok((hash_object(&bytes), bytes));
    };
    let repo = open_repo(&root)?;
    let entry = repo
        .find_commit(head)
        .map_err(|e| Error::git("commit", e))?
        .tree()
        .map_err(|e| Error::git("tree", e))?
        .lookup_entry_by_path(path)
        .map_err(|e| Error::git("tree", e))?
        .ok_or_else(|| Error::NotInTree {
            path: path.to_string(),
        })?;
    let oid = entry.object_id();
    let bytes = entry
        .object()
        .map_err(|e| Error::git("blob", e))?
        .detach()
        .data;
    Ok((oid, bytes))
}

/// The same file at the range's base: the blob out of `base`'s tree, or the
/// index entry when there is no base commit (the `INDEX...WORKTREE` range).
///
/// `None` when the file was not there, which makes the whole file read as
/// added. An unchanged file resolves to the same oid as the head and the diff
/// against it is empty, so one code path marks every file, changed or not.
pub fn read_at_base(
    repo_path: &Path,
    base: Option<ObjectId>,
    path: &str,
) -> Result<Option<(ObjectId, Vec<u8>)>, Error> {
    let root = discover(repo_path).ok_or_else(|| Error::NoRepository(repo_path.to_path_buf()))?;
    let repo = open_repo(&root)?;
    let oid = match base {
        Some(base) => repo
            .find_commit(base)
            .map_err(|e| Error::git("commit", e))?
            .tree()
            .map_err(|e| Error::git("tree", e))?
            .lookup_entry_by_path(path)
            .map_err(|e| Error::git("tree", e))?
            .map(|entry| entry.object_id()),
        None => repo
            .index_or_empty()
            .map_err(|e| Error::git("index", e))?
            .entry_by_path(path.into())
            .map(|e| e.id),
    };
    let Some(oid) = oid else { return Ok(None) };
    let bytes = repo
        .find_blob(oid)
        .map_err(|e| Error::git("blob", e))?
        .take_data();
    Ok(Some((oid, bytes)))
}

/// One file's whole content at the head, marked with what the range did to it:
/// every line of `new` as a row, additions tinted, and a collapsed `Removed`
/// marker wherever lines went.
///
/// This is not the diff lowering. `FileLowerer` builds addressable hunks with
/// bounded context and collapses the rest; this keeps the file whole and never
/// splices a deleted line into it. Reading a change and reading the file it
/// landed in are two different things, and this is the second.
pub fn whole_file_rows(blobs: &[Blob], old: Option<u32>, new: u32) -> Vec<Row> {
    let new_src = blobs[new as usize].text.as_str();
    let old_src = old.map_or("", |b| blobs[b as usize].text.as_str());

    let input = InternedInput::new(old_src, new_src);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);

    let mut rows: Vec<Row> = Vec::with_capacity(blobs[new as usize].line_count() + 8);
    // Walks the new side; `old_at` tracks the base line the equal run is at, so
    // context rows carry both numbers exactly as the diff view's do.
    let (mut new_at, mut old_at) = (0u32, 0u32);
    for h in diff.hunks() {
        while new_at < h.after.start {
            rows.push(Row::Code {
                kind: LineKind::Context,
                old_no: Some(old_at + 1),
                new_no: Some(new_at + 1),
                blob: new,
                line: new_at,
            });
            new_at += 1;
            old_at += 1;
        }
        // The anchor the seen tick box and the progress bar read. Emitted here
        // too, so a hunk ticked in the file view is ticked in the diff.
        rows.push(Row::HunkBar {
            old: old.filter(|_| h.before.end > h.before.start).map(|b| Side {
                blob: b,
                start: h.before.start,
                end: h.before.end - 1,
            }),
            new: Some(new)
                .filter(|_| h.after.end > h.after.start)
                .map(|b| Side {
                    blob: b,
                    start: h.after.start,
                    end: h.after.end - 1,
                }),
        });
        // What went, as a marker rather than as content — the reveal splices
        // the real rows in on demand.
        if let (Some(b), true) = (old, h.before.end > h.before.start) {
            rows.push(Row::Removed {
                blob: b,
                start: h.before.start,
                end: h.before.end - 1,
            });
        }
        while new_at < h.after.end {
            rows.push(Row::Code {
                kind: LineKind::Add,
                old_no: None,
                new_no: Some(new_at + 1),
                blob: new,
                line: new_at,
            });
            new_at += 1;
        }
        old_at = h.before.end;
    }
    while (new_at as usize) < blobs[new as usize].line_count() {
        rows.push(Row::Code {
            kind: LineKind::Context,
            old_no: Some(old_at + 1),
            new_no: Some(new_at + 1),
            blob: new,
            line: new_at,
        });
        new_at += 1;
        old_at += 1;
    }
    rows
}

// ---------------------------------------------------------------------------
// Shared git plumbing: revision resolution, repo and tree access, status.
// ---------------------------------------------------------------------------

/// Resolve any revision to a commit oid — gix's rev-parse handles HEAD~N,
/// branches, remotes, tags, and abbreviated oids; tags peel to their commit.
pub fn resolve(repo: &Repository, rev: &str) -> Result<ObjectId, Error> {
    let rev = rev.trim();
    if rev.is_empty() {
        return Err(Error::EmptyRevision);
    }
    let unknown = |source: Box<dyn std::error::Error + Send + Sync>| Error::UnknownRevision {
        rev: rev.to_string(),
        source,
    };
    let id = repo
        .rev_parse_single(rev)
        .map_err(|e| unknown(Box::new(e)))?;
    Ok(id
        .object()
        .map_err(|e| unknown(Box::new(e)))?
        .peel_to_kind(gix::object::Kind::Commit)
        .map_err(|_| Error::NotACommit {
            rev: rev.to_string(),
        })?
        .id)
}

/// Open the repo at `root` with a small object cache — tree and blob reads
/// repeat heavily during lowering and session mining.
pub(crate) fn open_repo(root: &Path) -> Result<Repository, Error> {
    let mut repo = gix::open(root).map_err(|e| Error::git("open", e))?;
    repo.object_cache_size_if_unset(16 * 1024 * 1024);
    Ok(repo)
}

/// All blobs (and symlinks) of `tree`, as path -> oid.
fn flatten_tree(repo: &Repository, tree: ObjectId) -> Result<HashMap<String, ObjectId>, Error> {
    let tree = repo.find_tree(tree).map_err(|e| Error::git("tree", e))?;
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse()
        .breadthfirst(&mut recorder)
        .map_err(|e| Error::git("tree", e))?;
    Ok(recorder
        .records
        .into_iter()
        .filter(|e| e.mode.is_blob() || e.mode.is_link())
        .map(|e| (e.filepath.to_string(), e.oid))
        .collect())
}

/// The diff of two commits: their trees, see [`diff_trees`].
pub fn diff_commits(
    repo: &Repository,
    base: ObjectId,
    head: ObjectId,
) -> Result<Vec<Change>, Error> {
    let tree_of = |oid: ObjectId| {
        repo.find_commit(oid)
            .map_err(|e| Error::git("commit", e))?
            .tree_id()
            .map(|id| id.detach())
            .map_err(|e| Error::git("tree", e))
    };
    diff_trees(repo, tree_of(base)?, tree_of(head)?)
}

/// Tree-to-tree diff: Added/Deleted/Modified, no renames (detect_renames runs
/// its own two-pass afterwards). Blobs and symlinks only; a submodule entry is
/// not a file.
pub fn diff_trees(repo: &Repository, old: ObjectId, new: ObjectId) -> Result<Vec<Change>, Error> {
    let old = repo.find_tree(old).map_err(|e| Error::git("tree", e))?;
    let new = repo.find_tree(new).map_err(|e| Error::git("tree", e))?;
    let mut out = Vec::new();
    old.changes()
        .map_err(|e| Error::git("diff", e))?
        .options(|options| {
            options.track_path();
            options.track_rewrites(None);
        })
        .for_each_to_obtain_tree(&new, |change| {
            use gix::object::tree::diff::Change as C;
            match change {
                C::Addition {
                    location,
                    entry_mode,
                    id,
                    ..
                } if entry_mode.is_blob() || entry_mode.is_link() => out.push(Change::Added {
                    path: location.to_string(),
                    oid: id.detach(),
                }),
                C::Deletion {
                    location,
                    entry_mode,
                    id,
                    ..
                } if entry_mode.is_blob() || entry_mode.is_link() => out.push(Change::Deleted {
                    path: location.to_string(),
                    oid: id.detach(),
                }),
                C::Modification {
                    location,
                    previous_id,
                    id,
                    entry_mode,
                    ..
                } if entry_mode.is_blob() || entry_mode.is_link() => out.push(Change::Modified {
                    path: location.to_string(),
                    old_oid: previous_id.detach(),
                    new_oid: id.detach(),
                }),
                _ => {}
            }
            Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
        })
        .map_err(|e| Error::git("diff", e))?;
    Ok(out)
}

/// Worktree-vs-index status (untracked included, ignore rules applied), as
/// (path, change-kind) pairs. gix handles the racy-git guard internally.
pub(crate) fn worktree_status(repo: &Repository) -> Result<Vec<(String, u8)>, Error> {
    let mut entries = Vec::new();
    let status = repo
        .status(gix::progress::Discard)
        .map_err(|e| Error::git("status", e))?
        .untracked_files(gix::status::UntrackedFiles::Files);
    for item in status
        .into_iter(Vec::new())
        .map_err(|e| Error::git("status", e))?
    {
        let item = item.map_err(|e| Error::git("status", e))?;
        if let gix::status::Item::IndexWorktree(change) = item
            && let Some(summary) = change.summary()
        {
            entries.push((change.rela_path().to_string(), summary as u8));
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{add, commit, init_repo};

    #[test]
    fn worktree_load_diffs_index_vs_worktree() {
        let (_tmp, root) = init_repo();
        add(&root, "a.txt", "one\ntwo\nthree\n");
        std::fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        std::fs::write(root.join("b.txt"), "untracked\n").unwrap();

        let loaded = load(&root, INDEX_REV, WORKTREE_REV).unwrap();
        assert_eq!(loaded.merge_base, None);
        assert_eq!(loaded.head, None);
        assert_eq!(loaded.workdir.as_deref(), Some(root.as_path()));
        let mut paths: Vec<&str> = loaded.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, ["a.txt", "b.txt"]);

        let a = loaded.files.iter().find(|f| f.path == "a.txt").unwrap();
        assert!(!a.is_new);
        assert_eq!((a.adds, a.dels), (2, 1)); // TWO replaces two, four appended
        let b = loaded.files.iter().find(|f| f.path == "b.txt").unwrap();
        assert!(b.is_new);

        // The stage payload mirrors the files, with real endpoint oids.
        let sa = loaded.stage.iter().find(|s| s.path == "a.txt").unwrap();
        assert!(sa.old.is_some() && sa.new.is_some());
        let sb = loaded.stage.iter().find(|s| s.path == "b.txt").unwrap();
        assert!(sb.old.is_none() && sb.new.is_some());

        // The new-side oid is the real git blob hash of the worktree bytes.
        assert_eq!(sa.new.unwrap(), hash_object(b"one\nTWO\nthree\nfour\n"));
    }

    #[test]
    fn a_worktree_load_lists_the_working_tree() {
        let (_tmp, root) = init_repo();
        add(&root, "kept.txt", "a\n");
        add(&root, "gone.txt", "b\n");
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::write(root.join("new.txt"), "c\n").unwrap();

        let loaded = load(&root, INDEX_REV, WORKTREE_REV).unwrap();

        // Tracked minus what the diff deletes, plus what it adds — untracked
        // files included, because the browser has to reach them too.
        assert_eq!(loaded.tree, ["kept.txt", "new.txt"]);
    }

    #[test]
    fn a_commit_load_lists_the_whole_tree_at_the_head() {
        let (_tmp, root) = init_repo();
        std::fs::write(root.join("quiet.txt"), "unchanged\n").unwrap();
        std::fs::write(root.join("src.txt"), "before\n").unwrap();
        commit(&root, "base");
        std::fs::write(root.join("src.txt"), "after\n").unwrap();
        commit(&root, "head");

        let loaded = load(&root, "HEAD~1", "HEAD").unwrap();

        // Everything at the head, not just what the range touched; that is what
        // the listing is for.
        assert_eq!(loaded.tree, ["quiet.txt", "src.txt"]);
        assert_eq!(
            loaded
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["src.txt"]
        );
    }

    #[test]
    fn a_file_deleted_in_the_range_is_not_at_the_head() {
        let (_tmp, root) = init_repo();
        std::fs::write(root.join("kept.txt"), "a\n").unwrap();
        std::fs::write(root.join("gone.txt"), "b\n").unwrap();
        commit(&root, "base");
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        commit(&root, "head");

        let loaded = load(&root, "HEAD~1", "HEAD").unwrap();

        // Changed but absent from the listing: that difference is what the
        // browser reads as a deletion.
        assert_eq!(loaded.tree, ["kept.txt"]);
        assert!(loaded.files.iter().any(|f| f.path == "gone.txt"));
    }
}
