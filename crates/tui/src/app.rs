use std::path::PathBuf;

use agent_client_protocol::{
    ContentBlock, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOptions, SessionNotification, SessionUpdate,
};
use concats_core::session::{SessionEvent, SessionHandle};
use ratatui_textarea::TextArea;

use crate::tabs::{SessionsTabState, Tab};

/// A message in the conversation log.
pub enum Message {
    User(String),
    Agent(String),
    System(String),
}

/// Which panel currently has focus.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusedPanel {
    Conversation,
    Stderr,
}

/// Describes a fork request from the Sessions tab.
pub struct ForkRequest {
    pub commit_oid: git2::Oid,
    pub source_session_id: String,
    pub source_turn: u32,
}

/// Application state for the TUI.
pub struct App<'a> {
    pub session: SessionHandle,
    pub messages: Vec<Message>,
    pub textarea: TextArea<'a>,
    pub status: String,
    pub waiting: bool,
    pub scroll_offset: u16,
    pub should_quit: bool,
    pub tick: usize,
    /// Lines of stderr output from the agent process.
    pub stderr_lines: Vec<String>,
    /// Scroll offset for the stderr panel.
    pub stderr_scroll: u16,
    /// Whether the stderr panel is visible.
    pub show_stderr: bool,
    /// Which panel currently has focus (for scrolling).
    pub focused_panel: FocusedPanel,
    /// Currently active tab.
    pub active_tab: Tab,
    /// State for the Sessions tab.
    pub sessions_state: SessionsTabState,
    /// Workspace root for the current session.
    pub workspace_root: PathBuf,
    /// Agent config needed to start new sessions when forking.
    pub agent_command: String,
    pub agent_args: Vec<String>,
    pub agent_env: std::collections::HashMap<String, String>,
    /// Agent/model label shown in the input title.
    pub agent_label: String,
    /// Current model label if exposed by the agent.
    pub current_model: Option<String>,
    /// Current mode label if exposed by the agent.
    pub current_mode: Option<String>,
    /// One-shot fork context appended to the next submitted prompt.
    pub pending_fork_message: Option<String>,
    /// Whether to auto-push session refs after each checkpoint.
    pub auto_push: bool,
    /// Git remote name for auto-push.
    pub push_remote: String,
}

impl<'a> App<'a> {
    pub fn new(session: SessionHandle, workspace_root: PathBuf) -> Self {
        let mut textarea = TextArea::default();
        textarea.set_placeholder_text("Type a prompt and press Enter...");
        textarea.set_cursor_line_style(ratatui::style::Style::default());

        let sessions_state = SessionsTabState::new(workspace_root.clone());

        Self {
            session,
            messages: vec![Message::System(
                "Session started. Type a prompt and press Enter.".into(),
            )],
            textarea,
            status: "connected".into(),
            waiting: false,
            scroll_offset: 0,
            should_quit: false,
            tick: 0,
            stderr_lines: Vec::new(),
            stderr_scroll: 0,
            show_stderr: false,
            focused_panel: FocusedPanel::Conversation,
            active_tab: Tab::Agent,
            sessions_state,
            workspace_root,
            agent_command: String::new(),
            agent_args: Vec::new(),
            agent_env: std::collections::HashMap::new(),
            agent_label: String::from("agent"),
            current_model: None,
            current_mode: None,
            pending_fork_message: None,
            auto_push: false,
            push_remote: String::from("origin"),
        }
    }

    pub fn input_title(&self) -> String {
        let primary = self
            .current_model
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.agent_label);

        if let Some(mode) = self.current_mode.as_deref().filter(|s| !s.is_empty()) {
            format!("{primary} - {mode}")
        } else {
            primary.to_string()
        }
    }

    /// Switch to a different tab. Refreshes sessions list when switching to Sessions.
    pub fn switch_tab(&mut self, tab: Tab) {
        self.active_tab = tab;
        if tab == Tab::Sessions {
            self.sessions_state.refresh();
        }
    }

    /// Attempt to fork from the currently selected turn in the Sessions tab.
    pub fn fork_from_selected_turn(&self) -> Option<ForkRequest> {
        let commit_oid = self.sessions_state.selected_turn_oid()?;
        let (session_id, turn) = self.sessions_state.selected_fork_info()?;
        Some(ForkRequest {
            commit_oid,
            source_session_id: session_id,
            source_turn: turn,
        })
    }

    /// Queue a one-shot message that is appended to the next user prompt.
    pub fn queue_fork_message(
        &mut self,
        source_session_id: &str,
        source_turn: u32,
        commit_oid: git2::Oid,
    ) {
        let commit = commit_oid.to_string();
        let short_commit: String = commit.chars().take(12).collect();
        self.pending_fork_message = Some(format!(
            "Fork context: This session was forked from session {source_session_id} at turn {source_turn} (commit {short_commit}). Old messages and context are available at refs/agent/sessions/{source_session_id}."
        ));
    }

    /// Send the current textarea content as a prompt.
    pub async fn send_prompt(&mut self) {
        let base_text: String = self.textarea.lines().join("\n");
        if base_text.trim().is_empty() || self.waiting {
            return;
        }
        let text = if let Some(fork_message) = self.pending_fork_message.take() {
            format!("{fork_message}\n\n{base_text}")
        } else {
            base_text
        };

        // Clear the textarea.
        self.textarea = TextArea::default();
        self.textarea
            .set_placeholder_text("Type a prompt and press Enter...");
        self.textarea
            .set_cursor_line_style(ratatui::style::Style::default());

        self.messages.push(Message::User(text.clone()));
        self.waiting = true;
        self.status = "waiting for agent...".into();

        if self.session.prompt_tx.send(text).await.is_err() {
            self.messages.push(Message::System(
                "Failed to send prompt (session closed).".into(),
            ));
            self.waiting = false;
            self.status = "disconnected".into();
        }
    }

    /// Handle an incoming session event.
    pub fn handle_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::SessionConfigured {
                mode,
                config_options,
            } => {
                self.update_session_labels(&config_options);
                if let Some(mode) = mode {
                    self.current_mode = Some(mode);
                }
            }
            SessionEvent::Notification(notification) => {
                self.handle_notification(*notification);
            }
            SessionEvent::TurnComplete {
                stop_reason,
                commit_oid,
            } => {
                self.waiting = false;
                self.status = format!("done ({stop_reason:?})");
                if let Some(oid) = commit_oid {
                    self.messages
                        .push(Message::System(format!("Checkpoint: {}", oid.short())));
                }
            }
            SessionEvent::PushFailed { ref_name, error } => {
                self.messages.push(Message::System(format!(
                    "Warning: failed to push {ref_name}: {error}"
                )));
            }
            SessionEvent::Stderr(line) => {
                self.stderr_lines.push(line);
                // Auto-show stderr on first output.
                if !self.show_stderr && self.stderr_lines.len() == 1 {
                    self.show_stderr = true;
                }
            }
            SessionEvent::Error(err) => {
                self.waiting = false;
                self.status = "error".into();
                self.messages.push(Message::System(format!("Error: {err}")));
            }
        }
    }

    fn handle_notification(&mut self, notification: SessionNotification) {
        match notification.update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                let text = match &chunk.content {
                    ContentBlock::Text(t) => t.text.clone(),
                    _ => return,
                };

                match self.messages.last_mut() {
                    Some(Message::Agent(existing)) => {
                        existing.push_str(&text);
                    }
                    _ => {
                        self.messages.push(Message::Agent(text));
                    }
                }
            }
            SessionUpdate::ToolCall(tc) => {
                self.messages
                    .push(Message::System(format!("Tool: {}", tc.title)));
            }
            SessionUpdate::CurrentModeUpdate(mode_update) => {
                self.current_mode = Some(mode_update.current_mode_id.to_string());
            }
            SessionUpdate::ConfigOptionUpdate(config_update) => {
                self.update_session_labels(&config_update.config_options);
            }
            _ => {}
        }
    }

    fn update_session_labels(&mut self, options: &[SessionConfigOption]) {
        for option in options {
            let Some(label) = current_select_label(option) else {
                continue;
            };

            match option.category {
                Some(SessionConfigOptionCategory::Model) => {
                    self.current_model = Some(label);
                }
                Some(SessionConfigOptionCategory::Mode) => {
                    self.current_mode = Some(label);
                }
                _ => {
                    let name = option.name.to_lowercase();
                    if name.contains("model") {
                        self.current_model = Some(label.clone());
                    }
                    if name.contains("mode") {
                        self.current_mode = Some(label);
                    }
                }
            }
        }
    }
}

fn current_select_label(option: &SessionConfigOption) -> Option<String> {
    let SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };

    match &select.options {
        SessionConfigSelectOptions::Ungrouped(values) => values
            .iter()
            .find(|v| v.value == select.current_value)
            .map(|v| v.name.clone())
            .or_else(|| Some(select.current_value.to_string())),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter())
            .find(|v| v.value == select.current_value)
            .map(|v| v.name.clone())
            .or_else(|| Some(select.current_value.to_string())),
        _ => Some(select.current_value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::{SessionNotification, StopReason};
    use catena_core::session::SessionEvent;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::path::PathBuf;
    use tokio::sync::mpsc;

    fn create_test_session() -> SessionHandle {
        let (prompt_tx, _) = mpsc::channel(1);
        let (_, event_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _) = mpsc::channel(1);
        SessionHandle {
            prompt_tx,
            event_rx,
            cancel_tx,
        }
    }

    #[test]
    fn test_app_initial_state() {
        let session = create_test_session();
        let workspace = PathBuf::from("/tmp");
        let app = App::new(session, workspace);

        assert_eq!(app.status, "connected");
        assert_eq!(app.waiting, false);
        assert_eq!(app.messages.len(), 1);
        match &app.messages[0] {
            Message::System(s) => assert!(s.contains("Session started")),
            _ => panic!("Expected system message"),
        }
        assert_eq!(app.active_tab, Tab::Agent);
    }

    #[test]
    fn test_app_switch_tab() {
        let session = create_test_session();
        let workspace = PathBuf::from("/tmp");
        let mut app = App::new(session, workspace);

        app.switch_tab(Tab::Sessions);
        assert_eq!(app.active_tab, Tab::Sessions);

        app.switch_tab(Tab::Agent);
        assert_eq!(app.active_tab, Tab::Agent);
    }

    #[test]
    fn test_handle_turn_complete() {
        let session = create_test_session();
        let mut app = App::new(session, PathBuf::from("/tmp"));
        app.waiting = true;

        let event = SessionEvent::TurnComplete {
            stop_reason: StopReason::EndTurn,
            commit_oid: None,
        };

        app.handle_session_event(event);

        assert_eq!(app.waiting, false);
        assert!(app.status.contains("done"));
    }

    #[test]
    fn test_handle_stderr() {
        let session = create_test_session();
        let mut app = App::new(session, PathBuf::from("/tmp"));
        assert_eq!(app.show_stderr, false);

        app.handle_session_event(SessionEvent::Stderr("debug log".into()));

        assert_eq!(app.stderr_lines.len(), 1);
        assert_eq!(app.stderr_lines[0], "debug log");
        assert_eq!(app.show_stderr, true);
    }
}
