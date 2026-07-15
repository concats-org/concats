pub mod support;

use std::rc::Rc;

use concats_core::{
    Oid, session,
    snapshot::{self, SnapshotReason},
};

#[test]
fn first_turn_omits_duplicate_head_parent_and_uses_empty_tree() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let commit = repo.find_commit(turn.oid.as_gix()).unwrap();

    assert_eq!(commit.parent_ids().count(), 1);
    assert_eq!(commit.parent_ids().next().unwrap().detach(), head.as_gix());
    assert_eq!(commit.tree().unwrap().iter().count(), 0);
}

#[test]
fn later_turn_links_previous_turn_and_current_head() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let branch_head = support::commit_head(&repo, dir.path(), "branch.txt", "branch");
    let next_turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let commit = repo.find_commit(next_turn.oid.as_gix()).unwrap();

    assert_eq!(commit.parent_ids().count(), 2);
    assert_eq!(
        commit.parent_ids().next().unwrap().detach(),
        turn.oid.as_gix()
    );
    assert_eq!(
        commit.parent_ids().nth(1).unwrap().detach(),
        branch_head.as_gix()
    );
}

#[test]
fn amend_refreshes_branch_parent_without_changing_session_parent() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let branch_head = support::commit_head(&repo, dir.path(), "branch.txt", "branch");
    let updated = session::amend(&session, turn.message()).unwrap();
    let updated_commit = repo.find_commit(updated.oid.as_gix()).unwrap();

    assert_eq!(updated_commit.parent_ids().count(), 2);
    assert_eq!(
        updated_commit.parent_ids().next().unwrap().detach(),
        head.as_gix()
    );
    assert_eq!(
        updated_commit.parent_ids().nth(1).unwrap().detach(),
        branch_head.as_gix()
    );

    let refreshed_head = support::commit_head(&repo, dir.path(), "branch-2.txt", "branch-2");
    let refreshed = session::amend(&session, updated.message()).unwrap();
    let refreshed_commit = repo.find_commit(refreshed.oid.as_gix()).unwrap();

    assert_eq!(refreshed_commit.parent_ids().count(), 2);
    assert_eq!(
        refreshed_commit.parent_ids().next().unwrap().detach(),
        head.as_gix()
    );
    assert_eq!(
        refreshed_commit.parent_ids().nth(1).unwrap().detach(),
        refreshed_head.as_gix()
    );
}

#[test]
fn forked_first_turn_uses_source_turn_as_first_parent() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let source_session = session::create(repo.clone(), "session-a", head).unwrap();
    let source_turn =
        session::commit(&source_session, &support::turn_message("session-a")).unwrap();

    let branch_head = support::commit_head(&repo, dir.path(), "branch.txt", "branch");
    let fork_session = session::create(repo.clone(), "session-b", source_turn.oid).unwrap();
    let fork_turn = session::commit(&fork_session, &support::turn_message("session-b")).unwrap();
    let commit = repo.find_commit(fork_turn.oid.as_gix()).unwrap();

    assert_eq!(commit.parent_ids().count(), 2);
    assert_eq!(
        commit.parent_ids().next().unwrap().detach(),
        source_turn.oid.as_gix()
    );
    assert_eq!(
        commit.parent_ids().nth(1).unwrap().detach(),
        branch_head.as_gix()
    );
}

#[test]
fn first_snapshot_points_only_to_corresponding_turn() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _ = snapshot::capture(&session, turn.oid, SnapshotReason::TurnCommit).unwrap();
    let snapshot = snapshot::get(&session, turn.oid).unwrap();
    let snapshot_commit = repo.find_commit(snapshot.oid.as_gix()).unwrap();

    assert_eq!(snapshot_commit.parent_ids().count(), 1);
    assert_eq!(
        snapshot_commit.parent_ids().next().unwrap().detach(),
        turn.oid.as_gix()
    );
}

#[test]
fn later_snapshot_links_previous_snapshot_and_current_turn() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let first_turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _ = snapshot::capture(&session, first_turn.oid, SnapshotReason::TurnCommit).unwrap();
    let first_snapshot = snapshot::get(&session, first_turn.oid).unwrap();

    std::fs::write(dir.path().join("changed.txt"), "changed").unwrap();
    let second_turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _ = snapshot::capture(&session, second_turn.oid, SnapshotReason::TurnCommit).unwrap();
    let second_snapshot = snapshot::get(&session, second_turn.oid).unwrap();
    let snapshot_commit = repo.find_commit(second_snapshot.oid.as_gix()).unwrap();

    assert_eq!(snapshot_commit.parent_ids().count(), 2);
    assert_eq!(
        snapshot_commit.parent_ids().next().unwrap().detach(),
        first_snapshot.oid.as_gix()
    );
    assert_eq!(
        snapshot_commit.parent_ids().nth(1).unwrap().detach(),
        second_turn.oid.as_gix()
    );
}

#[test]
fn git_for_each_ref_contains_finds_session_refs() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();
    let expected_ref = support::session_ref_name("session-a");

    let _turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let output = support::run_git(
        dir.path(),
        vec![
            "for-each-ref".to_string(),
            "--format=%(refname)".to_string(),
            "--contains".to_string(),
            head.to_string(),
        ],
    );

    assert!(output.lines().any(|line| line == expected_ref));
}

#[test]
fn git_log_ancestry_path_walks_snapshot_history() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let first_turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _ = snapshot::capture(&session, first_turn.oid, SnapshotReason::TurnCommit).unwrap();
    let first_snapshot = snapshot::get(&session, first_turn.oid).unwrap();
    std::fs::write(dir.path().join("src.txt"), "hello").unwrap();
    let second_turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let _ = snapshot::capture(&session, second_turn.oid, SnapshotReason::TurnCommit).unwrap();
    let second_snapshot = snapshot::get(&session, second_turn.oid).unwrap();

    let output = support::run_git(
        dir.path(),
        vec![
            "log".to_string(),
            "--ancestry-path".to_string(),
            format!(
                "{}..{}",
                first_turn.oid,
                support::snapshot_ref_name("session-a")
            ),
            "--format=%H".to_string(),
        ],
    );
    let commits = output.lines().collect::<Vec<_>>();
    let first_snapshot_oid = first_snapshot.oid.to_string();
    let second_snapshot_oid = second_snapshot.oid.to_string();

    assert!(commits.contains(&second_snapshot_oid.as_str()));
    assert!(commits.contains(&first_snapshot_oid.as_str()));
}

#[test]
fn contains_reaches_turns_and_branch_history() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let session = session::create(repo.clone(), "session-a", head).unwrap();

    let first_turn = session::commit(&session, &support::turn_message("session-a")).unwrap();
    let branch_head = support::commit_head(&repo, dir.path(), "branch.txt", "branch");
    let second_turn = session::commit(&session, &support::turn_message("session-a")).unwrap();

    assert!(session::contains(&session, head).unwrap());
    assert!(session::contains(&session, first_turn.oid).unwrap());
    assert!(session::contains(&session, branch_head).unwrap());
    assert!(session::contains(&session, second_turn.oid).unwrap());
}

#[test]
fn containing_returns_all_sessions_for_shared_turn_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Rc::new(support::init_repo_with_commit(dir.path()));
    let head = Oid::from(repo.head_id().unwrap().detach());
    let source_session = session::create(repo.clone(), "session-a", head).unwrap();
    let source_turn =
        session::commit(&source_session, &support::turn_message("session-a")).unwrap();

    support::commit_head(&repo, dir.path(), "branch.txt", "branch");
    let fork_session = session::create(repo.clone(), "session-b", source_turn.oid).unwrap();
    let _fork_turn = session::commit(&fork_session, &support::turn_message("session-b")).unwrap();

    let sessions = session::containing(&repo, source_turn.oid).unwrap();
    let ids = sessions
        .iter()
        .map(|session| session.id.as_ref())
        .collect::<Vec<_>>();

    assert_eq!(ids, vec!["session-b", "session-a"]);
}
