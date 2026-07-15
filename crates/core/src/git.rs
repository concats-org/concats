use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    path::Path,
};

use gix::bstr::{BStr, BString};
use time::{OffsetDateTime, UtcOffset};

use crate::error::{Error, Result};

/// An opaque commit identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Oid(gix::ObjectId);

impl Oid {
    #[must_use]
    pub fn short(&self) -> String {
        self.0.to_string()[..7].to_string()
    }

    #[must_use]
    pub fn as_gix(self) -> gix::ObjectId {
        self.0
    }
}

impl From<gix::ObjectId> for Oid {
    fn from(oid: gix::ObjectId) -> Self {
        Self(oid)
    }
}

impl std::str::FromStr for Oid {
    type Err = gix::hash::decode::Error;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        gix::ObjectId::from_hex(value.as_bytes()).map(Self)
    }
}

impl std::fmt::Display for Oid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl serde::Serialize for Oid {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for Oid {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Resolve `HEAD` to a commit object ID.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened, `HEAD` cannot be
/// resolved, or `HEAD` does not point to a commit.
pub fn current_head_oid(repo: &gix::Repository) -> Result<Oid> {
    let id = repo.head_id().map_err(Error::git)?;
    Ok(Oid::from(id.detach()))
}

pub(crate) fn commit_time(time: gix::date::Time) -> Result<OffsetDateTime> {
    let timestamp = OffsetDateTime::from_unix_timestamp(time.seconds)
        .map_err(|error| Error::session(format!("invalid git commit timestamp: {error}")))?;
    let offset = UtcOffset::from_whole_seconds(time.offset).unwrap_or(UtcOffset::UTC);
    Ok(timestamp.to_offset(offset))
}

pub(crate) fn resolve_ref<'repo>(
    repo: &'repo gix::Repository,
    ref_name: &str,
) -> Option<gix::Commit<'repo>> {
    let mut reference = repo.find_reference(ref_name).ok()?;
    let id = reference.peel_to_id().ok()?;
    repo.find_commit(id).ok()
}

/// Return whether `ancestor` is reachable from `tip` (inclusive).
pub(crate) fn reachable_from(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    ancestor: gix::ObjectId,
) -> Result<bool> {
    use gix::repository::merge_base::Error as MergeBaseError;
    match repo.merge_base(tip, ancestor) {
        Ok(base) => Ok(base.detach() == ancestor),
        Err(MergeBaseError::NotFound { .. }) => Ok(false),
        Err(error) => Err(Error::git(error)),
    }
}

pub(crate) fn signature(
    repo: &gix::Repository,
    minimum_time_seconds: Option<i64>,
) -> gix::actor::Signature {
    let (name, email) = repo
        .committer()
        .and_then(std::result::Result::ok)
        .map_or_else(
            || ("concats".into(), "concats@turn".into()),
            |sig| (sig.name.to_owned(), sig.email.to_owned()),
        );
    let mut time = repo
        .committer()
        .and_then(std::result::Result::ok)
        .and_then(|sig| sig.time().ok())
        .unwrap_or_else(gix::date::Time::now_local_or_utc);
    if let Some(minimum_time_seconds) = minimum_time_seconds
        && time.seconds <= minimum_time_seconds
    {
        time.seconds = minimum_time_seconds + 1;
    }

    gix::actor::Signature { name, email, time }
}

/// Write a commit object and force-point `ref_name` at it.
pub(crate) fn commit(
    repo: &gix::Repository,
    ref_name: &str,
    message: &str,
    parts: CommitParts,
) -> Result<gix::ObjectId> {
    let signature = signature(repo, parts.minimum_time_seconds);
    let commit = gix::objs::Commit {
        tree: parts.tree,
        parents: parts.parents.into_iter().collect(),
        author: signature.clone(),
        committer: signature,
        encoding: None,
        message: message.into(),
        extra_headers: Vec::new(),
    };
    let oid = repo.write_object(&commit).map_err(Error::git)?.detach();
    repo.reference(
        ref_name,
        oid,
        gix::refs::transaction::PreviousValue::Any,
        parts.log_message,
    )
    .map_err(Error::git)?;
    Ok(oid)
}

/// The non-message inputs of [`commit`] — tree, parents, and the write knobs.
pub(crate) struct CommitParts {
    pub tree: gix::ObjectId,
    pub parents: Vec<gix::ObjectId>,
    pub minimum_time_seconds: Option<i64>,
    pub log_message: &'static str,
}

pub(crate) fn snapshot_workdir(repo: &gix::Repository) -> Result<gix::ObjectId> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| Error::session("bare repository not supported"))?
        .to_path_buf();

    let walker = ignore::WalkBuilder::new(&workdir)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .filter_entry(move |entry| {
            // NOTE: Don't ignore the root even if it has a .git entry.
            if entry.depth() == 0 {
                return true;
            }

            let file_type = entry
                .file_type()
                .expect("filesystem walk entries always have a file type");

            // NOTE: Ignore the .git entry itself.
            if entry
                .path()
                .file_name()
                .is_some_and(|name| name == OsStr::new(".git"))
            {
                return false;
            }

            if file_type.is_file() || file_type.is_symlink() {
                return true;
            }

            // NOTE: Ignore non-file, non-directory entries such as sockets,
            // pipes, and device nodes.
            if !file_type.is_dir() {
                return false;
            }

            // NOTE: Ignore nested git roots such as worktrees and sub-repos.
            match entry.path().join(".git").try_exists() {
                Ok(true) => false,
                Ok(false) => true,
                Err(error) => {
                    tracing::warn!(
                        path = ?entry.path(),
                        "failed to inspect directory for nested git root: {error}"
                    );
                    true
                }
            }
        })
        .build();

    // The filter pipeline hashes worktree bytes the way `git add` would
    // (clean filters, symlink targets, executable bits), so snapshot blobs
    // stay content-identical to the blobs regular commits produce.
    let (mut pipeline, index) = repo.filter_pipeline(None).map_err(Error::git)?;
    let mut editor = repo
        .edit_tree(gix::ObjectId::empty_tree(repo.object_hash()))
        .map_err(Error::git)?;

    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(&workdir) else {
            continue;
        };
        let rela_path = gix::path::to_unix_separators_on_windows(gix::path::into_bstr(relative));
        match pipeline.worktree_file_to_object(rela_path.as_ref(), &index) {
            Ok(Some((oid, kind, _))) => {
                if let Err(error) = editor.upsert(rela_path.as_ref(), kind, oid) {
                    tracing::warn!("failed to add {relative:?} to snapshot tree: {error}");
                }
            }
            // NOTE: The file vanished between walk and read, or is a type git
            // cannot track — the same cases `git add` skips.
            Ok(None) => {}
            Err(error) => {
                tracing::warn!("failed to add {relative:?} to snapshot tree: {error}");
            }
        }
    }

    Ok(editor.write().map_err(Error::git)?.detach())
}

/// Make the working tree (and index) match `tree_id`, replicating libgit2's
/// checkout semantics: only paths that differ between the index and the target
/// are touched. In safe mode (`force == false`), touching a path whose
/// worktree state differs from the index is a conflict — conflicts are
/// collected and returned without changing anything. With `force`, dirty
/// paths are overwritten and files absent from the target are removed.
pub(crate) fn checkout_tree(
    repo: &gix::Repository,
    tree_id: gix::ObjectId,
    force: bool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| Error::session("bare repository not supported"))?
        .to_path_buf();

    let tree = repo.find_tree(tree_id).map_err(Error::git)?;
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse()
        .breadthfirst(&mut recorder)
        .map_err(Error::git)?;
    let target: HashMap<BString, (gix::objs::tree::EntryMode, gix::ObjectId)> = recorder
        .records
        .into_iter()
        .filter(|entry| entry.mode.is_blob() || entry.mode.is_link())
        .map(|entry| (entry.filepath, (entry.mode, entry.oid)))
        .collect();

    let index = repo.index_or_empty().map_err(Error::git)?;
    let indexed: HashMap<BString, (gix::index::entry::Mode, gix::ObjectId)> = index
        .entries()
        .iter()
        .filter(|entry| entry.stage() == gix::index::entry::Stage::Unconflicted)
        .map(|entry| (entry.path(&index).to_owned(), (entry.mode, entry.id)))
        .collect();

    let dirty = worktree_dirty(repo)?;
    let writes: Vec<(BString, gix::objs::tree::EntryMode, gix::ObjectId)> = target
        .iter()
        .filter(|(path, (mode, oid))| {
            let index_match = indexed.get(*path).is_some_and(|(index_mode, index_oid)| {
                index_oid == oid && *index_mode == gix::index::entry::Mode::from(*mode)
            });
            !index_match || (force && dirty.contains(*path))
        })
        .map(|(path, (mode, oid))| (path.clone(), *mode, *oid))
        .collect();
    let deletes: Vec<BString> = indexed
        .keys()
        .filter(|path| !target.contains_key(*path))
        .cloned()
        .collect();

    if !force {
        let mut conflicts: Vec<String> = writes
            .iter()
            .map(|(path, ..)| path)
            .chain(deletes.iter())
            .filter(|path| dirty.contains(*path))
            .map(ToString::to_string)
            .collect();
        conflicts.sort();
        if !conflicts.is_empty() {
            return Err(Error::restore_conflict(conflicts));
        }
    }

    let checkout_state = write_worktree_files(repo, &workdir, &writes)?;
    remove_worktree_files(&workdir, &deletes)?;

    // Fold the checked-out entries (with their fresh stat) into the index so
    // git sees the restored files as clean, matching libgit2's checkout.
    let touched: HashSet<&BStr> = writes
        .iter()
        .map(|(path, ..)| path.as_ref())
        .chain(deletes.iter().map(AsRef::as_ref))
        .collect();
    let mut new_state = gix::index::State::new(repo.object_hash());
    for entry in index.entries() {
        let path = entry.path(&index);
        if touched.contains(path) {
            continue;
        }
        new_state.dangerously_push_entry(entry.stat, entry.id, entry.flags, entry.mode, path);
    }
    for entry in checkout_state.entries() {
        new_state.dangerously_push_entry(
            entry.stat,
            entry.id,
            entry.flags,
            entry.mode,
            entry.path(&checkout_state),
        );
    }
    new_state.sort_entries();
    let mut index_file = gix::index::File::from_state(new_state, repo.index_path());
    index_file
        .write(gix::index::write::Options::default())
        .map_err(Error::git)?;

    Ok(())
}

/// Paths whose worktree state differs from the index — untracked included.
fn worktree_dirty(repo: &gix::Repository) -> Result<HashSet<BString>> {
    let mut dirty = HashSet::new();
    let status = repo
        .status(gix::progress::Discard)
        .map_err(Error::git)?
        .untracked_files(gix::status::UntrackedFiles::Files);
    for item in status.into_iter(Vec::new()).map_err(Error::git)? {
        let item = item.map_err(Error::git)?;
        if let gix::status::Item::IndexWorktree(change) = item
            && change.summary().is_some()
        {
            dirty.insert(change.rela_path().to_owned());
        }
    }
    Ok(dirty)
}

/// Materialize `writes` in the working tree, returning the checked-out index
/// entries with their fresh stat information.
fn write_worktree_files(
    repo: &gix::Repository,
    workdir: &Path,
    writes: &[(BString, gix::objs::tree::EntryMode, gix::ObjectId)],
) -> Result<gix::index::State> {
    let mut state = gix::index::State::new(repo.object_hash());
    for (path, mode, oid) in writes {
        state.dangerously_push_entry(
            gix::index::entry::Stat::default(),
            *oid,
            gix::index::entry::Flags::empty(),
            gix::index::entry::Mode::from(*mode),
            path.as_ref(),
        );
    }
    state.sort_entries();

    let mut options = repo
        .checkout_options(gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping)
        .map_err(Error::git)?;
    options.destination_is_initially_empty = false;
    options.overwrite_existing = true;
    let objects = repo.objects.clone().into_arc().map_err(Error::git)?;
    let should_interrupt = std::sync::atomic::AtomicBool::new(false);
    gix::worktree::state::checkout(
        &mut state,
        workdir.to_path_buf(),
        objects,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &should_interrupt,
        options,
    )
    .map_err(Error::git)?;
    Ok(state)
}

fn remove_worktree_files(workdir: &Path, deletes: &[BString]) -> Result<()> {
    for path in deletes {
        let file = workdir.join(gix::path::from_bstr(path).as_ref());
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        // Prune now-emptied parent directories, as `git checkout` would.
        let mut dir = file.parent();
        while let Some(parent) = dir
            && parent != workdir
            && parent.starts_with(workdir)
            && std::fs::remove_dir(parent).is_ok()
        {
            dir = parent.parent();
        }
    }
    Ok(())
}
