use std::{cell::RefCell, rc::Rc};

use agent_client_protocol::{
    ContentBlock, SessionNotification, SessionUpdate, ToolCall as AcpToolCall, ToolKind,
};
use concats_core::{
    Oid,
    error::Result,
    session::{self, Session},
    snapshot::{self, SnapshotReason},
    turn::{Turn, TurnEntry},
};
use concats_message::{Turn as TurnMessage, TurnToolKind};

#[derive(Clone)]
struct ActiveTurn {
    session: Session,
    turn: Turn,
}

#[derive(Clone)]
pub struct TurnRecorder {
    active: Rc<RefCell<Option<ActiveTurn>>>,
    response_buffer: Rc<RefCell<String>>,
    agent_name: String,
}

impl TurnRecorder {
    #[must_use]
    pub fn new(agent_name: impl Into<String>) -> Self {
        Self {
            active: Rc::default(),
            response_buffer: Rc::default(),
            agent_name: agent_name.into(),
        }
    }

    pub fn start_prompt(&self, session: &Session, prompt_text: &str) -> Result<Turn> {
        self.clear_buffer();
        let message = self
            .new_message(session)?
            .with_entry(TurnEntry::prompt_now(prompt_text));
        let subject = message
            .suggest_subject()
            .unwrap_or_else(|| "files changed".to_string());
        let message = message.with_subject(subject)?;
        let turn = session::commit(session, &message)?;
        let _ = snapshot::capture(session, turn.oid, SnapshotReason::TurnCommit)?;

        *self.active.borrow_mut() = Some(ActiveTurn {
            session: session.clone(),
            turn: turn.clone(),
        });

        Ok(turn)
    }

    pub fn handle_notification(&self, notification: &SessionNotification) {
        match &notification.update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let ContentBlock::Text(text) = &chunk.content {
                    self.push_response_chunk(&text.text);
                }
            }
            SessionUpdate::ToolCall(tool_call) => {
                if let Err(error) = self.record_tool_call(tool_call) {
                    tracing::warn!("turn tool-call amend failed: {error}");
                }
            }
            _ => {}
        }
    }

    pub fn finish_response(&self) -> Result<()> {
        let text = {
            let mut buffer = self.response_buffer.borrow_mut();
            if buffer.trim().is_empty() {
                buffer.clear();
                return Ok(());
            }
            std::mem::take(&mut *buffer)
        };

        self.amend_active(|message| message.with_entry(TurnEntry::response_now(text)))
    }

    pub fn snapshot_after_tool_write(&self) -> Result<()> {
        if let Some(current) = self.active.borrow().as_ref() {
            let _ = snapshot::capture(
                &current.session,
                current.turn.oid,
                SnapshotReason::ToolWrite,
            )?;
        }
        Ok(())
    }

    #[must_use]
    pub fn current_oid(&self) -> Option<Oid> {
        self.active.borrow().as_ref().map(|active| active.turn.oid)
    }

    fn clear_buffer(&self) {
        self.response_buffer.borrow_mut().clear();
    }

    fn push_response_chunk(&self, text: &str) {
        self.response_buffer.borrow_mut().push_str(text);
    }

    fn record_tool_call(&self, tool_call: &AcpToolCall) -> Result<()> {
        self.finish_response()?;

        self.amend_active(|message| {
            message.with_entry(TurnEntry::tool_call_now(turn_tool_kind(tool_call.kind)))
        })
    }

    fn amend_active(&self, update_message: impl FnOnce(TurnMessage) -> TurnMessage) -> Result<()> {
        if let Some(current) = self.active.borrow().as_ref().cloned() {
            let message = update_message(current.turn.message().clone());
            let updated = session::amend(&current.session, &message)?;
            let _ = snapshot::capture(&current.session, updated.oid, SnapshotReason::TurnAmend)?;

            *self.active.borrow_mut() = Some(ActiveTurn {
                turn: updated,
                ..current
            });
        }
        Ok(())
    }

    fn new_message(&self, session: &Session) -> Result<TurnMessage> {
        Ok(TurnMessage::new(session.id.clone()).with_agent_name(self.agent_name.clone())?)
    }
}

fn turn_tool_kind(kind: ToolKind) -> TurnToolKind {
    match kind {
        ToolKind::Read => TurnToolKind::Read,
        ToolKind::Edit => TurnToolKind::Edit,
        ToolKind::Delete => TurnToolKind::Delete,
        ToolKind::Move => TurnToolKind::Move,
        ToolKind::Search => TurnToolKind::Search,
        ToolKind::Execute => TurnToolKind::Execute,
        ToolKind::Think => TurnToolKind::Think,
        ToolKind::Fetch => TurnToolKind::Fetch,
        ToolKind::SwitchMode => TurnToolKind::SwitchMode,
        _ => TurnToolKind::Other,
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::rc::Rc;

    use concats_core::{Oid, session};

    use super::*;

    fn init_repo_with_commit(dir: &std::path::Path) -> git2::Repository {
        let repo = git2::Repository::init(dir).unwrap();
        let mut index = repo.index().unwrap();
        std::fs::write(dir.join("init.txt"), "init").unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("test", "test@test").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        repo
    }

    #[test]
    fn start_prompt_sets_subject_from_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Rc::new(init_repo_with_commit(dir.path()));
        let head = Oid::from(repo.head().unwrap().target().unwrap());
        let session = session::create(repo, "session-a", head).unwrap();
        let recorder = TurnRecorder::new("Claude");

        let turn = recorder
            .start_prompt(&session, "  hello \n world  ")
            .unwrap();

        assert_eq!(turn.subject(), "hello world");
    }
}
