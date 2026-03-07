use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect, Spacing},
    style::{Color, Modifier, Style},
    symbols::{merge::MergeStrategy, scrollbar::Set},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
};
use tui_widget_list::{ListBuildContext, ListBuilder, ListView};

use crate::{
    app::{App, FocusedPanel, Message, SessionTab},
    sessions_ui,
    tabs::{ActiveTab, ClickTarget, TabBarEntry},
};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const TAB_BAR_HEIGHT: u16 = 1;
pub const AGENT_INPUT_HEIGHT: u16 = 4;
pub const FORK_HINT_MIN_BLOCK_HEIGHT: u16 = 3;
pub const FORK_HINT_MAX_BLOCK_HEIGHT: u16 = 9;

pub fn session_input_height(tab: &SessionTab, area_width: u16) -> u16 {
    AGENT_INPUT_HEIGHT
        + if let Some(fork_message) = tab.pending_fork_message.as_deref() {
            fork_hint_block_height(fork_message, area_width).saturating_sub(1)
        } else {
            0
        }
}

fn fork_hint_block_height(fork_message: &str, area_width: u16) -> u16 {
    let inner_width = area_width.saturating_sub(2).max(1) as usize;
    let wrapped_lines: usize = fork_message
        .lines()
        .map(|line| {
            let len = line.chars().count();
            len.div_ceil(inner_width).max(1)
        })
        .sum();
    let content_height = wrapped_lines.min(u16::MAX as usize) as u16;
    content_height
        .saturating_add(2) // top and bottom borders
        .clamp(FORK_HINT_MIN_BLOCK_HEIGHT, FORK_HINT_MAX_BLOCK_HEIGHT)
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),                 // main content
            Constraint::Length(TAB_BAR_HEIGHT), // tab bar
        ])
        .split(frame.area());

    // Render tab-specific content.
    match app.active_tab {
        ActiveTab::Session(id) => {
            // Find the session tab and render it.
            if let Some(idx) = app.session_tabs.iter().position(|t| t.id == id) {
                let tab = &mut app.session_tabs[idx];
                render_session_tab(frame, tab, app.tick, chunks[0]);
            } else {
                render_placeholder(frame, chunks[0], "Session", "Session not found.");
            }
        }
        ActiveTab::Sessions => sessions_ui::render_sessions_tab(frame, app, chunks[0]),
        ActiveTab::Settings => {
            render_placeholder(frame, chunks[0], "Settings", "Not yet implemented.")
        }
        ActiveTab::Help => render_placeholder(
            frame,
            chunks[0],
            "Help",
            "Ctrl+N: new session | Ctrl+W: close tab | Ctrl+1-9: switch tabs | Up/Down: navigate | Enter: expand | f: fork | r: refresh | Ctrl+C: quit",
        ),
    }

    // Render tab bar.
    render_tab_bar(frame, app, chunks[1]);

    // Render agent picker overlay if active.
    if let Some(ref picker) = app.agent_picker {
        render_agent_picker(frame, picker);
    }
}

fn render_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = Vec::new();

    // Status indicator from the active session (if any).
    spans.extend(status_spans(app));
    spans.push(Span::raw("  "));

    let entries = app.tab_bar_entries();
    for entry in &entries {
        match entry {
            TabBarEntry::Session { id, label } => {
                let is_active = app.active_tab == ActiveTab::Session(*id);
                if is_active {
                    spans.push(Span::styled(
                        format!(" {label} "),
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::styled(
                        "\u{2715} ",
                        Style::default().fg(Color::DarkGray).bg(Color::White),
                    ));
                } else {
                    spans.push(Span::styled(
                        format!(" {label} "),
                        Style::default().fg(Color::DarkGray),
                    ));
                    spans.push(Span::styled(
                        "\u{2715} ",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ));
                }
            }
            TabBarEntry::NewButton => {
                spans.push(Span::styled(" [+] ", Style::default().fg(Color::Green)));
            }
            TabBarEntry::Utility { tab, label } => {
                let is_active = app.active_tab == *tab;
                if is_active {
                    spans.push(Span::styled(
                        format!(" {label} "),
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::styled(
                        format!(" {label} "),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
        }
    }

    let tab_bar = Paragraph::new(Line::from(spans));
    frame.render_widget(tab_bar, area);
}

fn status_spans(app: &App) -> Vec<Span<'static>> {
    // Get status from the active session tab, or show a default.
    let (waiting, status, tick) = if let Some(tab) = app.active_session() {
        (tab.waiting, tab.status.clone(), app.tick)
    } else {
        (false, "no session".to_string(), 0)
    };

    if waiting {
        let spinner = SPINNER_FRAMES[tick % SPINNER_FRAMES.len()];
        vec![
            Span::styled(spinner.to_string(), Style::default().fg(Color::Yellow)),
            Span::styled(
                " working".to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::DIM),
            ),
        ]
    } else {
        vec![
            Span::styled(
                "\u{25cf}".to_string(),
                Style::default().fg(status_color(&status)),
            ),
            Span::styled(
                format!(" {status}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        ]
    }
}

fn status_color(status: &str) -> Color {
    match status {
        s if s.starts_with("done") => Color::Green,
        "error" => Color::Red,
        "disconnected" | "session ended" | "no session" => Color::DarkGray,
        _ => Color::Green,
    }
}

/// Returns click targets with their column ranges for the tab bar.
pub fn tab_click_hitboxes(app: &App) -> Vec<(ClickTarget, usize, usize)> {
    let mut hitboxes = Vec::new();
    let mut pos = status_cell_width(app) + 2;

    let entries = app.tab_bar_entries();
    for entry in &entries {
        match entry {
            TabBarEntry::Session { id, label } => {
                // Label part: clicking switches to the tab.
                let label_text = format!(" {label} ");
                let label_start = pos;
                let label_end = label_start + label_text.chars().count();
                hitboxes.push((
                    ClickTarget::SwitchTab(ActiveTab::Session(*id)),
                    label_start,
                    label_end,
                ));
                pos = label_end;

                // Close button part.
                let close_text = "\u{2715} ";
                let close_start = pos;
                let close_end = close_start + close_text.chars().count();
                hitboxes.push((ClickTarget::CloseSession(*id), close_start, close_end));
                pos = close_end;
            }
            TabBarEntry::NewButton => {
                let text = " [+] ";
                let start = pos;
                let end = start + text.chars().count();
                hitboxes.push((ClickTarget::NewSession, start, end));
                pos = end;
            }
            TabBarEntry::Utility { tab, label } => {
                let text = format!(" {label} ");
                let start = pos;
                let end = start + text.chars().count();
                hitboxes.push((ClickTarget::SwitchTab(*tab), start, end));
                pos = end;
            }
        }
    }

    hitboxes
}

fn status_cell_width(app: &App) -> usize {
    if let Some(tab) = app.active_session() {
        if tab.waiting {
            "\u{280b} working".chars().count()
        } else {
            format!("\u{25cf} {}", tab.status).chars().count()
        }
    } else {
        "\u{25cf} no session".chars().count()
    }
}

fn render_placeholder(frame: &mut Frame, area: Rect, title: &str, content: &str) {
    let p = Paragraph::new(content)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

/// Render the agent picker overlay (centered popup).
fn render_agent_picker(frame: &mut Frame, picker: &crate::app::AgentPickerState) {
    let area = frame.area();
    let popup_width = 40u16.min(area.width.saturating_sub(4));
    let popup_height = (picker.agents.len() as u16 + 2).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    // Clear the area behind the popup.
    frame.render_widget(Clear, popup_area);

    let mut lines: Vec<Line> = Vec::new();
    for (i, (_id, display_name)) in picker.agents.iter().enumerate() {
        let style = if i == picker.selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(format!(" {display_name} "), style)));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Select Agent")
        .border_style(Style::default().fg(Color::Cyan));
    let list = Paragraph::new(lines).block(block);
    frame.render_widget(list, popup_area);
}

/// Render the conversation as a `ListView` of per-message `Paragraph` widgets.
fn render_conversation_list(frame: &mut Frame, tab: &mut SessionTab, area: Rect) {
    let messages = &tab.messages;
    let msg_count = messages.len();

    let builder = ListBuilder::new(move |context: &ListBuildContext| {
        let width = context.cross_axis_size.max(1) as usize;
        let msg = &messages[context.index];

        let (lines, height) = match msg {
            Message::User(text) => {
                let lines = vec![Line::from(vec![
                    Span::styled(
                        "> ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text.as_str()),
                ])];
                let h = (text.chars().count() + 2).div_ceil(width).max(1) as u16;
                (lines, h)
            }
            Message::Agent(text) => {
                let lines: Vec<Line> = text
                    .lines()
                    .map(|line| {
                        Line::from(Span::styled(line, Style::default().fg(Color::Cyan)))
                    })
                    .collect();
                let h: u16 = text
                    .lines()
                    .map(|line| line.chars().count().div_ceil(width).max(1) as u16)
                    .sum::<u16>()
                    .max(1);
                (lines, h)
            }
            Message::System(text) => {
                let lines = vec![Line::from(vec![
                    Span::styled("[", Style::default().fg(Color::Yellow)),
                    Span::styled(text.as_str(), Style::default().fg(Color::Yellow)),
                    Span::styled("]", Style::default().fg(Color::Yellow)),
                ])];
                let h = (text.chars().count() + 2).div_ceil(width).max(1) as u16;
                (lines, h)
            }
        };

        (Paragraph::new(lines).wrap(Wrap { trim: false }), height)
    });

    let list_view = ListView::new(builder, msg_count);
    frame.render_stateful_widget(list_view, area, &mut tab.list);
}

pub fn render_session_tab(frame: &mut Frame, tab: &mut SessionTab, tick: usize, area: Rect) {
    let _ = tick; // tick is used in the tab bar, not directly here
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1), // conversation log (+ optional stderr)
            Constraint::Length(session_input_height(tab, area.width)), // input (+ optional fork context block)
        ])
        .split(area);

    if tab.show_stderr {
        // Split the main area horizontally: conversation left, stderr right.
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(60), // conversation
                Constraint::Percentage(40), // stderr
            ])
            .split(chunks[0]);

        render_conversation_list(frame, tab, horiz[0]);

        // Stderr panel.
        let stderr_lines: Vec<Line> = tab
            .stderr_lines
            .iter()
            .map(|l| Line::from(Span::styled(l.as_str(), Style::default().fg(Color::Red))))
            .collect();

        let stderr_border_style = if tab.focused_panel == FocusedPanel::Stderr {
            Style::default().fg(Color::Blue)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let stderr_len = stderr_lines.len();
        let stderr_panel = Paragraph::new(stderr_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Stderr")
                    .border_style(stderr_border_style),
            )
            .wrap(Wrap { trim: false })
            .scroll((tab.stderr_scroll, 0));
        frame.render_widget(stderr_panel, horiz[1]);

        let stderr_scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight).symbols(Set {
            track: "\u{2502}",
            thumb: "\u{2588}",
            begin: "\u{25b2}",
            end: "\u{25bc}",
        });
        let mut stderr_scrollbar_state = ScrollbarState::default()
            .content_length(stderr_len)
            .position(tab.stderr_scroll as usize);
        frame.render_stateful_widget(
            stderr_scrollbar,
            horiz[1].inner(ratatui::layout::Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut stderr_scrollbar_state,
        );
    } else {
        // Full-width conversation.
        render_conversation_list(frame, tab, chunks[0]);
    }

    // Input area using TextArea widget.
    if let Some(fork_message) = tab.pending_fork_message.as_deref() {
        let hint_block_height = fork_hint_block_height(fork_message, chunks[1].width);
        let input_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(hint_block_height),
                Constraint::Length(AGENT_INPUT_HEIGHT),
            ])
            .spacing(Spacing::Overlap(1))
            .split(chunks[1]);
        let hint_area = input_chunks[0];
        let input_area = input_chunks[1];

        let input_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .merge_borders(MergeStrategy::Exact)
            .title(tab.input_title());

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
        frame.render_widget(hint, hint_area);

        if tab.waiting {
            let waiting_msg = Paragraph::new(" (waiting for agent...)").block(input_block);
            frame.render_widget(waiting_msg, input_area);
        } else {
            tab.textarea.set_block(input_block);
            frame.render_widget(&tab.textarea, input_area);
        }
    } else if tab.waiting {
        let waiting_msg = Paragraph::new(" (waiting for agent...)").block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title(tab.input_title()),
        );
        frame.render_widget(waiting_msg, chunks[1]);
    } else {
        tab.textarea.set_block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title(tab.input_title()),
        );
        frame.render_widget(&tab.textarea, chunks[1]);
    }
}
