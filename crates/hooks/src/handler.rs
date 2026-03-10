use std::path::Path;

use concats_core::{
    checkpoint::{self, Checkpoint, Draft, TranscriptEntry},
    current_head_oid,
    error::Result,
    session::{self, Session},
};

use crate::state::{HookState, ensure, save};

/// Ensure a session and hook state exist when Claude starts a session.
///
/// # Errors
///
/// Returns an error if the session cannot be opened or created, or the hook
/// state cannot be loaded or initialized.
pub fn on_session_started(repo_path: &Path, session_id: &str) -> Result<()> {
    let _ = ensure_session(repo_path, session_id)?;
    let _ = ensure(repo_path, session_id)?;
    Ok(())
}

/// Start a checkpoint and record the submitted user prompt.
///
/// # Errors
///
/// Returns an error if the session cannot be opened or created, the checkpoint
/// cannot be committed, or the hook state cannot be saved.
pub fn on_prompt_submitted(repo_path: &Path, session_id: &str, prompt: &str) -> Result<()> {
    let session = ensure_session(repo_path, session_id)?;
    let mut draft = Draft::new();
    draft
        .transcript
        .append(TranscriptEntry::prompt_now(prompt))?;
    let checkpoint = session::commit(&session, &draft)?;
    save(
        repo_path,
        session_id,
        &HookState {
            current_checkpoint: Some(checkpoint.oid),
        },
    )?;
    Ok(())
}

/// Update the open checkpoint snapshot after file changes.
///
/// # Errors
///
/// Returns an error if the current checkpoint cannot be loaded, the snapshot
/// commit or amendment fails, or the hook state cannot be saved.
pub fn on_files_changed(repo_path: &Path, session_id: &str) -> Result<()> {
    let (session, checkpoint) = load_open_checkpoint(repo_path, session_id)?;
    let updated = match checkpoint {
        Some(checkpoint) => session::amend(&session, &Draft::from_checkpoint(&checkpoint))?,
        None => session::commit(&session, &Draft::new())?,
    };
    save(
        repo_path,
        session_id,
        &HookState {
            current_checkpoint: Some(updated.oid),
        },
    )?;
    Ok(())
}

/// Append the assistant response and close the open checkpoint.
///
/// # Errors
///
/// Returns an error if the current checkpoint cannot be loaded, the response
/// commit or amendment fails, or the hook state cannot be saved.
pub fn on_stop(repo_path: &Path, session_id: &str, response: &str) -> Result<()> {
    let (session, checkpoint) = load_open_checkpoint(repo_path, session_id)?;
    let mut draft = checkpoint
        .as_ref()
        .map_or_else(Draft::new, Draft::from_checkpoint);
    draft
        .transcript
        .append(TranscriptEntry::response_now(response))?;

    let _updated = match checkpoint {
        Some(_) => session::amend(&session, &draft)?,
        None => session::commit(&session, &draft)?,
    };
    save(
        repo_path,
        session_id,
        &HookState {
            current_checkpoint: None,
        },
    )?;
    Ok(())
}

fn load_open_checkpoint(
    repo_path: &Path,
    session_id: &str,
) -> Result<(Session, Option<Checkpoint>)> {
    let state = ensure(repo_path, session_id)?;
    let session = ensure_session(repo_path, session_id)?;
    let checkpoint = state
        .current_checkpoint
        .map(|oid| checkpoint::get(&session, oid))
        .transpose()?;
    Ok((session, checkpoint))
}

fn ensure_session(repo_path: &Path, session_id: &str) -> Result<Session> {
    let base = current_head_oid(repo_path)?;
    session::open(repo_path, session_id).or_else(|_| session::create(repo_path, session_id, base))
}
