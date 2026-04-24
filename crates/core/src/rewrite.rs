//! Rebase-aware rewriting of session and snapshot chains.
//!
//! When branch history is rewritten (`git rebase`, `git commit --amend`), turn
//! commits whose parents referenced the old SHAs are left anchored to orphans.
//! [`apply`] mirrors the rewrite onto `refs/agent/sessions/*` and
//! `refs/agent/snapshots/*` so the graph stays connected to the live branch.

use std::{
    collections::HashMap,
    hash::{BuildHasher, RandomState},
    rc::Rc,
};

use crate::{
    error::Result,
    git::Oid,
    session::{self, Session},
    snapshot,
    turn::{self, Turn},
};

/// A single ref update performed during [`apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedRef {
    pub name: String,
    pub old_tip: Oid,
    pub new_tip: Oid,
}

/// A turn whose parents include commits missing from the rewrite map.
///
/// Typically an interactive rebase that dropped a commit outright: the anchor
/// has no replacement, so the turn keeps pointing at the orphaned commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedAnchor {
    pub turn: Oid,
    pub parent: Oid,
}

/// Outcome of a rewrite pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RewriteReport {
    pub sessions: Vec<UpdatedRef>,
    pub snapshots: Vec<UpdatedRef>,
    pub dropped_anchors: Vec<DroppedAnchor>,
}

/// Apply a commit rewrite map to every session and snapshot ref.
///
/// The map is typically produced by a `post-rewrite` git hook. Each session's
/// turns are reconstructed base-to-tip so downstream turns see the new parent 0
/// from the previous turn in the same pass. Trees, authors, committers, and
/// commit messages are preserved verbatim; only parents are remapped.
///
/// # Errors
///
/// Returns an error if session or snapshot refs cannot be enumerated, commits
/// cannot be loaded or written, or ref updates fail.
pub fn apply<S: BuildHasher>(
    repo: &Rc<git2::Repository>,
    initial: &HashMap<Oid, Oid, S>,
) -> Result<RewriteReport> {
    let mut report = RewriteReport::default();
    if initial.is_empty() {
        return Ok(report);
    }

    let mut map: HashMap<Oid, Oid, RandomState> = initial.iter().map(|(k, v)| (*k, *v)).collect();

    for session in session::list(repo)? {
        rewrite_session(&session, &mut map, &mut report)?;
    }

    for session in session::list(repo)? {
        rewrite_snapshots(&session, &map, &mut report)?;
    }

    Ok(report)
}

fn rewrite_session(
    session: &Session,
    map: &mut HashMap<Oid, Oid>,
    report: &mut RewriteReport,
) -> Result<()> {
    let turns = turn::list(session)?;
    if turns.is_empty() {
        return Ok(());
    }

    let repo = session.repo();
    let original_tip = turns
        .last()
        .map(|turn| turn.oid)
        .expect("turns is non-empty");
    let mut new_tip = original_tip;
    let mut changed = false;

    for turn in &turns {
        let commit = repo.find_commit(turn.oid.as_git())?;
        let original_parents: Vec<Oid> = commit.parent_ids().map(Oid::from).collect();
        let new_parents: Vec<Oid> = original_parents
            .iter()
            .map(|parent| map.get(parent).copied().unwrap_or(*parent))
            .collect();

        if new_parents == original_parents {
            new_tip = turn.oid;
            continue;
        }

        record_dropped(turn, &original_parents, map, report);

        let new_oid = write_with_parents(repo, &commit, &new_parents)?;
        map.insert(turn.oid, Oid::from(new_oid));
        new_tip = Oid::from(new_oid);
        changed = true;
    }

    if !changed {
        return Ok(());
    }

    let ref_name = session::ref_name(&session.id);
    repo.reference(
        &ref_name,
        new_tip.as_git(),
        true,
        "concats rewrite: post-rewrite",
    )?;
    report.sessions.push(UpdatedRef {
        name: ref_name,
        old_tip: original_tip,
        new_tip,
    });

    Ok(())
}

fn rewrite_snapshots(
    session: &Session,
    map: &HashMap<Oid, Oid>,
    report: &mut RewriteReport,
) -> Result<()> {
    let snapshots = snapshot::list(session)?;
    if snapshots.is_empty() {
        return Ok(());
    }

    let repo = session.repo();
    let original_tip = snapshots
        .last()
        .map(|snap| snap.oid)
        .expect("snapshots is non-empty");
    let mut prev_rewrite: Option<Oid> = None;
    let mut new_tip = original_tip;
    let mut changed = false;

    for snap in &snapshots {
        let commit = repo.find_commit(snap.oid.as_git())?;
        let original_parents: Vec<Oid> = commit.parent_ids().map(Oid::from).collect();
        let new_parents: Vec<Oid> = match original_parents.len() {
            1 => vec![
                map.get(&original_parents[0])
                    .copied()
                    .unwrap_or(original_parents[0]),
            ],
            2 => {
                let prev_parent = prev_rewrite.unwrap_or(original_parents[0]);
                let turn_parent = map
                    .get(&original_parents[1])
                    .copied()
                    .unwrap_or(original_parents[1]);
                vec![prev_parent, turn_parent]
            }
            _ => original_parents.clone(),
        };

        if new_parents == original_parents {
            prev_rewrite = Some(snap.oid);
            new_tip = snap.oid;
            continue;
        }

        let new_oid = write_with_parents(repo, &commit, &new_parents)?;
        prev_rewrite = Some(Oid::from(new_oid));
        new_tip = Oid::from(new_oid);
        changed = true;
    }

    if !changed {
        return Ok(());
    }

    let ref_name = snapshot::ref_name(&session.id);
    repo.reference(
        &ref_name,
        new_tip.as_git(),
        true,
        "concats rewrite: post-rewrite",
    )?;
    report.snapshots.push(UpdatedRef {
        name: ref_name,
        old_tip: original_tip,
        new_tip,
    });

    Ok(())
}

fn write_with_parents(
    repo: &git2::Repository,
    commit: &git2::Commit<'_>,
    parents: &[Oid],
) -> Result<git2::Oid> {
    let parent_commits: Vec<git2::Commit<'_>> = parents
        .iter()
        .map(|oid| repo.find_commit(oid.as_git()))
        .collect::<std::result::Result<_, _>>()?;
    let parent_refs: Vec<&git2::Commit<'_>> = parent_commits.iter().collect();
    let tree = commit.tree()?;
    let author = commit.author();
    let committer = commit.committer();
    Ok(repo.commit(
        None,
        &author,
        &committer,
        commit.message_raw().unwrap_or(""),
        &tree,
        &parent_refs,
    )?)
}

fn record_dropped(
    turn: &Turn,
    parents: &[Oid],
    map: &HashMap<Oid, Oid>,
    report: &mut RewriteReport,
) {
    for parent in parents {
        if map.contains_key(parent) {
            continue;
        }
        // NOTE: The parent was not in the rewrite map. When the turn is still
        // being rewritten (because another parent was remapped), surface this
        // parent as a potentially-dropped anchor so callers can warn the user.
        report.dropped_anchors.push(DroppedAnchor {
            turn: turn.oid,
            parent: *parent,
        });
    }
}
