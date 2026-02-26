use std::path::PathBuf;

use crate::error::Result;
use crate::git::Oid;

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
    /// Loads the repo's index into memory, runs `add_all` to capture the
    /// current workdir, then writes the tree to the ODB via `write_tree()`.
    /// Crucially, we never call `index.write()`, so the on-disk index file
    /// (the user's staged changes) is left untouched.
    fn build_tree_from_workdir(&self, repo: &git2::Repository) -> Result<git2::Oid> {
        let mut index = repo.index()?;
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
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
            "checkpoint: {subject}\n\n{prompt}\n\nAgent-Session: {}\nAgent-Turn: {}",
            self.session_id, self.turn_count
        )
    }

    fn final_message(&self, prompt: &str, response_summary: &str, stop_reason: &str) -> String {
        let subject: String = prompt.chars().take(72).collect();
        let trimmed_response: String = response_summary.chars().take(500).collect();
        format!(
            "checkpoint: {subject}\n\n{prompt}\n\n---\n\n{trimmed_response}\n\nAgent-Session: {}\nAgent-Turn: {}\nAgent-Stop-Reason: {stop_reason}",
            self.session_id, self.turn_count
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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

        assert!(msg.contains("Agent-Session: test-session"));
        assert!(msg.contains("Agent-Turn: 0"));
        assert!(msg.contains("Agent-Stop-Reason: end_turn"));
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
        assert!(msg.contains("Agent-Turn: 1"));
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
            tip.message().unwrap().contains("Agent-Turn: 1"),
            "tip should be turn 1"
        );
        assert_eq!(tip.parent_count(), 1);

        let turn0 = tip.parent(0).unwrap();
        assert!(
            turn0.message().unwrap().contains("Agent-Turn: 0"),
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
}
