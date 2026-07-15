//! A real git repository to load from, shared by the [`crate::load`] and
//! [`crate::stage`] tests.
//!
//! One fixture, not two: the staging tests load a range and write it back, so a
//! difference in how two fixtures were built would show up as a difference in
//! what staging does.

use std::path::{Path, PathBuf};

use gix::ObjectId;

use crate::stage::{StageFile, stage_file};

/// A fresh repo (no commits) in a temp dir.
pub fn init_repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["init", "-q"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success());
    (tmp, root)
}

/// Write + `git add` a file, so the index holds its current content.
pub fn add(root: &Path, path: &str, content: &str) {
    std::fs::write(root.join(path), content).unwrap();
    let repo = gix::open(root).unwrap();
    let mut index = repo.open_index().unwrap_or_else(|_| {
        gix::index::File::from_state(
            gix::index::State::new(repo.object_hash()),
            repo.index_path(),
        )
    });
    stage_file(&repo, root, &mut index, path).unwrap();
    index.write(gix::index::write::Options::default()).unwrap();
}

pub fn commit(root: &Path, message: &str) {
    let env = [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_AUTHOR_NAME", "test"),
        ("GIT_AUTHOR_EMAIL", "test@example.com"),
        ("GIT_COMMITTER_NAME", "test"),
        ("GIT_COMMITTER_EMAIL", "test@example.com"),
    ];
    for args in [
        ["add", "-A", "."].as_slice(),
        ["commit", "-q", "--no-gpg-sign", "-m", message].as_slice(),
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .envs(env)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

/// The seen keys of one hunk of a [`StageFile`] — what the GUI's tick writes.
pub fn hunk_seen_keys(f: &StageFile, hunk: usize) -> Vec<(ObjectId, u32)> {
    let (old_start, dels, new_start, adds) = f.hunks[hunk];
    let mut v = Vec::new();
    if let Some(o) = f.old {
        for l in 0..dels as u32 {
            v.push((o, old_start - 1 + l));
        }
    }
    if let Some(n) = f.new {
        for l in 0..adds as u32 {
            v.push((n, new_start - 1 + l));
        }
    }
    v
}
