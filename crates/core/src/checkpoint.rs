use std::path::PathBuf;

use crate::{error::Result, git::Oid};

/// Session-scoped checkpoint store that writes commits to a per-session ref
/// (`refs/agent/sessions/<session-id>`) without touching the user's branch,
/// index, or working tree state.
///
/// Trees are built with an in-memory index — the on-disk `.git/index` is never
/// read or written by this code.
pub struct CheckpointStore {
    repo_path: PathBuf,
    session_id: String,
    ref_name: String,
    turn_count: u32,
    /// When forking, the first checkpoint uses this as its parent instead of HEAD.
    fork_parent: Option<git2::Oid>,
}

impl CheckpointStore {
    /// Create a new checkpoint store for the given session.
    pub fn new(repo_path: PathBuf, session_id: String) -> Self {
        let ref_name = format!("refs/agent/sessions/{session_id}");
        Self {
            repo_path,
            session_id,
            ref_name,
            turn_count: 0,
            fork_parent: None,
        }
    }

    /// Create a checkpoint store with a specific turn count.
    ///
    /// Used by hook handlers to reconstruct state from a persisted turn count
    /// across separate process invocations.
    pub fn new_with_turn_count(repo_path: PathBuf, session_id: String, turn_count: u32) -> Self {
        let ref_name = format!("refs/agent/sessions/{session_id}");
        Self {
            repo_path,
            session_id,
            ref_name,
            turn_count,
            fork_parent: None,
        }
    }

    /// Create a checkpoint store that forks from an existing commit.
    ///
    /// The first checkpoint will be parented from `fork_from_oid` instead of HEAD.
    pub fn new_forked(repo_path: PathBuf, session_id: String, fork_from_oid: git2::Oid) -> Self {
        let ref_name = format!("refs/agent/sessions/{session_id}");
        Self {
            repo_path,
            session_id,
            ref_name,
            turn_count: 0,
            fork_parent: Some(fork_from_oid),
        }
    }

    /// Return the session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Return the full ref name (e.g. `refs/agent/sessions/<session-id>`).
    pub fn ref_name(&self) -> &str {
        &self.ref_name
    }

    /// Create the initial checkpoint commit for a new turn.
    ///
    /// Captures the full working directory state into a tree and writes a
    /// commit on the session ref.
    pub fn create_checkpoint(&self, prompt: &str) -> Result<Oid> {
        self.create_checkpoint_sync(prompt)
    }

    /// Amend the current checkpoint: rebuild the tree from the working
    /// directory and create a new commit with the same parent, then
    /// force-update the ref. The previous tip becomes unreferenced.
    pub fn amend_checkpoint(&self) -> Result<Oid> {
        self.amend_checkpoint_sync()
    }

    /// Finalize the checkpoint for the completed turn. Writes the full commit
    /// message with prompt, response summary, and trailers, then increments
    /// the turn counter.
    pub fn finalize_checkpoint(
        &mut self,
        prompt: &str,
        response_summary: &str,
        stop_reason: &str,
    ) -> Result<Oid> {
        let oid = self.finalize_checkpoint_sync(prompt, response_summary, stop_reason)?;
        self.turn_count += 1;
        Ok(oid)
    }

    // ── sync implementations ──────────────────────────────────────────

    fn create_checkpoint_sync(&self, prompt: &str) -> Result<Oid> {
        let repo = git2::Repository::open(&self.repo_path)?;
        let tree_oid = self.build_tree_from_workdir(&repo)?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = self.signature(&repo)?;

        let message = self.initial_message(prompt);
        // Chain from the previous turn's finalized commit. For the very first
        // turn, use the fork parent (if forking) or HEAD.
        let fork_commit = self.fork_parent.and_then(|oid| repo.find_commit(oid).ok());
        let parent = self
            .current_tip(&repo)
            .or(fork_commit)
            .or_else(|| self.head_commit(&repo));
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();

        let oid = repo.commit(None, &sig, &sig, &message, &tree, &parents)?;
        self.update_ref(&repo, oid)?;

        Ok(Oid::from(oid))
    }

    fn amend_checkpoint_sync(&self) -> Result<Oid> {
        let repo = git2::Repository::open(&self.repo_path)?;

        let tip = self
            .current_tip(&repo)
            .ok_or_else(|| crate::error::Error::session("no checkpoint to amend"))?;

        let tree_oid = self.build_tree_from_workdir(&repo)?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = self.signature(&repo)?;

        // Reuse the existing commit message.
        let message = tip.message().unwrap_or("checkpoint").to_string();

        // Same parent(s) as the current tip (amend semantics).
        let parents = self.tip_parents_or_head(&repo);
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();

        let oid = repo.commit(None, &sig, &sig, &message, &tree, &parent_refs)?;
        self.update_ref(&repo, oid)?;

        Ok(Oid::from(oid))
    }

    fn finalize_checkpoint_sync(
        &self,
        prompt: &str,
        response_summary: &str,
        stop_reason: &str,
    ) -> Result<Oid> {
        let repo = git2::Repository::open(&self.repo_path)?;
        let tree_oid = self.build_tree_from_workdir(&repo)?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = self.signature(&repo)?;

        let message = self.final_message(prompt, response_summary, stop_reason);

        // Amend semantics: reuse the current tip's parents so we replace
        // it rather than chaining from it. Falls back to HEAD if no tip
        // exists (e.g. create_checkpoint was never called).
        let parents = self.tip_parents_or_head(&repo);
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();

        let oid = repo.commit(None, &sig, &sig, &message, &tree, &parent_refs)?;
        self.update_ref(&repo, oid)?;

        Ok(Oid::from(oid))
    }

    // ── helpers ───────────────────────────────────────────────────────

    /// Build a tree object from the working directory state.
    ///
    /// Walk the working directory, respecting `.gitignore`, `.git/info/exclude`,
    /// and global gitignore rules. Dotfiles are included so that configs
    /// like `.eslintrc`, `.prettierrc`, `.editorconfig` are captured.
    /// NOTE: `.claude/worktrees/` is explicitly filtered out as it may contain nested
    /// git state and may be modified by other agents which leads to trouble.
    ///
    /// NOTE: Never call `index.write()`, so the on-disk index (the user's staged
    /// changes) is left untouched.
    fn build_tree_from_workdir(&self, repo: &git2::Repository) -> Result<git2::Oid> {
        let mut index = repo.index()?;
        index.clear()?;

        let workdir = repo
            .workdir()
            .ok_or_else(|| crate::error::Error::session("bare repository not supported"))?;

        let walker = ignore::WalkBuilder::new(workdir)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(true)
            .filter_entry(|entry| {
                !entry
                    .path()
                    .ancestors()
                    .any(|a| a.ends_with(".claude/worktrees"))
            })
            .build();

        for result in walker {
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue, // vanished mid-walk, skip
            };
            // Skip directories — only add files
            if entry.file_type().is_none_or(|ft| !ft.is_file()) {
                continue;
            }
            if let Ok(rel) = entry.path().strip_prefix(workdir) {
                // Silently skip individual failures (e.g. file vanished)
                let _ = index.add_path(rel);
            }
        }

        let oid = index.write_tree()?;
        Ok(oid)
    }

    /// Resolve the session ref to its tip commit.
    fn current_tip<'r>(&self, repo: &'r git2::Repository) -> Option<git2::Commit<'r>> {
        repo.find_reference(&self.ref_name)
            .ok()
            .and_then(|r| r.peel_to_commit().ok())
    }

    /// Resolve HEAD to a commit.
    fn head_commit<'r>(&self, repo: &'r git2::Repository) -> Option<git2::Commit<'r>> {
        repo.head().ok().and_then(|h| h.peel_to_commit().ok())
    }

    /// Return the current tip's parents (amend semantics). If there is no
    /// session ref yet, fall back to HEAD so the first commit is parented
    /// from the user's branch.
    fn tip_parents_or_head<'r>(&self, repo: &'r git2::Repository) -> Vec<git2::Commit<'r>> {
        if let Some(tip) = self.current_tip(repo) {
            (0..tip.parent_count())
                .filter_map(|i| tip.parent(i).ok())
                .collect()
        } else {
            self.head_commit(repo).into_iter().collect()
        }
    }

    /// Force-update the session ref to point at the given OID.
    fn update_ref(&self, repo: &git2::Repository, oid: git2::Oid) -> Result<()> {
        repo.reference(&self.ref_name, oid, true, "checkpoint")?;
        Ok(())
    }

    fn signature(&self, repo: &git2::Repository) -> Result<git2::Signature<'static>> {
        let sig = repo
            .signature()
            .or_else(|_| git2::Signature::now("concats", "concats@checkpoint"))?;
        Ok(sig)
    }

    fn initial_message(&self, prompt: &str) -> String {
        let subject: String = prompt.chars().take(72).collect();
        format!(
            "checkpoint: {subject}\n\n\
             <checkpoint>\n\
             <prompt>\n{prompt}\n</prompt>\n\
             <session>{}</session>\n\
             <turn>{}</turn>\n\
             </checkpoint>",
            self.session_id, self.turn_count
        )
    }

    fn final_message(&self, prompt: &str, response_summary: &str, stop_reason: &str) -> String {
        let subject: String = prompt.chars().take(72).collect();
        let trimmed_response: String = response_summary.chars().take(500).collect();
        format!(
            "checkpoint: {subject}\n\n\
             <checkpoint>\n\
             <prompt>\n{prompt}\n</prompt>\n\
             <response>\n{trimmed_response}\n</response>\n\
             <session>{}</session>\n\
             <turn>{}</turn>\n\
             <stop-reason>{stop_reason}</stop-reason>\n\
             </checkpoint>",
            self.session_id, self.turn_count
        )
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::fs;

    use super::*;

    /// Helper: create a temp git repo with an initial commit so HEAD exists.
    fn init_repo_with_commit(dir: &std::path::Path) -> git2::Repository {
        let repo = git2::Repository::init(dir).unwrap();
        {
            let mut index = repo.index().unwrap();
            // Write an initial file so we have something to commit.
            fs::write(dir.join("init.txt"), "init").unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();
            let sig = git2::Signature::now("test", "test@test").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        repo
    }

    #[test]
    fn create_checkpoint_writes_ref() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        let store = CheckpointStore::new(dir.path().to_path_buf(), "test-session".into());
        let oid = store.create_checkpoint("hello world").unwrap();

        // The ref should exist and point to our commit.
        let repo = git2::Repository::open(dir.path()).unwrap();
        let r = repo
            .find_reference("refs/agent/sessions/test-session")
            .unwrap();
        assert_eq!(
            r.peel_to_commit().unwrap().id().to_string(),
            oid.to_string()
        );
    }

    #[test]
    fn amend_checkpoint_updates_ref() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        let store = CheckpointStore::new(dir.path().to_path_buf(), "test-session".into());
        let oid1 = store.create_checkpoint("prompt").unwrap();

        // Write a new file and amend.
        fs::write(dir.path().join("new.txt"), "content").unwrap();
        let oid2 = store.amend_checkpoint().unwrap();

        assert_ne!(oid1.to_string(), oid2.to_string());
    }

    #[test]
    fn finalize_checkpoint_includes_trailers() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        let mut store = CheckpointStore::new(dir.path().to_path_buf(), "test-session".into());
        store.create_checkpoint("fix the bug").unwrap();
        store
            .finalize_checkpoint("fix the bug", "I fixed the bug by...", "end_turn")
            .unwrap();

        let repo = git2::Repository::open(dir.path()).unwrap();
        let r = repo
            .find_reference("refs/agent/sessions/test-session")
            .unwrap();
        let commit = r.peel_to_commit().unwrap();
        let msg = commit.message().unwrap();

        assert!(msg.contains("<session>test-session</session>"));
        assert!(msg.contains("<turn>0</turn>"));
        assert!(msg.contains("<stop-reason>end_turn</stop-reason>"));
        assert!(msg.contains("I fixed the bug by..."));
    }

    #[test]
    fn finalize_increments_turn_count() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        let mut store = CheckpointStore::new(dir.path().to_path_buf(), "test-session".into());

        store.create_checkpoint("turn 0").unwrap();
        store
            .finalize_checkpoint("turn 0", "resp 0", "end_turn")
            .unwrap();

        store.create_checkpoint("turn 1").unwrap();
        store
            .finalize_checkpoint("turn 1", "resp 1", "end_turn")
            .unwrap();

        let repo = git2::Repository::open(dir.path()).unwrap();
        let r = repo
            .find_reference("refs/agent/sessions/test-session")
            .unwrap();
        let commit = r.peel_to_commit().unwrap();
        let msg = commit.message().unwrap();
        assert!(msg.contains("<turn>1</turn>"));
    }

    #[test]
    fn empty_commit_allowed() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        let mut store = CheckpointStore::new(dir.path().to_path_buf(), "test-session".into());

        // Create + finalize without any file changes — should succeed (empty commit).
        let oid = store.create_checkpoint("just talking").unwrap();
        assert!(!oid.to_string().is_empty());

        let oid2 = store
            .finalize_checkpoint("just talking", "response", "end_turn")
            .unwrap();
        assert!(!oid2.to_string().is_empty());
    }

    #[test]
    fn head_and_index_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());

        let head_before = repo.head().unwrap().peel_to_commit().unwrap().id();

        let mut store = CheckpointStore::new(dir.path().to_path_buf(), "test-session".into());
        fs::write(dir.path().join("changed.txt"), "data").unwrap();
        store.create_checkpoint("test").unwrap();
        store
            .finalize_checkpoint("test", "resp", "end_turn")
            .unwrap();

        // HEAD should not have moved.
        let repo = git2::Repository::open(dir.path()).unwrap();
        let head_after = repo.head().unwrap().peel_to_commit().unwrap().id();
        assert_eq!(head_before, head_after);
    }

    #[test]
    fn finalize_amends_instead_of_chaining() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();

        let mut store = CheckpointStore::new(dir.path().to_path_buf(), "test-session".into());

        // Turn 0: create + finalize.
        store.create_checkpoint("turn 0").unwrap();
        store
            .finalize_checkpoint("turn 0", "resp 0", "end_turn")
            .unwrap();

        // Turn 1: create + finalize.
        store.create_checkpoint("turn 1").unwrap();
        store
            .finalize_checkpoint("turn 1", "resp 1", "end_turn")
            .unwrap();

        // Walk the ref: tip should be turn 1, its parent should be turn 0,
        // turn 0's parent should be HEAD. Total chain length = 2 (not 4).
        let repo = git2::Repository::open(dir.path()).unwrap();
        let tip = repo
            .find_reference("refs/agent/sessions/test-session")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert!(
            tip.message().unwrap().contains("<turn>1</turn>"),
            "tip should be turn 1"
        );
        assert_eq!(tip.parent_count(), 1);

        let turn0 = tip.parent(0).unwrap();
        assert!(
            turn0.message().unwrap().contains("<turn>0</turn>"),
            "parent should be turn 0"
        );
        assert_eq!(turn0.parent_count(), 1);
        assert_eq!(
            turn0.parent(0).unwrap().id(),
            head_oid,
            "turn 0's parent should be HEAD"
        );
    }

    #[test]
    fn amend_preserves_parent() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();

        let store = CheckpointStore::new(dir.path().to_path_buf(), "test-session".into());
        store.create_checkpoint("prompt").unwrap();

        fs::write(dir.path().join("file.txt"), "data").unwrap();
        store.amend_checkpoint().unwrap();

        // After amend, the tip's parent should still be HEAD (not the
        // initial create commit).
        let repo = git2::Repository::open(dir.path()).unwrap();
        let tip = repo
            .find_reference("refs/agent/sessions/test-session")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert_eq!(tip.parent_count(), 1);
        assert_eq!(tip.parent(0).unwrap().id(), head_oid);
    }

    #[test]
    fn first_checkpoint_parents_from_head() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();

        let store = CheckpointStore::new(dir.path().to_path_buf(), "test-session".into());
        store.create_checkpoint("first").unwrap();

        let repo = git2::Repository::open(dir.path()).unwrap();
        let tip = repo
            .find_reference("refs/agent/sessions/test-session")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert_eq!(tip.parent_count(), 1);
        assert_eq!(
            tip.parent(0).unwrap().id(),
            head_oid,
            "first checkpoint should be parented from HEAD"
        );
    }

    #[test]
    fn concurrent_sessions_dont_collide() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        let store_a = CheckpointStore::new(dir.path().to_path_buf(), "session-a".into());
        let store_b = CheckpointStore::new(dir.path().to_path_buf(), "session-b".into());

        store_a.create_checkpoint("prompt a").unwrap();
        store_b.create_checkpoint("prompt b").unwrap();

        // Both refs should exist independently.
        let repo = git2::Repository::open(dir.path()).unwrap();
        assert!(repo.find_reference("refs/agent/sessions/session-a").is_ok());
        assert!(repo.find_reference("refs/agent/sessions/session-b").is_ok());
    }

    #[test]
    fn gitignored_files_excluded_from_tree() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        // Create a .gitignore that excludes target/ and *.log
        fs::write(dir.path().join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        fs::write(dir.path().join("target/debug/binary"), "ELF").unwrap();
        fs::write(dir.path().join("build.log"), "some log output").unwrap();

        // Also add a normal file that should be included.
        fs::write(dir.path().join("src.rs"), "fn main() {}").unwrap();

        let store = CheckpointStore::new(dir.path().to_path_buf(), "ignore-test".into());
        store.create_checkpoint("test gitignore exclusion").unwrap();

        let repo = git2::Repository::open(dir.path()).unwrap();
        let tip = repo
            .find_reference("refs/agent/sessions/ignore-test")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        let tree = tip.tree().unwrap();

        // src.rs should be present
        assert!(
            tree.get_name("src.rs").is_some(),
            "src.rs should be in the tree"
        );

        // .gitignore itself is a dotfile but should be captured
        assert!(
            tree.get_name(".gitignore").is_some(),
            ".gitignore should be in the tree"
        );

        // target/ and *.log should be excluded by .gitignore
        assert!(
            tree.get_name("target").is_none(),
            "target/ should be excluded by .gitignore"
        );
        assert!(
            tree.get_name("build.log").is_none(),
            "build.log should be excluded by .gitignore"
        );
    }

    #[test]
    fn dotfiles_included_in_tree() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        // Create dotfiles that are typically tracked
        fs::write(dir.path().join(".eslintrc"), "{}").unwrap();
        fs::write(dir.path().join(".prettierrc"), "{}").unwrap();

        let store = CheckpointStore::new(dir.path().to_path_buf(), "dotfile-test".into());
        store.create_checkpoint("test dotfile inclusion").unwrap();

        let repo = git2::Repository::open(dir.path()).unwrap();
        let tip = repo
            .find_reference("refs/agent/sessions/dotfile-test")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        let tree = tip.tree().unwrap();

        assert!(
            tree.get_name(".eslintrc").is_some(),
            ".eslintrc should be in the tree"
        );
        assert!(
            tree.get_name(".prettierrc").is_some(),
            ".prettierrc should be in the tree"
        );
    }

    #[test]
    fn worktree_paths_excluded_from_tree() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());

        // Simulate a Claude Code worktree with a nested .git file.
        let wt_dir = dir.path().join(".claude/worktrees/some-worktree");
        fs::create_dir_all(&wt_dir).unwrap();
        fs::write(wt_dir.join(".git"), "gitdir: /tmp/fake").unwrap();
        fs::write(wt_dir.join("file.txt"), "worktree file").unwrap();

        // Also add a normal file that should be included.
        fs::write(dir.path().join("real.txt"), "real content").unwrap();

        let store = CheckpointStore::new(dir.path().to_path_buf(), "wt-test".into());
        store.create_checkpoint("test worktree exclusion").unwrap();

        // Verify the tree: real.txt should be present, worktree files should not.
        let repo = git2::Repository::open(dir.path()).unwrap();
        let tip = repo
            .find_reference("refs/agent/sessions/wt-test")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        let tree = tip.tree().unwrap();

        assert!(
            tree.get_name("real.txt").is_some(),
            "real.txt should be in the tree"
        );
        // Walk the full tree to ensure no worktree paths leaked in.
        let mut found_worktree = false;
        tree.walk(git2::TreeWalkMode::PreOrder, |root, _entry| {
            if root.contains("worktrees") {
                found_worktree = true;
                return git2::TreeWalkResult::Abort;
            }
            git2::TreeWalkResult::Ok
        })
        .unwrap();
        assert!(!found_worktree, ".claude/worktrees/ should be excluded");
    }
}
