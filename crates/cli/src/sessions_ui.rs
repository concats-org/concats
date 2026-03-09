use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use tui_widget_list::{ListBuilder, ListView};

use concats_core::session_history::{
    DiffLineKind, DiffStatus, FileDiff, SessionInfo, TurnInfo,
};

use crate::{
    app::App,
    tabs::SessionsPanelFocus,
};

// ── Session list item widget (left panel) ──────────────────────────

struct SessionItemWidget<'a> {
    paragraph: Paragraph<'a>,
    height: u16,
}

impl SessionItemWidget<'_> {
    fn new(session: &SessionInfo, is_selected: bool, _width: u16) -> SessionItemWidget<'_> {
        let style = if is_selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::White)
        };

        let line = Line::from(vec![
            Span::styled(format!("  {} ", &session.title), style),
            Span::styled("  •  ", Style::default().fg(Color::DarkGray)),
            Span::styled(&session.timestamp, Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("  •  {} turns", session.turn_count),
                Style::default().fg(Color::Cyan),
            ),
        ]);

        SessionItemWidget {
            paragraph: Paragraph::new(line),
            height: 1,
        }
    }
}

impl Widget for SessionItemWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.paragraph.render(area, buf);
    }
}

// ── Checkpoint widget (right panel) ────────────────────────────────

struct CheckpointWidget<'a> {
    lines: Vec<Line<'a>>,
    height: u16,
}

impl CheckpointWidget<'_> {
    fn new(turn: &TurnInfo, _width: u16) -> CheckpointWidget<'_> {
        let mut lines: Vec<Line<'_>> = Vec::new();

        // Turn header.
        let header_style = Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        let short_oid = turn.commit_oid.short();
        lines.push(Line::from(vec![
            Span::styled(format!("  #{} ", turn.turn_number), header_style),
            Span::styled(
                format!("({short_oid})"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));

        // Prompt.
        let prompt_preview: String = turn.prompt.chars().take(120).collect();
        let prompt_display = if turn.prompt.len() > 120 {
            format!("{prompt_preview}...")
        } else {
            prompt_preview
        };
        lines.push(Line::from(vec![
            Span::styled(
                "  > ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(prompt_display, Style::default().fg(Color::Green)),
        ]));

        // Response summary (truncated).
        if !turn.response_summary.is_empty() {
            let resp_preview: String = turn.response_summary.chars().take(200).collect();
            let resp_display = if turn.response_summary.len() > 200 {
                format!("{resp_preview}...")
            } else {
                resp_preview
            };
            // Wrap long responses across multiple lines.
            for resp_line in resp_display.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {resp_line}"),
                    Style::default().fg(Color::Cyan),
                )));
            }
        }

        // File diffs.
        render_file_diffs(&turn.diffs, &mut lines);

        // Blank separator.
        lines.push(Line::from(""));

        let height = lines.len() as u16;

        CheckpointWidget { lines, height }
    }
}

fn render_file_diffs<'a>(diffs: &[FileDiff], lines: &mut Vec<Line<'a>>) {
    if diffs.is_empty() {
        return;
    }
    lines.push(Line::from(""));
    for file in diffs {
        let (icon, icon_color) = match &file.status {
            DiffStatus::Added => ("A", Color::Green),
            DiffStatus::Modified => ("M", Color::Yellow),
            DiffStatus::Deleted => ("D", Color::Red),
            DiffStatus::Renamed { .. } => ("R", Color::Cyan),
        };
        let path_suffix = match &file.status {
            DiffStatus::Renamed { old_path } => format!("{} -> {}", old_path, file.path),
            _ => file.path.clone(),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {icon} "), Style::default().fg(icon_color)),
            Span::styled(
                path_suffix,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        for hunk in &file.hunks {
            lines.push(Line::from(Span::styled(
                format!("    {}", &hunk.header),
                Style::default().fg(Color::DarkGray),
            )));
            for dl in &hunk.lines {
                let (prefix, color) = match dl.kind {
                    DiffLineKind::Add => ("+", Color::Green),
                    DiffLineKind::Remove => ("-", Color::Red),
                    DiffLineKind::Context => (" ", Color::Gray),
                };
                lines.push(Line::from(Span::styled(
                    format!("    {prefix}{}", &dl.content),
                    Style::default().fg(color),
                )));
            }
        }
    }
}

impl Widget for CheckpointWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let paragraph = Paragraph::new(self.lines).wrap(Wrap { trim: false });
        paragraph.render(area, buf);
    }
}

// ── Main render function ───────────────────────────────────────────

pub fn render_sessions_tab(frame: &mut Frame, app: &mut App, area: Rect) {
    let state = &mut app.sessions_state;

    // Reserve one row at the bottom for the hint line.
    let body_area;
    let hint_area;
    if area.height > 2 {
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);
        body_area = vert[0];
        hint_area = vert[1];
    } else {
        body_area = area;
        hint_area = Rect::default();
    }

    if state.detail.is_some() {
        // Two-panel layout.
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(body_area);

        render_session_list(frame, state, horiz[0]);
        render_detail_panel(frame, state, horiz[1]);
    } else {
        // Single panel — full width session list.
        render_session_list(frame, state, body_area);
    }

    // Hint line.
    if hint_area.width > 0 && hint_area.height > 0 {
        let hint = Paragraph::new(Line::from(Span::styled(
            " [f: fork | ←/→: switch panel | r: refresh]",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )));
        frame.render_widget(hint, hint_area);
    }
}

fn render_session_list(
    frame: &mut Frame,
    state: &mut crate::tabs::SessionsTabState,
    area: Rect,
) {
    let selected = state.list_state.selected;
    let focused = state.focus == SessionsPanelFocus::List;
    let sessions = &state.sessions;
    let session_count = sessions.len();

    if session_count == 0 {
        let empty = Paragraph::new(Span::styled(
            "  No sessions found. Press 'r' to refresh.",
            Style::default().fg(Color::DarkGray),
        ))
        .block(sessions_block("Sessions (0)", focused));
        frame.render_widget(empty, area);
        return;
    }

    let title = format!("Sessions ({session_count})");
    let block = sessions_block(&title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let builder = ListBuilder::new(move |context| {
        let session = &sessions[context.index];
        let is_selected = selected == Some(context.index) && focused;
        let widget = SessionItemWidget::new(session, is_selected, context.cross_axis_size);
        let height = widget.height;
        (widget, height)
    });

    let list_view = ListView::new(builder, session_count).infinite_scrolling(false);
    frame.render_stateful_widget(list_view, inner, &mut state.list_state);
}

fn render_detail_panel(
    frame: &mut Frame,
    state: &mut crate::tabs::SessionsTabState,
    area: Rect,
) {
    let focused = state.focus == SessionsPanelFocus::Detail;
    let detail = match state.detail.as_mut() {
        Some(d) => d,
        None => return,
    };

    let session_title = state
        .sessions
        .get(detail.session_index)
        .map(|s| s.title.as_str())
        .unwrap_or("Session");
    let title = format!("Checkpoints — {session_title}");
    let block = sessions_block(&title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let turns = &detail.turns;
    let turn_count = turns.len();
    if turn_count == 0 {
        let empty = Paragraph::new(Span::styled(
            "  No checkpoints found.",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(empty, inner);
        return;
    }

    let builder = ListBuilder::new(move |context| {
        let turn = &turns[context.index];
        let widget = CheckpointWidget::new(turn, context.cross_axis_size);
        let height = widget.height;
        (widget, height)
    });

    let list_view = ListView::new(builder, turn_count).infinite_scrolling(false);
    frame.render_stateful_widget(list_view, inner, &mut detail.list_state);
}

fn sessions_block(title: &str, focused: bool) -> Block<'_> {
    let border_color = if focused { Color::Blue } else { Color::DarkGray };
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(border_color))
}
