use std::path::PathBuf;

use concats_core::{
    Oid,
    diff::{DiffLineKind, DiffStatus, FileDiff},
    session::{self, Session},
    turn::{self, Turn, TurnEntry, TurnEntryKind},
};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use time::format_description::well_known::Rfc3339;
use tokio::sync::mpsc;
use tui_widget_list::{ListBuilder, ListView};

use crate::{action::Action, components::Component, tabs::ActiveTab};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionsPanelFocus {
    List,
    Detail,
}

pub struct DetailPanel {
    pub session_index: usize,
    pub rows: Vec<TurnRow>,
    pub list_state: tui_widget_list::ListState,
}

pub struct TurnRow {
    pub lines: Vec<Line<'static>>,
    pub height: u16,
}

pub struct SessionListItem {
    pub session: Session,
    pub tip: Oid,
    pub modified_at: time::OffsetDateTime,
}

pub struct SessionsBrowserComponent {
    action_tx: Option<mpsc::UnboundedSender<Action>>,
    repo_path: PathBuf,
    pub sessions: Vec<SessionListItem>,
    pub detail: Option<DetailPanel>,
    pub list_state: tui_widget_list::ListState,
    pub focus: SessionsPanelFocus,
}

impl SessionsBrowserComponent {
    #[must_use]
    pub fn new(repo_path: PathBuf) -> Self {
        Self {
            action_tx: None,
            repo_path,
            sessions: Vec::new(),
            detail: None,
            list_state: tui_widget_list::ListState::default(),
            focus: SessionsPanelFocus::List,
        }
    }

    #[must_use]
    pub fn selected_session_index(&self) -> usize {
        self.list_state.selected.unwrap_or(0)
    }

    #[must_use]
    pub fn selected_fork_info(&self) -> Option<(String, Oid)> {
        let item = self.sessions.get(self.selected_session_index())?;
        Some((item.session.id.to_string(), item.tip))
    }

    #[must_use]
    pub fn has_detail(&self) -> bool {
        self.detail.is_some()
    }

    fn send_action(&self, action: Action) {
        if let Some(action_tx) = &self.action_tx {
            let _ = action_tx.send(action);
        }
    }

    fn refresh(&mut self) {
        let repo = match git2::Repository::open(&self.repo_path) {
            Ok(r) => std::rc::Rc::new(r),
            Err(error) => {
                tracing::warn!("failed to open repository: {error}");
                return;
            }
        };
        match session::list(&repo) {
            Ok(sessions) => {
                self.sessions = sessions
                    .into_iter()
                    .filter_map(|session| match load_session_item(session) {
                        Ok(item) => Some(item),
                        Err(error) => {
                            tracing::warn!("failed to load session metadata: {error}");
                            None
                        }
                    })
                    .collect();
                let selected = self
                    .selected_session_index()
                    .min(self.sessions.len().saturating_sub(1));
                self.list_state.select(Some(selected));
                self.detail = None;
                self.focus = SessionsPanelFocus::List;
            }
            Err(error) => {
                tracing::warn!("failed to list sessions: {error}");
            }
        }
    }

    fn select_next(&mut self) {
        match self.focus {
            SessionsPanelFocus::List => {
                if !self.sessions.is_empty() {
                    let current = self.selected_session_index();
                    self.list_state
                        .select(Some((current + 1).min(self.sessions.len() - 1)));
                }
            }
            SessionsPanelFocus::Detail => {
                self.scroll_detail(1);
            }
        }
    }

    fn select_prev(&mut self) {
        match self.focus {
            SessionsPanelFocus::List => {
                let current = self.selected_session_index();
                self.list_state.select(Some(current.saturating_sub(1)));
            }
            SessionsPanelFocus::Detail => {
                self.scroll_detail(-1);
            }
        }
    }

    fn scroll_detail(&mut self, delta: i16) {
        if let Some(detail) = &mut self.detail {
            detail.list_state.scroll_by(delta);
        }
    }

    fn open_detail(&mut self) {
        let index = self.selected_session_index();
        let Some(item) = self.sessions.get(index) else {
            return;
        };

        match turn::list(&item.session) {
            Ok(turns) => {
                self.detail = Some(DetailPanel {
                    session_index: index,
                    rows: turns
                        .iter()
                        .map(|turn| build_turn_row(&item.session, turn))
                        .collect(),
                    list_state: tui_widget_list::ListState::default(),
                });
                self.focus = SessionsPanelFocus::Detail;
            }
            Err(error) => {
                tracing::warn!(
                    "failed to load turns for session {}: {error}",
                    item.session.id
                );
            }
        }
    }

    fn close_detail(&mut self) {
        self.detail = None;
        self.focus = SessionsPanelFocus::List;
    }
}

impl Component for SessionsBrowserComponent {
    fn register_action_handler(&mut self, tx: mpsc::UnboundedSender<Action>) {
        self.action_tx = Some(tx);
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> miette::Result<()> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.send_action(Action::SessionsSelectPrev),
            KeyCode::Down | KeyCode::Char('j') => self.send_action(Action::SessionsSelectNext),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                self.send_action(Action::SessionsOpenDetail);
            }
            KeyCode::Left | KeyCode::Char('h') => self.send_action(Action::SessionsCloseDetail),
            KeyCode::Char('r') => self.send_action(Action::SessionsRefresh),
            KeyCode::Esc => self.send_action(Action::SessionsBack),
            _ => {}
        }
        Ok(())
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent, _area: Rect) -> miette::Result<()> {
        let delta = match mouse.kind {
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => return Ok(()),
        };
        self.send_action(Action::SessionsScrollDetail(delta));
        Ok(())
    }

    fn update(&mut self, action: &Action) -> miette::Result<()> {
        match action {
            Action::SwitchTab(ActiveTab::Sessions) | Action::SessionsRefresh => self.refresh(),
            Action::SessionsSelectNext => self.select_next(),
            Action::SessionsSelectPrev => self.select_prev(),
            Action::SessionsOpenDetail => {
                if self.focus == SessionsPanelFocus::List {
                    self.open_detail();
                }
            }
            Action::SessionsCloseDetail => {
                if self.focus == SessionsPanelFocus::Detail {
                    self.close_detail();
                }
            }
            Action::SessionsScrollDetail(delta) => self.scroll_detail(*delta),
            _ => {}
        }
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let (body_area, hint_area) = if area.height > 2 {
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);
            (vertical[0], vertical[1])
        } else {
            (area, Rect::default())
        };

        if self.detail.is_some() {
            let horizontal = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(body_area);
            render_session_list(frame, self, horizontal[0]);
            render_detail_panel(frame, self, horizontal[1]);
        } else {
            render_session_list(frame, self, body_area);
        }

        if hint_area.width > 0 && hint_area.height > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " [←/→: switch panel | r: refresh]",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ))),
                hint_area,
            );
        }
    }
}

fn render_session_list(frame: &mut Frame, state: &mut SessionsBrowserComponent, area: Rect) {
    let selected = state.list_state.selected;
    let focused = state.focus == SessionsPanelFocus::List;
    let session_count = state.sessions.len();

    if session_count == 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  No sessions found. Press 'r' to refresh.",
                Style::default().fg(Color::DarkGray),
            ))
            .block(sessions_block("Sessions (0)", focused)),
            area,
        );
        return;
    }

    let title = format!("Sessions ({session_count})");
    let block = sessions_block(&title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sessions = &state.sessions;
    let builder = ListBuilder::new(move |context| {
        let session = &sessions[context.index];
        let is_selected = selected == Some(context.index) && focused;
        let widget = SessionItemWidget::new(session, is_selected);
        let height = widget.height;
        (widget, height)
    });

    let list_view = ListView::new(builder, session_count).infinite_scrolling(false);
    frame.render_stateful_widget(list_view, inner, &mut state.list_state);
}

fn render_detail_panel(frame: &mut Frame, state: &mut SessionsBrowserComponent, area: Rect) {
    let focused = state.focus == SessionsPanelFocus::Detail;
    let Some(detail) = state.detail.as_mut() else {
        return;
    };

    let title = state
        .sessions
        .get(detail.session_index)
        .and_then(|item| item.session.name.as_deref())
        .map_or_else(|| "Turns".into(), |name| format!("Turns - {name}"));
    let block = sessions_block(&title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if detail.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  No turns found.",
                Style::default().fg(Color::DarkGray),
            )),
            inner,
        );
        return;
    }

    let rows = &detail.rows;
    let builder = ListBuilder::new(move |context| {
        let row = &rows[context.index];
        (
            TurnWidget {
                lines: row.lines.clone(),
            },
            row.height,
        )
    });

    let list_view = ListView::new(builder, rows.len()).infinite_scrolling(false);
    frame.render_stateful_widget(list_view, inner, &mut detail.list_state);
}

fn load_session_item(session: Session) -> concats_core::error::Result<SessionListItem> {
    Ok(SessionListItem {
        tip: session::tip(&session)?,
        modified_at: session::modified_at(&session)?,
        session,
    })
}

fn build_turn_row(session: &Session, turn: &Turn) -> TurnRow {
    let mut lines = Vec::new();
    let short_oid = turn.oid.short();
    lines.push(Line::from(vec![
        Span::styled(
            "  turn ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({short_oid})"),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    if turn.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no transcript captured)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for entry in turn.entries() {
            render_transcript_entry(entry, &mut lines);
        }
    }

    match concats_core::diff::for_turn(session, turn) {
        Ok(diffs) => render_file_diffs(&diffs, &mut lines),
        Err(error) => lines.push(Line::from(Span::styled(
            format!("  failed to load diff: {error}"),
            Style::default().fg(Color::Red),
        ))),
    }

    lines.push(Line::from(""));

    TurnRow {
        height: u16::try_from(lines.len()).unwrap_or(u16::MAX),
        lines,
    }
}

fn format_timestamp(value: time::OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| value.unix_timestamp().to_string())
}

fn render_transcript_entry(entry: &TurnEntry, lines: &mut Vec<Line<'static>>) {
    match &entry.kind {
        TurnEntryKind::Prompt { text } => {
            let preview: String = text.chars().take(120).collect();
            let display = if text.len() > 120 {
                format!("{preview}...")
            } else {
                preview
            };
            lines.push(Line::from(vec![
                Span::styled(
                    "  > ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(display, Style::default().fg(Color::Green)),
            ]));
        }
        TurnEntryKind::Response { text } => {
            let preview: String = text.chars().take(200).collect();
            let display = if text.len() > 200 {
                format!("{preview}...")
            } else {
                preview
            };
            for response_line in display.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {response_line}"),
                    Style::default().fg(Color::Cyan),
                )));
            }
        }
        TurnEntryKind::ToolCall { kind } => {
            lines.push(Line::from(vec![
                Span::styled("  tool ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    kind.to_string(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }
}

fn render_file_diffs(diffs: &[FileDiff], lines: &mut Vec<Line<'static>>) {
    if diffs.is_empty() {
        return;
    }
    lines.push(Line::from(""));
    for file in diffs {
        let (icon, color) = match &file.status {
            DiffStatus::Added => ("A", Color::Green),
            DiffStatus::Modified => ("M", Color::Yellow),
            DiffStatus::Deleted => ("D", Color::Red),
            DiffStatus::Renamed { .. } => ("R", Color::Cyan),
        };
        let path = match &file.status {
            DiffStatus::Renamed { old_path } => format!("{old_path} -> {}", file.path),
            _ => file.path.clone(),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {icon} "), Style::default().fg(color)),
            Span::styled(
                path,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        for hunk in &file.hunks {
            lines.push(Line::from(Span::styled(
                format!("    {}", hunk.header),
                Style::default().fg(Color::DarkGray),
            )));
            for line in &hunk.lines {
                let (prefix, color) = match line.kind {
                    DiffLineKind::Add => ("+", Color::Green),
                    DiffLineKind::Remove => ("-", Color::Red),
                    DiffLineKind::Context => (" ", Color::Gray),
                };
                lines.push(Line::from(Span::styled(
                    format!("    {prefix}{}", line.content),
                    Style::default().fg(color),
                )));
            }
        }
    }
}

fn sessions_block(title: &str, focused: bool) -> Block<'_> {
    let border_color = if focused {
        Color::Blue
    } else {
        Color::DarkGray
    };
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(border_color))
}

struct SessionItemWidget<'a> {
    paragraph: Paragraph<'a>,
    height: u16,
}

impl SessionItemWidget<'_> {
    fn new(item: &SessionListItem, is_selected: bool) -> SessionItemWidget<'_> {
        let style = if is_selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::White)
        };

        SessionItemWidget {
            paragraph: Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(
                        "  {} ",
                        item.session.name.as_deref().unwrap_or("(empty session)")
                    ),
                    style,
                ),
                Span::styled("  •  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format_timestamp(item.modified_at),
                    Style::default().fg(Color::DarkGray),
                ),
            ])),
            height: 1,
        }
    }
}

impl Widget for SessionItemWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.paragraph.render(area, buf);
    }
}

struct TurnWidget {
    lines: Vec<Line<'static>>,
}

impl Widget for TurnWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_keeps_selection_in_bounds() {
        let mut component = SessionsBrowserComponent::new(PathBuf::from("."));
        component.list_state.select(Some(5));
        component.sessions = vec![];
        component.detail = Some(DetailPanel {
            session_index: 0,
            rows: vec![],
            list_state: tui_widget_list::ListState::default(),
        });
        component.focus = SessionsPanelFocus::Detail;

        component.sessions = vec![];
        component.list_state.select(Some(5));
        component.detail = None;
        component.focus = SessionsPanelFocus::List;

        assert_eq!(component.selected_session_index(), 5);
    }
}
