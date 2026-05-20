pub mod support;

use std::{collections::HashMap, rc::Rc};

use concats_core::{
    Oid, rewrite, session,
    snapshot::{self, SnapshotReason},
    turn,
};

fn simulate_rebase(
    repo: &git2::Repository,
    worktree: &std::path::Path,
    commits: &[(&str, &str)],
    base: Oid,
) -> Vec<(Oid, Oid)> {
    // Replay the rebase: from `base`, recreate each commit with a fresh tree and
    // a bumped committer timestamp so the new SHAs differ.
    let original_head = Oid::from(repo.head().unwrap().target().unwrap());
    let mut pairs = Vec::new();
    // Walk original history from base..HEAD collecting the original OIDs.
    let mut revwalk = repo.revwalk().unwrap();
    revwalk.push(original_head.as_git()).unwrap();
    revwalk.hide(base.as_git()).unwrap();
    let mut original: Vec<git2::Oid> = revwalk
        .filter_map(std::result::Result::ok)
        .collect::<Vec<_>>();
    original.reverse();

    // Reset HEAD to base, then recommit each original commit with updated timestamps.
    let base_commit = repo.find_commit(base.as_git()).unwrap();
    repo.reference("refs/heads/master", base.as_git(), true, "rebase reset")
        .ok();
    repo.set_head_detached(base.as_git()).unwrap();
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_head(Some(&mut checkout)).unwrap();

    let mut parent_commit = base_commit;
    for (i, old_oid) in original.iter().enumerate() {
        let (file_name, contents) = commits[i];
        std::fs::write(worktree.join(file_name), contents).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        // Bump timestamp to force new SHA.
        let sig = git2::Signature::new(
            "test",
            "test@test",
            &git2::Time::new(2_000_000_000 + i64::try_from(i).unwrap(), 0),
        )
        .unwrap();
        let new_oid = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                &format!("rebased {file_name}"),
                &tree,
                &[&parent_commit],
            )
            .unwrap();
        pairs.push((Oid::from(*old_oid), Oid::from(new_oid)));
        parent_commit = repo.find_commit(new_oid).unwrap();
    }
    pairs
}

#[test]
fn linear_rebase_rewrites_all_turns() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());

    let anchor_a = support::commit_head(&repo, dir.path(), "a.txt", "A");
    let session = session::create(repo.clone(), "session-a", anchor_a).unwrap();
    let turn1 = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _anchor_b = support::commit_head(&repo, dir.path(), "b.txt", "B");
    let turn2 = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let anchor_c = support::commit_head(&repo, dir.path(), "c.txt", "C");
    let turn3 = session::commit(&session, &support::turn_message("session-a")).unwrap();

    let pairs = simulate_rebase(
        &repo,
        dir.path(),
        &[("a.txt", "A+"), ("b.txt", "B+"), ("c.txt", "C+")],
        base,
    );
    let map: HashMap<Oid, Oid> = pairs.iter().copied().collect();
    let new_c = map[&anchor_c];

    let report = rewrite::apply(&repo, &map).unwrap();

    assert_eq!(report.sessions.len(), 1);
    assert!(report.dropped_anchors.is_empty());

    let reloaded = session::open(repo.clone(), "session-a").unwrap();
    let turns = turn::list(&reloaded).unwrap();
    assert_eq!(turns.len(), 3);
    assert_ne!(turns[0].oid, turn1.oid);
    assert_ne!(turns[1].oid, turn2.oid);
    assert_ne!(turns[2].oid, turn3.oid);

    let tip_commit = repo.find_commit(turns[2].oid.as_git()).unwrap();
    assert_eq!(tip_commit.parent_id(1).unwrap(), new_c.as_git());

    assert!(session::containing(&repo, new_c).unwrap().len() == 1);
}

#[test]
fn amend_rewrites_only_affected_turns() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));

    let anchor_a = support::commit_head(&repo, dir.path(), "a.txt", "A");
    let session = session::create(repo.clone(), "session-a", anchor_a).unwrap();
    let turn1 = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let anchor_b = support::commit_head(&repo, dir.path(), "b.txt", "B");
    let turn2 = session::commit(&session, &support::turn_message("session-a")).unwrap();

    // Amend only anchor B.
    let new_b_commit = {
        let sig =
            git2::Signature::new("test", "test@test", &git2::Time::new(2_000_000_000, 0)).unwrap();
        let parent = repo.find_commit(anchor_a.as_git()).unwrap();
        let tree = parent.tree().unwrap();
        repo.commit(None, &sig, &sig, "amended", &tree, &[&parent])
            .unwrap()
    };
    let new_b = Oid::from(new_b_commit);

    let mut map = HashMap::new();
    map.insert(anchor_b, new_b);

    let report = rewrite::apply(&repo, &map).unwrap();
    assert_eq!(report.sessions.len(), 1);
    assert!(report.dropped_anchors.is_empty());

    let reloaded = session::open(repo.clone(), "session-a").unwrap();
    let turns = turn::list(&reloaded).unwrap();
    // turn1 had parent anchor_a (unmapped, unchanged): unchanged SHA.
    assert_eq!(turns[0].oid, turn1.oid);
    // turn2 had parent anchor_b: rewritten.
    assert_ne!(turns[1].oid, turn2.oid);
    let tip_commit = repo.find_commit(turns[1].oid.as_git()).unwrap();
    assert_eq!(tip_commit.parent_id(0).unwrap(), turn1.oid.as_git());
    assert_eq!(tip_commit.parent_id(1).unwrap(), new_b.as_git());

    // Running again with the same (now stale) map is a no-op.
    let second = rewrite::apply(&repo, &map).unwrap();
    assert!(second.sessions.is_empty());
    assert!(second.snapshots.is_empty());
}

#[test]
fn dropped_anchor_is_reported_and_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));

    let anchor_a = support::commit_head(&repo, dir.path(), "a.txt", "A");
    let session = session::create(repo.clone(), "session-a", anchor_a).unwrap();
    let _turn1 = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let anchor_b = support::commit_head(&repo, dir.path(), "b.txt", "B");
    let _turn2 = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let anchor_c = support::commit_head(&repo, dir.path(), "c.txt", "C");
    let _turn3 = session::commit(&session, &support::turn_message("session-a")).unwrap();

    // Simulate: anchor_b is dropped (absent from map); a and c are rewritten.
    let new_a = {
        let sig =
            git2::Signature::new("test", "test@test", &git2::Time::new(2_000_000_100, 0)).unwrap();
        let parent = repo
            .find_commit(Oid::from(repo.head().unwrap().target().unwrap()).as_git())
            .unwrap();
        // Build a synthetic predecessor as a new root for mapping purposes.
        // For this test we only need a distinct OID, reuse the tree.
        let tree = parent.tree().unwrap();
        Oid::from(repo.commit(None, &sig, &sig, "new a", &tree, &[]).unwrap())
    };
    let new_c = {
        let sig =
            git2::Signature::new("test", "test@test", &git2::Time::new(2_000_000_200, 0)).unwrap();
        let parent = repo.find_commit(new_a.as_git()).unwrap();
        let tree = parent.tree().unwrap();
        Oid::from(
            repo.commit(None, &sig, &sig, "new c", &tree, &[&parent])
                .unwrap(),
        )
    };

    let mut map = HashMap::new();
    map.insert(anchor_a, new_a);
    map.insert(anchor_c, new_c);

    let report = rewrite::apply(&repo, &map).unwrap();

    assert_eq!(report.sessions.len(), 1);
    // turn2's parent anchor_b was not in the map — reported as dropped.
    assert!(
        report
            .dropped_anchors
            .iter()
            .any(|drop| drop.parent == anchor_b),
        "expected dropped anchor for B, got {:?}",
        report.dropped_anchors
    );

    let reloaded = session::open(repo.clone(), "session-a").unwrap();
    let turns = turn::list(&reloaded).unwrap();
    assert_eq!(turns.len(), 3);
    // turn2 keeps anchor_b (orphaned) as parent 1.
    let turn2_commit = repo.find_commit(turns[1].oid.as_git()).unwrap();
    assert_eq!(turn2_commit.parent_id(1).unwrap(), anchor_b.as_git());
}

#[test]
fn concurrent_sessions_sharing_anchor_both_update() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));

    let anchor = support::commit_head(&repo, dir.path(), "a.txt", "A");
    let session_a = session::create(repo.clone(), "session-a", anchor).unwrap();
    let _ta = session::commit(&session_a, &support::turn_message("session-a")).unwrap();
    let session_b = session::create(repo.clone(), "session-b", anchor).unwrap();
    let _tb = session::commit(&session_b, &support::turn_message("session-b")).unwrap();

    // Synthesize a new anchor.
    let new_anchor = {
        let sig =
            git2::Signature::new("test", "test@test", &git2::Time::new(2_000_000_300, 0)).unwrap();
        let parent = repo
            .find_commit(Oid::from(repo.head().unwrap().target().unwrap()).as_git())
            .unwrap();
        let tree = parent.tree().unwrap();
        Oid::from(
            repo.commit(None, &sig, &sig, "new anchor", &tree, &[])
                .unwrap(),
        )
    };

    let mut map = HashMap::new();
    map.insert(anchor, new_anchor);

    let report = rewrite::apply(&repo, &map).unwrap();
    assert_eq!(report.sessions.len(), 2);

    for name in ["session-a", "session-b"] {
        let reloaded = session::open(repo.clone(), name).unwrap();
        let turns = turn::list(&reloaded).unwrap();
        let tip = repo.find_commit(turns[0].oid.as_git()).unwrap();
        // First turn in each session now anchors on the new commit.
        assert_eq!(tip.parent_id(0).unwrap(), new_anchor.as_git());
    }
}

#[test]
fn snapshot_chain_is_rewritten_when_turns_change() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));

    let anchor = support::commit_head(&repo, dir.path(), "a.txt", "A");
    let session = session::create(repo.clone(), "session-a", anchor).unwrap();

    let turn1 = session::commit(&session, &support::turn_message("session-a")).unwrap();
    snapshot::capture(&session, turn1.oid, SnapshotReason::TurnCommit).unwrap();
    std::fs::write(dir.path().join("changed.txt"), "hello").unwrap();
    let turn2 = session::commit(&session, &support::turn_message("session-a")).unwrap();
    snapshot::capture(&session, turn2.oid, SnapshotReason::TurnCommit).unwrap();

    // Synthesize a new anchor.
    let new_anchor = {
        let sig =
            git2::Signature::new("test", "test@test", &git2::Time::new(2_000_000_400, 0)).unwrap();
        let parent = repo.find_commit(anchor.as_git()).unwrap();
        let tree = parent.tree().unwrap();
        Oid::from(
            repo.commit(None, &sig, &sig, "new anchor", &tree, &[])
                .unwrap(),
        )
    };

    let mut map = HashMap::new();
    map.insert(anchor, new_anchor);

    let report = rewrite::apply(&repo, &map).unwrap();
    assert_eq!(report.sessions.len(), 1);
    assert_eq!(report.snapshots.len(), 1);

    let reloaded = session::open(repo.clone(), "session-a").unwrap();
    let turns = turn::list(&reloaded).unwrap();
    assert_eq!(turns.len(), 2);
    let snap_for_turn2 = snapshot::get(&reloaded, turns[1].oid).unwrap();
    let snap_commit = repo.find_commit(snap_for_turn2.oid.as_git()).unwrap();
    assert_eq!(snap_commit.parent_count(), 2);
    // parent 1 points at rewritten turn2.
    assert_eq!(snap_commit.parent_id(1).unwrap(), turns[1].oid.as_git());
    // First snapshot's parent 0 is the rewritten turn1.
    let snap_for_turn1 = snapshot::get(&reloaded, turns[0].oid).unwrap();
    let snap1_commit = repo.find_commit(snap_for_turn1.oid.as_git()).unwrap();
    assert_eq!(snap1_commit.parent_count(), 1);
    assert_eq!(snap1_commit.parent_id(0).unwrap(), turns[0].oid.as_git());
}

#[test]
fn irrelevant_rewrite_is_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));

    let anchor = support::commit_head(&repo, dir.path(), "a.txt", "A");
    let session = session::create(repo.clone(), "session-a", anchor).unwrap();
    let original_turn = session::commit(&session, &support::turn_message("session-a")).unwrap();

    // Map keys that no session references.
    let unrelated_old = {
        let sig =
            git2::Signature::new("test", "test@test", &git2::Time::new(2_000_000_500, 0)).unwrap();
        let parent = repo.find_commit(anchor.as_git()).unwrap();
        let tree = parent.tree().unwrap();
        Oid::from(
            repo.commit(None, &sig, &sig, "unrelated old", &tree, &[])
                .unwrap(),
        )
    };
    let unrelated_new = {
        let sig =
            git2::Signature::new("test", "test@test", &git2::Time::new(2_000_000_600, 0)).unwrap();
        let parent = repo.find_commit(anchor.as_git()).unwrap();
        let tree = parent.tree().unwrap();
        Oid::from(
            repo.commit(None, &sig, &sig, "unrelated new", &tree, &[])
                .unwrap(),
        )
    };

    let mut map = HashMap::new();
    map.insert(unrelated_old, unrelated_new);

    let report = rewrite::apply(&repo, &map).unwrap();
    assert!(report.sessions.is_empty());
    assert!(report.snapshots.is_empty());
    assert!(report.dropped_anchors.is_empty());

    // Session tip unchanged.
    let reloaded = session::open(repo.clone(), "session-a").unwrap();
    assert_eq!(session::tip(&reloaded).unwrap(), original_turn.oid);
}

#[test]
fn first_turn_with_single_parent_anchor_is_rewritten() {
    // Session where first turn's parent 0 IS the anchor (HEAD == base at turn time).
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let base = Oid::from(repo.head().unwrap().target().unwrap());

    let session = session::create(repo.clone(), "session-a", base).unwrap();
    let turn1 = session::commit(&session, &support::turn_message("session-a")).unwrap();
    // First-turn sanity: single parent pointing at base.
    let t1_commit = repo.find_commit(turn1.oid.as_git()).unwrap();
    assert_eq!(t1_commit.parent_count(), 1);
    assert_eq!(t1_commit.parent_id(0).unwrap(), base.as_git());

    // Rewrite the base.
    let new_base = {
        let sig =
            git2::Signature::new("test", "test@test", &git2::Time::new(2_000_000_700, 0)).unwrap();
        let parent = repo.find_commit(base.as_git()).unwrap();
        let tree = parent.tree().unwrap();
        Oid::from(
            repo.commit(None, &sig, &sig, "new base", &tree, &[])
                .unwrap(),
        )
    };

    let mut map = HashMap::new();
    map.insert(base, new_base);

    let report = rewrite::apply(&repo, &map).unwrap();
    assert_eq!(report.sessions.len(), 1);

    let reloaded = session::open(repo.clone(), "session-a").unwrap();
    let turns = turn::list(&reloaded).unwrap();
    let new_t1 = repo.find_commit(turns[0].oid.as_git()).unwrap();
    assert_eq!(new_t1.parent_count(), 1);
    assert_eq!(new_t1.parent_id(0).unwrap(), new_base.as_git());
}
