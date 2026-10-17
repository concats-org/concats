//! A real git repository to load from, shared by the [`crate::load`] and
//! [`crate::stage`] tests.
//!
//! One fixture, not two: the staging tests load a range and write it back, so a
//! difference in how two fixtures were built would show up as a difference in
//! what staging does.

use std::path::{Path, PathBuf};

use concats_fixture::git;

use crate::stage::stage_file;

/// A fresh repo (no commits) in a temp dir.
pub fn init_repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    git(&root, &["init", "-q"]);
    (tmp, root)
}

/// Write + `git add` a file, so the index holds its current content.
pub fn add(root: &Path, path: &str, content: &str) {
    std::fs::write(root.join(path), content).unwrap();
    let repo = gix::open(root).unwrap();
    let mut index = repo.index_or_empty().unwrap().into_owned_or_cloned();
    stage_file(&repo, root, &mut index, path).unwrap();
    index.write(gix::index::write::Options::default()).unwrap();
}

pub fn commit(root: &Path, message: &str) {
    git(root, &["add", "-A", "."]);
    git(root, &["commit", "-q", "-m", message]);
}
