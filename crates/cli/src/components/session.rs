use std::collections::HashMap;

use agent_client_protocol::{
    ContentBlock, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOptions, SessionNotification, SessionUpdate, ToolCall,
};
use concats_acp::{SessionEvent, SessionHandle};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Margin, Rect, Spacing},
    style::{Color, Modifier, Style},
    symbols::{merge::MergeStrategy, scrollbar::Set},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Widget, Wrap,
    },
};
use ratatui_textarea::TextArea;
use tokio::sync::{mpsc, mpsc::error::TrySendError};
use tui_widget_list::{ListBuilder, ListView};

use crate::{action::Action, components::Component, launch::SessionTabConfig};

pub const AGENT_INPUT_HEIGHT: u16 = 4;
pub const FORK_HINT_MIN_BLOCK_HEIGHT: u16 = 3;
pub const FORK_HINT_MAX_BLOCK_HEIGHT: u16 = 9;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusedPanel {
    Conversation,
    Stderr,
}

pub enum Message {
    User(String),
    Agent(String),
    System(String),
    ToolCall(ToolCall),
}

pub enum SessionLifecycle {
    Active,
    CloseRequested,
    Closed,
}

pub struct SessionModel {
    pub messages: Vec<Message>,
    pub status: String,
    pub waiting: bool,
    pub agent_label: String,
    pub current_model: Option<String>,
    pub current_mode: Option<String>,
    pub pending_fork_message: Option<String>,
    pub agent_command: String,
    pub agent_args: Vec<String>,
    pub agent_env: HashMap<String, String>,
    pub auto_push: bool,
    pub push_remote: String,
    pub lifecycle: SessionLifecycle,
}

pub struct SessionViewState {
    pub textarea: TextArea<'static>,
    pub conversation_list: tui_widget_list::ListState,
    pub stderr_lines: Vec<String>,
    pub stderr_scroll: u16,
    pub show_stderr: bool,
    pub focused_panel: FocusedPanel,
}

pub struct SessionComponent {
    action_tx: Option<mpsc::UnboundedSender<Action>>,
    id: u32,
    label: String,
    session: SessionHandle,
    model: SessionModel,
    view: SessionViewState,
}

impl SessionComponent {
    #[must_use]
    pub fn new(id: u32, label: String, session: SessionHandle, config: SessionTabConfig) -> Self {
        Self {
            action_tx: None,
            id,
            label,
            session,
            model: SessionModel {
                messages: vec![Message::System(
                    "Session started. Type a prompt and press Enter.".into(),
                )],
                status: "connected".into(),
                waiting: false,
                agent_label: config.agent_label,
                current_model: None,
                current_mode: None,
                pending_fork_message: None,
                agent_command: config.agent_command,
                agent_args: config.agent_args,
                agent_env: config.agent_env,
                auto_push: config.auto_push,
                push_remote: config.push_remote,
                lifecycle: SessionLifecycle::Active,
            },
            view: SessionViewState {
                textarea: new_textarea(),
                conversation_list: tui_widget_list::ListState::default(),
                stderr_lines: Vec::new(),
                stderr_scroll: 0,
                show_stderr: false,
                focused_panel: FocusedPanel::Conversation,
            },
        }
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn set_label(&mut self, label: String) {
        self.label = label;
    }

    pub fn waiting(&self) -> bool {
        self.model.waiting
    }

    pub fn status(&self) -> &str {
        &self.model.status
    }

    pub fn agent_label(&self) -> &str {
        &self.model.agent_label
    }

    pub fn agent_command(&self) -> &str {
        &self.model.agent_command
    }

    pub fn agent_args(&self) -> &[String] {
        &self.model.agent_args
    }

    pub fn agent_env(&self) -> &HashMap<String, String> {
        &self.model.agent_env
    }

    pub fn auto_push(&self) -> bool {
        self.model.auto_push
    }

    pub fn push_remote(&self) -> &str {
        &self.model.push_remote
    }

    pub fn session_handle(&self) -> &SessionHandle {
        &self.session
    }

    pub fn session_handle_mut(&mut self) -> &mut SessionHandle {
        &mut self.session
    }

    pub fn input_title(&self) -> String {
        let primary = self
            .model
            .current_model
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.model.agent_label);

        if let Some(mode) = self
            .model
            .current_mode
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            format!("{primary} - {mode}")
        } else {
            primary.to_string()
        }
    }

    pub fn queue_fork_message(&mut self, source_session_id: &str, turn_oid: concats_core::Oid) {
        let turn = turn_oid.to_string();
        let short_turn: String = turn.chars().take(12).collect();
        let ref_path = format!("refs/agent/sessions/{source_session_id}");
        self.model.pending_fork_message = Some(format!(
            "<session_context>\n\
             Forked from session {source_session_id} at turn {short_turn}.\n\
             Prior conversation and file changes: {ref_path}\n\
             </session_context>"
        ));
    }

    pub async fn send_prompt(&mut self) {
        let base_text = self.view.textarea.lines().join("\n");
        if base_text.trim().is_empty()
            || self.model.waiting
            || !matches!(self.model.lifecycle, SessionLifecycle::Active)
        {
            return;
        }

        let text = if let Some(fork_message) = self.model.pending_fork_message.take() {
            format!("{fork_message}\n\n{base_text}")
        } else {
            base_text
        };

        self.view.textarea = new_textarea();
        self.model.messages.push(Message::User(text.clone()));
        self.model.waiting = true;
        self.model.status = "waiting for agent...".into();

        if self.session.prompt_tx.send(text).await.is_err() {
            self.model.messages.push(Message::System(
                "Failed to send prompt (session closed).".into(),
            ));
            self.model.waiting = false;
            self.model.status = "disconnected".into();
        }
    }

    pub fn handle_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::SessionConfigured {
                mode,
                config_options,
            } => {
                self.update_session_labels(&config_options);
                if let Some(mode) = mode {
                    self.model.current_mode = Some(mode);
                }
            }
            SessionEvent::Notification(notification) => {
                self.handle_notification(*notification);
            }
            SessionEvent::TurnComplete {
                stop_reason,
                turn_oid,
            } => {
                self.model.waiting = false;
                self.model.status = format!("done ({stop_reason:?})");
                if let Some(oid) = turn_oid {
                    self.model
                        .messages
                        .push(Message::System(format!("Turn: {}", oid.short())));
                }
            }
            SessionEvent::Stderr(line) => {
                self.view.stderr_lines.push(line);
                if !self.view.show_stderr && self.view.stderr_lines.len() == 1 {
                    self.view.show_stderr = true;
                }
            }
            SessionEvent::PushFailed { ref_name, error } => {
                self.model.messages.push(Message::System(format!(
                    "Push failed for {ref_name}: {error}"
                )));
            }
            SessionEvent::Error(err) => {
                self.model.waiting = false;
                self.model.status = "error".into();
                self.model
                    .messages
                    .push(Message::System(format!("Error: {err}")));
            }
        }
    }

    pub fn push_system_message(&mut self, message: impl Into<String>) {
        self.model.messages.push(Message::System(message.into()));
    }

    pub fn request_close(&mut self) -> bool {
        match self.model.lifecycle {
            SessionLifecycle::Closed => return false,
            SessionLifecycle::CloseRequested => return true,
            SessionLifecycle::Active => {}
        }

        self.model.lifecycle = SessionLifecycle::CloseRequested;
        self.model.waiting = false;
        self.model.status = "closing session...".into();
        self.push_system_message("Closing session...");

        match self.session.cancel_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => true,
            Err(TrySendError::Closed(())) => false,
        }
    }

    pub fn close_requested(&self) -> bool {
        matches!(self.model.lifecycle, SessionLifecycle::CloseRequested)
    }

    pub fn mark_closed(&mut self) {
        self.model.lifecycle = SessionLifecycle::Closed;
        self.model.waiting = false;
        self.model.status = "session ended".into();
    }

    fn send_action(&self, action: Action) {
        if let Some(action_tx) = &self.action_tx {
            let _ = action_tx.send(action);
        }
    }

    fn is_submit_key(&self, key: KeyEvent) -> bool {
        key.code == KeyCode::Enter
            && !key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
            && !self.model.waiting
    }

    fn is_newline_key(&self, key: KeyEvent) -> bool {
        key.code == KeyCode::Enter
            && key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
            && !self.model.waiting
    }

    fn handle_navigation_key(&self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.send_action(Action::SessionToggleStderr(self.id));
                true
            }
            KeyCode::Tab if self.view.show_stderr => {
                self.send_action(Action::SessionCycleFocus(self.id));
                true
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.send_focused_scroll(-1);
                true
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.send_focused_scroll(1);
                true
            }
            KeyCode::PageUp => {
                self.send_focused_scroll(-8);
                true
            }
            KeyCode::PageDown => {
                self.send_focused_scroll(8);
                true
            }
            _ => false,
        }
    }

    fn send_focused_scroll(&self, delta: i16) {
        match self.view.focused_panel {
            FocusedPanel::Conversation => self.send_action(Action::SessionScrollConversation {
                tab_id: self.id,
                delta,
            }),
            FocusedPanel::Stderr => self.send_action(Action::SessionScrollStderr {
                tab_id: self.id,
                delta,
            }),
        }
    }

    fn render_output_panels(&mut self, frame: &mut Frame, layout: &SessionLayout) {
        if let Some(stderr) = layout.stderr {
            render_conversation_list(
                frame,
                &self.model.messages,
                &mut self.view.conversation_list,
                layout.conversation_panel.expect("conversation panel"),
            );

            let stderr_lines: Vec<Line> = self
                .view
                .stderr_lines
                .iter()
                .map(|line| {
                    Line::from(Span::styled(line.as_str(), Style::default().fg(Color::Red)))
                })
                .collect();
            let stderr_border_style = if self.view.focused_panel == FocusedPanel::Stderr {
                Style::default().fg(Color::Blue)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let stderr_panel = Paragraph::new(stderr_lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Stderr")
                        .border_style(stderr_border_style),
                )
                .wrap(Wrap { trim: false })
                .scroll((self.view.stderr_scroll, 0));
            frame.render_widget(stderr_panel, stderr);

            let mut scrollbar_state = ScrollbarState::default()
                .content_length(self.view.stderr_lines.len())
                .position(usize::from(self.view.stderr_scroll));
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight).symbols(Set {
                    track: "│",
                    thumb: "█",
                    begin: "▲",
                    end: "▼",
                }),
                stderr.inner(Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut scrollbar_state,
            );
        } else {
            render_conversation_list(
                frame,
                &self.model.messages,
                &mut self.view.conversation_list,
                layout.conversation,
            );
        }
    }

    fn render_input_panel(&mut self, frame: &mut Frame, layout: &SessionLayout) {
        if let Some(fork_message) = self.model.pending_fork_message.as_deref() {
            let input_block = Block::bordered()
                .border_type(BorderType::Rounded)
                .merge_borders(MergeStrategy::Exact)
                .title(self.input_title());
            let hint = Paragraph::new(Text::styled(
                fork_message,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ))
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .merge_borders(MergeStrategy::Exact)
                    .title(
                        Line::from(Span::styled(
                            "fork context pending",
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::DIM),
                        ))
                        .right_aligned(),
                    ),
            )
            .wrap(Wrap { trim: false });
            frame.render_widget(hint, layout.fork_hint.expect("fork hint"));

            if self.model.waiting {
                frame.render_widget(
                    Paragraph::new(" (waiting for agent...)").block(input_block),
                    layout.input,
                );
            } else {
                self.view.textarea.set_block(input_block);
                frame.render_widget(&self.view.textarea, layout.input);
            }
            return;
        }

        if self.model.waiting {
            frame.render_widget(
                Paragraph::new(" (waiting for agent...)").block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .title(self.input_title()),
                ),
                layout.input,
            );
        } else {
            self.view.textarea.set_block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(self.input_title()),
            );
            frame.render_widget(&self.view.textarea, layout.input);
        }
    }

    fn handle_notification(&mut self, notification: SessionNotification) {
        match notification.update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                let text = match &chunk.content {
                    ContentBlock::Text(text) => text.text.clone(),
                    _ => return,
                };

                match self.model.messages.last_mut() {
                    Some(Message::Agent(existing)) => existing.push_str(&text),
                    _ => self.model.messages.push(Message::Agent(text)),
                }
            }
            SessionUpdate::ToolCall(tool_call) => {
                self.model.messages.push(Message::ToolCall(tool_call));
            }
            SessionUpdate::CurrentModeUpdate(mode_update) => {
                self.model.current_mode = Some(mode_update.current_mode_id.to_string());
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
                    self.model.current_model = Some(label);
                }
                Some(SessionConfigOptionCategory::Mode) => {
                    self.model.current_mode = Some(label);
                }
                _ => {
                    let name = option.name.to_lowercase();
                    if name.contains("model") {
                        self.model.current_model = Some(label.clone());
                    }
                    if name.contains("mode") {
                        self.model.current_mode = Some(label);
                    }
                }
            }
        }
    }
}

impl Component for SessionComponent {
    fn register_action_handler(&mut self, tx: mpsc::UnboundedSender<Action>) {
        self.action_tx = Some(tx);
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> miette::Result<()> {
        if self.is_submit_key(key) {
            self.send_action(Action::SessionSubmitPrompt(self.id));
            return Ok(());
        }

        if self.is_newline_key(key) {
            self.send_action(Action::SessionInsertNewline(self.id));
            return Ok(());
        }

        if self.handle_navigation_key(key) {
            return Ok(());
        }

        if !self.model.waiting {
            self.send_action(Action::SessionInput {
                tab_id: self.id,
                key,
            });
        }

        Ok(())
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent, area: Rect) -> miette::Result<()> {
        let delta = match mouse.kind {
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => return Ok(()),
        };

        let layout = session_layout(
            area,
            self.view.show_stderr,
            self.model.pending_fork_message.as_deref(),
        );
        if !rect_contains(layout.conversation, mouse.column, mouse.row) {
            return Ok(());
        }

        if let Some(stderr_area) = layout.stderr
            && rect_contains(stderr_area, mouse.column, mouse.row)
        {
            self.send_action(Action::SessionFocusStderr(self.id));
            self.send_action(Action::SessionScrollStderr {
                tab_id: self.id,
                delta,
            });
            return Ok(());
        }

        self.send_action(Action::SessionFocusConversation(self.id));
        self.send_action(Action::SessionScrollConversation {
            tab_id: self.id,
            delta,
        });
        Ok(())
    }

    fn update(&mut self, action: &Action) -> miette::Result<()> {
        if action_target_tab_id(action).is_some_and(|tab_id| tab_id != self.id) {
            return Ok(());
        }

        match action {
            Action::SessionInput { key, .. } => {
                self.view.textarea.input(*key);
            }
            Action::SessionInsertNewline(_) => {
                self.view.textarea.insert_newline();
            }
            Action::SessionToggleStderr(_) => {
                self.view.show_stderr = !self.view.show_stderr;
                if !self.view.show_stderr {
                    self.view.focused_panel = FocusedPanel::Conversation;
                }
            }
            Action::SessionCycleFocus(_) => {
                self.view.focused_panel = match self.view.focused_panel {
                    FocusedPanel::Conversation => FocusedPanel::Stderr,
                    FocusedPanel::Stderr => FocusedPanel::Conversation,
                };
            }
            Action::SessionFocusConversation(_) => {
                self.view.focused_panel = FocusedPanel::Conversation;
            }
            Action::SessionFocusStderr(_) => {
                self.view.focused_panel = FocusedPanel::Stderr;
            }
            Action::SessionScrollConversation { delta, .. } => {
                self.view.conversation_list.scroll_by(*delta);
            }
            Action::SessionScrollStderr { delta, .. } => {
                self.view.stderr_scroll = apply_scroll_delta(self.view.stderr_scroll, *delta);
            }
            _ => {}
        }
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let layout = session_layout(
            area,
            self.view.show_stderr,
            self.model.pending_fork_message.as_deref(),
        );
        self.render_output_panels(frame, &layout);
        self.render_input_panel(frame, &layout);
    }
}

#[derive(Clone, Copy)]
struct SessionLayout {
    conversation: Rect,
    conversation_panel: Option<Rect>,
    stderr: Option<Rect>,
    input: Rect,
    fork_hint: Option<Rect>,
}

fn session_layout(area: Rect, show_stderr: bool, fork_message: Option<&str>) -> SessionLayout {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(session_input_height(fork_message, area.width)),
        ])
        .split(area);

    let conversation = chunks[0];
    let input_container = chunks[1];

    let (conversation_panel, stderr) = if show_stderr {
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(conversation);
        (Some(horiz[0]), Some(horiz[1]))
    } else {
        (None, None)
    };

    let (fork_hint, input) = if let Some(fork_message) = fork_message {
        let hint_block_height = fork_hint_block_height(fork_message, input_container.width);
        let input_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(hint_block_height),
                Constraint::Length(AGENT_INPUT_HEIGHT),
            ])
            .spacing(Spacing::Overlap(1))
            .split(input_container);
        (Some(input_chunks[0]), input_chunks[1])
    } else {
        (None, input_container)
    };

    SessionLayout {
        conversation,
        conversation_panel,
        stderr,
        input,
        fork_hint,
    }
}

#[must_use]
pub fn session_input_height(fork_message: Option<&str>, area_width: u16) -> u16 {
    AGENT_INPUT_HEIGHT
        + fork_message.map_or(0, |text| {
            fork_hint_block_height(text, area_width).saturating_sub(1)
        })
}

fn fork_hint_block_height(fork_message: &str, area_width: u16) -> u16 {
    let inner_width = area_width.saturating_sub(2).max(1) as usize;
    let wrapped_lines: usize = fork_message
        .lines()
        .map(|line| line.chars().count().div_ceil(inner_width).max(1))
        .sum();
    let content_height = saturated_u16(wrapped_lines.min(usize::from(u16::MAX)));
    content_height
        .saturating_add(2)
        .clamp(FORK_HINT_MIN_BLOCK_HEIGHT, FORK_HINT_MAX_BLOCK_HEIGHT)
}

fn render_conversation_list(
    frame: &mut Frame,
    messages: &[Message],
    list_state: &mut tui_widget_list::ListState,
    area: Rect,
) {
    let builder = ListBuilder::new(move |context| {
        let message = &messages[context.index];
        let widget = match message {
            Message::ToolCall(tool_call) => ConversationWidget::ToolCall(
                ToolCallWidget::from_tool_call(tool_call, context.cross_axis_size),
            ),
            other => ConversationWidget::Message(MessageWidget::from_message(
                other,
                context.cross_axis_size,
            )),
        };
        let height = widget.height();
        (widget, height)
    });

    let list_view = ListView::new(builder, messages.len());
    frame.render_stateful_widget(list_view, area, list_state);
}

fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    row >= rect.y
        && row < rect.y.saturating_add(rect.height)
        && column >= rect.x
        && column < rect.x.saturating_add(rect.width)
}

fn apply_scroll_delta(current: u16, delta: i16) -> u16 {
    if delta >= 0 {
        current.saturating_add(delta.cast_unsigned())
    } else {
        current.saturating_sub(delta.unsigned_abs())
    }
}

fn action_target_tab_id(action: &Action) -> Option<u32> {
    match action {
        Action::SessionInput { tab_id, .. }
        | Action::SessionScrollConversation { tab_id, .. }
        | Action::SessionScrollStderr { tab_id, .. }
        | Action::SessionInsertNewline(tab_id)
        | Action::SessionToggleStderr(tab_id)
        | Action::SessionCycleFocus(tab_id)
        | Action::SessionFocusConversation(tab_id)
        | Action::SessionFocusStderr(tab_id) => Some(*tab_id),
        _ => None,
    }
}

fn saturated_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn wrapped_line_height(char_count: usize, width: usize) -> u16 {
    saturated_u16(char_count.div_ceil(width).max(1))
}

fn new_textarea() -> TextArea<'static> {
    let mut textarea = TextArea::default();
    textarea.set_placeholder_text("Type a prompt and press Enter...");
    textarea.set_cursor_line_style(Style::default());
    textarea
}

fn current_select_label(option: &SessionConfigOption) -> Option<String> {
    let SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };

    match &select.options {
        SessionConfigSelectOptions::Ungrouped(values) => values
            .iter()
            .find(|value| value.value == select.current_value)
            .map(|value| value.name.clone())
            .or_else(|| Some(select.current_value.to_string())),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter())
            .find(|value| value.value == select.current_value)
            .map(|value| value.name.clone())
            .or_else(|| Some(select.current_value.to_string())),
        _ => Some(select.current_value.to_string()),
    }
}

struct MessageWidget<'a> {
    paragraph: Paragraph<'a>,
    height: u16,
}

impl MessageWidget<'_> {
    fn from_message(message: &Message, width: u16) -> MessageWidget<'_> {
        let width = width.max(1) as usize;
        match message {
            Message::User(text) => {
                let line = Line::from(vec![
                    Span::styled(
                        "> ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text.as_str()),
                ]);
                let height = wrapped_line_height(text.chars().count() + 2, width);
                MessageWidget {
                    paragraph: Paragraph::new(line).wrap(Wrap { trim: false }),
                    height,
                }
            }
            Message::Agent(text) => {
                let lines: Vec<Line> = text
                    .lines()
                    .map(|line| Line::from(Span::styled(line, Style::default().fg(Color::Cyan))))
                    .collect();
                let height = text
                    .lines()
                    .fold(0_u16, |total, line| {
                        total.saturating_add(wrapped_line_height(line.chars().count(), width))
                    })
                    .max(1);
                MessageWidget {
                    paragraph: Paragraph::new(lines).wrap(Wrap { trim: false }),
                    height,
                }
            }
            Message::System(text) => {
                let line = Line::from(vec![
                    Span::styled("[", Style::default().fg(Color::Yellow)),
                    Span::styled(text.as_str(), Style::default().fg(Color::Yellow)),
                    Span::styled("]", Style::default().fg(Color::Yellow)),
                ]);
                let height = wrapped_line_height(text.chars().count() + 2, width);
                MessageWidget {
                    paragraph: Paragraph::new(line).wrap(Wrap { trim: false }),
                    height,
                }
            }
            Message::ToolCall(tool_call) => {
                let text = &tool_call.title;
                let line = Line::from(Span::styled(
                    format!("[Tool: {text}]"),
                    Style::default().fg(Color::Yellow),
                ));
                let height = wrapped_line_height(text.chars().count() + 8, width);
                MessageWidget {
                    paragraph: Paragraph::new(line).wrap(Wrap { trim: false }),
                    height,
                }
            }
        }
    }
}

impl Widget for MessageWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.paragraph.render(area, buf);
    }
}

struct ToolCallWidget<'a> {
    block: Block<'a>,
    lines: Vec<Line<'a>>,
    height: u16,
}

impl ToolCallWidget<'_> {
    fn from_tool_call(tool_call: &ToolCall, _width: u16) -> ToolCallWidget<'_> {
        let kind_label = match tool_call.kind {
            agent_client_protocol::ToolKind::Read => "Read",
            agent_client_protocol::ToolKind::Edit => "Edit",
            agent_client_protocol::ToolKind::Delete => "Delete",
            agent_client_protocol::ToolKind::Move => "Move",
            agent_client_protocol::ToolKind::Search => "Search",
            agent_client_protocol::ToolKind::Execute => "Execute",
            agent_client_protocol::ToolKind::Think => "Think",
            agent_client_protocol::ToolKind::Fetch => "Fetch",
            agent_client_protocol::ToolKind::SwitchMode => "SwitchMode",
            _ => "Tool",
        };

        let mut lines = Vec::new();
        if !tool_call.title.is_empty() {
            lines.push(Line::from(Span::styled(
                tool_call.title.clone(),
                Style::default().fg(Color::White),
            )));
        }
        for location in &tool_call.locations {
            let path_str = location.path.display().to_string();
            let text = if let Some(line) = location.line {
                format!("path: {path_str}:{line}")
            } else {
                format!("path: {path_str}")
            };
            lines.push(Line::from(Span::styled(
                text,
                Style::default().fg(Color::DarkGray),
            )));
        }

        ToolCallWidget {
            block: Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    format!(" {kind_label} "),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ))
                .border_style(Style::default().fg(Color::DarkGray)),
            height: saturated_u16(lines.len()).saturating_add(2),
            lines,
        }
    }
}

impl Widget for ToolCallWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.lines)
            .block(self.block)
            .render(area, buf);
    }
}

enum ConversationWidget<'a> {
    Message(MessageWidget<'a>),
    ToolCall(ToolCallWidget<'a>),
}

impl ConversationWidget<'_> {
    fn height(&self) -> u16 {
        match self {
            ConversationWidget::Message(widget) => widget.height,
            ConversationWidget::ToolCall(widget) => widget.height,
        }
    }
}

impl Widget for ConversationWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        match self {
            ConversationWidget::Message(widget) => widget.render(area, buf),
            ConversationWidget::ToolCall(widget) => widget.render(area, buf),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_splits_stderr_and_input() {
        let layout = session_layout(
            Rect::new(0, 0, 80, 24),
            true,
            Some("forked from another session"),
        );

        assert!(layout.stderr.is_some());
        assert!(layout.fork_hint.is_some());
        assert!(layout.input.height > 0);
    }
}
