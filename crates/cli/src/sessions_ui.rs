use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::App;

pub fn render_sessions_tab(frame: &mut Frame, app: &mut App, area: Rect) {
    let state = &app.sessions_state;

    let mut lines: Vec<Line> = Vec::new();

    if state.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No sessions found. Press 'r' to refresh.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    for (i, session) in state.sessions.iter().enumerate() {
        let is_selected =
            !state.expanded.as_ref().is_some_and(|_| true) && i == state.selected_session;
        let is_expanded = state
            .expanded
            .as_ref()
            .is_some_and(|e| e.session_index == i);

        let arrow = if is_expanded { "▼" } else { "▶" };

        let style = if is_selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::REVERSED)
        } else if is_expanded {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {arrow} "), style),
            Span::styled(&session.title, style),
            Span::styled("  •  ", Style::default().fg(Color::DarkGray)),
            Span::styled(&session.timestamp, Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("  •  {} turns", session.turn_count),
                Style::default().fg(Color::Cyan),
            ),
        ]));

        // Render expanded turns.
        if let Some(ref expanded) = state.expanded
            && expanded.session_index == i
        {
            for (t_idx, turn) in expanded.turns.iter().enumerate() {
                let is_turn_selected = t_idx == expanded.selected_turn;
                let prompt_preview: String = turn.prompt.chars().take(60).collect();
                let prompt_display = if turn.prompt.len() > 60 {
                    format!("{prompt_preview}...")
                } else {
                    prompt_preview
                };

                let turn_style = if is_turn_selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::REVERSED)
                } else {
                    Style::default().fg(Color::Gray)
                };

                let stop_style = match turn.stop_reason.as_str() {
                    "EndTurn" => Style::default().fg(Color::Green),
                    _ => Style::default().fg(Color::Yellow),
                };

                let turn_prefix = format!("      #{:<3} ", turn.turn_number);
                let stop_reason = turn.stop_reason.clone();

                lines.push(Line::from(vec![
                    Span::styled(turn_prefix, turn_style),
                    Span::styled(prompt_display, turn_style),
                    Span::raw("  "),
                    Span::styled(stop_reason, stop_style),
                ]));
            }

            // Hint line.
            lines.push(Line::from(Span::styled(
                "      [f: fork from selected turn | Esc: collapse]",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
        }
    }

    let title = format!("Sessions ({})", state.sessions.len());
    let content = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(content, area);
}
