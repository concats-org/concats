use std::{cell::RefCell, rc::Rc};

use agent_client_protocol::{
    ContentBlock, SessionNotification, SessionUpdate, ToolCall as AcpToolCall,
};
use concats_core::{
    Oid,
    checkpoint::{Checkpoint, Draft, TranscriptEntry},
    error::Result as CoreResult,
    session::{self, Session},
};
use serde_json::Value;

#[derive(Clone)]
struct ActiveCheckpoint {
    session: Session,
    checkpoint: Checkpoint,
}

#[derive(Clone, Default)]
pub struct CheckpointRecorder {
    active: Rc<RefCell<Option<ActiveCheckpoint>>>,
    response_buffer: Rc<RefCell<String>>,
}

impl CheckpointRecorder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_prompt(&self, session: &Session, prompt_text: &str) {
        self.clear_buffer();
        let updated: CoreResult<Checkpoint> = (|| {
            let mut draft = Draft::new();
            draft
                .transcript
                .append(TranscriptEntry::prompt_now(prompt_text))?;
            session::commit(session, &draft)
        })();

        match updated {
            Ok(updated) => {
                *self.active.borrow_mut() = Some(ActiveCheckpoint {
                    session: session.clone(),
                    checkpoint: updated,
                });
            }
            Err(error) => tracing::warn!("failed to record prompt checkpoint: {error}"),
        }
    }

    pub fn handle_notification(&self, notification: &SessionNotification) {
        match &notification.update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let ContentBlock::Text(text) = &chunk.content {
                    self.push_response_chunk(&text.text);
                }
            }
            SessionUpdate::ToolCall(tool_call) => self.record_tool_call(tool_call),
            _ => {}
        }
    }

    pub fn finish_response(&self) {
        let text = {
            let mut buffer = self.response_buffer.borrow_mut();
            if buffer.trim().is_empty() {
                buffer.clear();
                return;
            }
            std::mem::take(&mut *buffer)
        };

        self.amend_active("checkpoint response amend failed", |draft| {
            draft.transcript.append(TranscriptEntry::response_now(text))
        });
    }

    pub fn snapshot_after_tool_write(&self) {
        self.amend_active("checkpoint snapshot amend failed", |_| Ok(()));
    }

    #[must_use]
    pub fn current_oid(&self) -> Option<Oid> {
        self.active
            .borrow()
            .as_ref()
            .map(|active| active.checkpoint.oid)
    }

    fn clear_buffer(&self) {
        self.response_buffer.borrow_mut().clear();
    }

    fn push_response_chunk(&self, text: &str) {
        self.response_buffer.borrow_mut().push_str(text);
    }

    fn record_tool_call(&self, tool_call: &AcpToolCall) {
        self.finish_response();

        let name = if tool_call.title.is_empty() {
            format!("{:?}", tool_call.kind)
        } else {
            tool_call.title.clone()
        };
        let payload = serde_json::to_value(tool_call).unwrap_or(Value::Null);
        self.amend_active("checkpoint tool-call amend failed", |draft| {
            draft
                .transcript
                .append(TranscriptEntry::tool_call_now(name, payload))
        });
    }

    fn amend_active(
        &self,
        error_message: &str,
        update_draft: impl FnOnce(&mut Draft) -> CoreResult<()>,
    ) {
        if let Some(current) = self.active.borrow().as_ref().cloned() {
            let mut draft = Draft::from_checkpoint(&current.checkpoint);
            match update_draft(&mut draft).and_then(|()| session::amend(&current.session, &draft)) {
                Ok(updated) => {
                    *self.active.borrow_mut() = Some(ActiveCheckpoint {
                        checkpoint: updated,
                        ..current
                    });
                }
                Err(error) => tracing::warn!("{error_message}: {error}"),
            }
        }
    }
}
