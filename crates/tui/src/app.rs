use std::collections::HashMap;
use std::path::PathBuf;

use agent_client_protocol::{
    ContentBlock, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOptions, SessionNotification, SessionUpdate,
};
use concats_core::session::{SessionEvent, SessionHandle};
use ratatui_textarea::TextArea;
use tokio::sync::mpsc;

use crate::tabs::{ActiveTab, SessionsTabState, TabBarEntry};

/// Events arriving on the fan-in channel. Wraps `SessionEvent` and adds
/// a channel-closed signal so the TUI can mark tabs as ended.
pub enum FanInEvent {
    Session(SessionEvent),
    ChannelClosed,
}

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

/// State for the agent picker overlay.
pub struct AgentPickerState {
    /// Available agents: (id, display_name).
    pub agents: Vec<(String, String)>,
    /// Currently highlighted index.
    pub selected: usize,
}

/// Per-session state. Each open session tab holds one of these.
pub struct SessionTab<'a> {
    pub id: u32,
    pub label: String,
    pub session: SessionHandle,
    pub messages: Vec<Message>,
    pub textarea: TextArea<'a>,
    pub status: String,
    pub waiting: bool,
    pub scroll_offset: u16,
    pub stderr_lines: Vec<String>,
    pub stderr_scroll: u16,
    pub show_stderr: bool,
    pub focused_panel: FocusedPanel,
    pub agent_label: String,
    pub current_model: Option<String>,
    pub current_mode: Option<String>,
    pub pending_fork_message: Option<String>,
    /// Agent config needed to fork/recreate sessions of the same type.
    pub agent_command: String,
    pub agent_args: Vec<String>,
    pub agent_env: HashMap<String, String>,
    /// Whether to auto-push session refs after each checkpoint.
    pub auto_push: bool,
    /// Git remote name for auto-push.
    pub push_remote: String,
}

impl<'a> SessionTab<'a> {
    pub fn new(id: u32, label: String, session: SessionHandle) -> Self {
        let mut textarea = TextArea::default();
        textarea.set_placeholder_text("Type a prompt and press Enter...");
        textarea.set_cursor_line_style(ratatui::style::Style::default());

        Self {
            id,
            label,
            session,
            messages: vec![Message::System(
                "Session started. Type a prompt and press Enter.".into(),
            )],
            textarea,
            status: "connected".into(),
            waiting: false,
            scroll_offset: 0,
            stderr_lines: Vec::new(),
            stderr_scroll: 0,
            show_stderr: false,
            focused_panel: FocusedPanel::Conversation,
            agent_label: String::from("agent"),
            current_model: None,
            current_mode: None,
            pending_fork_message: None,
            agent_command: String::new(),
            agent_args: Vec::new(),
            agent_env: HashMap::new(),
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

    /// Queue a one-shot message that is prepended to the next user prompt.
    pub fn queue_fork_message(
        &mut self,
        source_session_id: &str,
        source_turn: u32,
        commit_oid: git2::Oid,
    ) {
        let commit = commit_oid.to_string();
        let short_commit: String = commit.chars().take(12).collect();
        let ref_path = format!("refs/agent/sessions/{source_session_id}");
        self.pending_fork_message = Some(format!(
            "<session_context>\n\
             Forked from session {source_session_id} at turn {source_turn} (commit {short_commit}).\n\
             Prior conversation and file changes: {ref_path}\n\
             </session_context>"
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
            SessionEvent::Stderr(line) => {
                self.stderr_lines.push(line);
                // Auto-show stderr on first output.
                if !self.show_stderr && self.stderr_lines.len() == 1 {
                    self.show_stderr = true;
                }
            }
            SessionEvent::PushFailed { ref_name, error } => {
                self.messages.push(Message::System(format!(
                    "Push failed for {ref_name}: {error}"
                )));
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

/// Application state for the TUI.
pub struct App<'a> {
    /// All open session tabs.
    pub session_tabs: Vec<SessionTab<'a>>,
    /// Currently active tab.
    pub active_tab: ActiveTab,
    /// Counter for assigning unique session tab IDs.
    pub next_session_id: u32,
    pub should_quit: bool,
    pub tick: usize,
    /// State for the Sessions (history) tab.
    pub sessions_state: SessionsTabState,
    /// Workspace root directory.
    pub workspace_root: PathBuf,
    /// All available agents from config (id, AgentConfig).
    pub available_agents: Vec<(String, concats_config::AgentConfig)>,
    /// Agent picker overlay state (None when hidden).
    pub agent_picker: Option<AgentPickerState>,
    /// Whether to auto-push session refs after each checkpoint.
    pub auto_push: bool,
    /// Git remote name for auto-push.
    pub push_remote: String,
    /// Fan-in channel sender for all session events (tagged with session ID).
    pub session_event_tx: mpsc::UnboundedSender<(u32, FanInEvent)>,
    /// Fan-in channel receiver for all session events.
    pub session_event_rx: mpsc::UnboundedReceiver<(u32, FanInEvent)>,
}

impl<'a> App<'a> {
    pub fn new(workspace_root: PathBuf, available_agents: Vec<(String, concats_config::AgentConfig)>) -> Self {
        let sessions_state = SessionsTabState::new(workspace_root.clone());
        let (session_event_tx, session_event_rx) = mpsc::unbounded_channel();

        Self {
            session_tabs: Vec::new(),
            active_tab: ActiveTab::Sessions, // will switch once first session is added
            next_session_id: 0,
            should_quit: false,
            tick: 0,
            sessions_state,
            workspace_root,
            available_agents,
            agent_picker: None,
            auto_push: false,
            push_remote: String::from("origin"),
            session_event_tx,
            session_event_rx,
        }
    }

    /// Get the active session tab (if the active tab is a session).
    pub fn active_session(&self) -> Option<&SessionTab<'a>> {
        if let ActiveTab::Session(id) = self.active_tab {
            self.session_tabs.iter().find(|t| t.id == id)
        } else {
            None
        }
    }

    /// Get the active session tab mutably (if the active tab is a session).
    pub fn active_session_mut(&mut self) -> Option<&mut SessionTab<'a>> {
        if let ActiveTab::Session(id) = self.active_tab {
            self.session_tabs.iter_mut().find(|t| t.id == id)
        } else {
            None
        }
    }

    /// Add a new session tab. Returns the new tab's ID.
    ///
    /// Spawns a forwarder task that reads from the session's `event_rx` and
    /// writes tagged events into the shared fan-in channel.
    pub fn add_session(
        &mut self,
        mut session: SessionHandle,
        label: String,
        agent_id: &str,
        agent_config: &concats_config::AgentConfig,
    ) -> u32 {
        let id = self.next_session_id;
        self.next_session_id += 1;

        let final_label = self.deduplicate_label(&label);

        // Take the event_rx out of the handle *before* moving it into the tab.
        // The forwarder task will own it; the tab retains prompt_tx and cancel_tx.
        let (placeholder_tx, placeholder_rx) = mpsc::unbounded_channel();
        let session_rx = std::mem::replace(&mut session.event_rx, placeholder_rx);
        drop(placeholder_tx);

        let mut tab = SessionTab::new(id, final_label, session);
        tab.agent_command = agent_config.command.clone();
        tab.agent_args = agent_config.args.clone();
        tab.agent_env = agent_config.env.clone();
        tab.agent_label = if !agent_config.name.trim().is_empty() {
            agent_config.name.clone()
        } else {
            agent_id.to_string()
        };
        tab.auto_push = self.auto_push;
        tab.push_remote = self.push_remote.clone();

        self.session_tabs.push(tab);

        // Spawn forwarder: tags each event with the session ID and sends it
        // into the fan-in channel. When the session's channel closes, sends
        // a ChannelClosed signal so the app can mark the tab as ended.
        let fan_in_tx = self.session_event_tx.clone();
        tokio::spawn(async move {
            let mut rx = session_rx;
            while let Some(event) = rx.recv().await {
                if fan_in_tx.send((id, FanInEvent::Session(event))).is_err() {
                    break;
                }
            }
            let _ = fan_in_tx.send((id, FanInEvent::ChannelClosed));
        });

        id
    }

    /// Close a session tab by ID.
    ///
    /// If the closed tab was active, switches to the nearest adjacent session
    /// tab, or falls back to the Sessions (history) utility tab.
    pub fn close_session(&mut self, id: u32) {
        let old_index = self.session_tabs.iter().position(|t| t.id == id);
        self.session_tabs.retain(|t| t.id != id);

        if self.active_tab == ActiveTab::Session(id) {
            // Try to stay near the same position in the tab list.
            let neighbor = old_index
                .and_then(|idx| {
                    // Prefer the tab that slid into the same index, else the one before.
                    self.session_tabs
                        .get(idx)
                        .or_else(|| self.session_tabs.last())
                })
                .map(|t| t.id);

            self.active_tab = match neighbor {
                Some(neighbor_id) => ActiveTab::Session(neighbor_id),
                None => ActiveTab::Sessions,
            };
        }
    }

    /// Build the ordered list of tab bar entries for rendering.
    pub fn tab_bar_entries(&self) -> Vec<TabBarEntry> {
        let mut entries = Vec::new();

        // Session tabs first.
        for tab in &self.session_tabs {
            entries.push(TabBarEntry::Session {
                id: tab.id,
                label: tab.label.clone(),
            });
        }

        // [+] new session button.
        entries.push(TabBarEntry::NewButton);

        // Utility tabs.
        entries.push(TabBarEntry::Utility {
            tab: ActiveTab::Sessions,
            label: "Sessions",
        });
        entries.push(TabBarEntry::Utility {
            tab: ActiveTab::Settings,
            label: "Settings",
        });
        entries.push(TabBarEntry::Utility {
            tab: ActiveTab::Help,
            label: "Help",
        });

        entries
    }

    /// Switch to a different tab. Refreshes sessions list when switching to Sessions.
    pub fn switch_tab(&mut self, tab: ActiveTab) {
        self.active_tab = tab;
        if tab == ActiveTab::Sessions {
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

    /// Deduplicate a label by appending a counter if needed.
    fn deduplicate_label(&self, label: &str) -> String {
        let exists = self.session_tabs.iter().any(|t| t.label == label);
        if !exists {
            return label.to_string();
        }

        for i in 2.. {
            let candidate = format!("{label} ({i})");
            if !self.session_tabs.iter().any(|t| t.label == candidate) {
                return candidate;
            }
        }
        unreachable!()
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
