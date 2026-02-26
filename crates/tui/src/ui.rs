use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect, Spacing};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::merge::MergeStrategy;
use ratatui::symbols::scrollbar::Set;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};

use crate::app::{App, FocusedPanel, Message};
use crate::sessions_ui;
use crate::tabs::Tab;

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const TAB_BAR_HEIGHT: u16 = 1;
pub const AGENT_INPUT_HEIGHT: u16 = 4;
pub const FORK_HINT_MIN_BLOCK_HEIGHT: u16 = 3;
pub const FORK_HINT_MAX_BLOCK_HEIGHT: u16 = 9;

pub fn agent_input_height(app: &App, area_width: u16) -> u16 {
    AGENT_INPUT_HEIGHT
        + if let Some(fork_message) = app.pending_fork_message.as_deref() {
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
        Tab::Agent => render_agent_tab(frame, app, chunks[0]),
        Tab::Sessions => sessions_ui::render_sessions_tab(frame, app, chunks[0]),
        Tab::Settings => render_placeholder(frame, chunks[0], "Settings", "Not yet implemented."),
        Tab::Help => render_placeholder(
            frame,
            chunks[0],
            "Help",
            "Ctrl+1-4: switch tabs | Up/Down: navigate | Enter: expand | f: fork | r: refresh | Ctrl+C: quit",
        ),
    }

    // Render tab bar.
    render_tab_bar(frame, app, chunks[1]);
}

fn render_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = Vec::new();
    spans.extend(status_spans(app));
    spans.push(Span::raw("  "));

    for tab in Tab::all() {
        let idx = tab.index() + 1;
        if *tab == app.active_tab {
            spans.push(Span::styled(
                format!(" {idx}:{} ", tab.label()),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                format!(" {idx}:{} ", tab.label()),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    let tab_bar = Paragraph::new(Line::from(spans));
    frame.render_widget(tab_bar, area);
}

fn status_spans(app: &App) -> Vec<Span<'static>> {
    if app.waiting {
        let spinner = SPINNER_FRAMES[app.tick % SPINNER_FRAMES.len()];
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
                "●".to_string(),
                Style::default().fg(status_color(app.status.as_str())),
            ),
            Span::styled(
                format!(" {}", app.status),
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
        "disconnected" | "session ended" => Color::DarkGray,
        _ => Color::Green,
    }
}

pub fn tab_click_hitboxes(app: &App) -> Vec<(Tab, usize, usize)> {
    let mut hitboxes = Vec::new();
    let mut pos = status_cell_width(app) + 2;

    for tab in Tab::all() {
        let idx = tab.index() + 1;
        let label = format!(" {idx}:{} ", tab.label());
        let start = pos;
        let end = start + label.chars().count();
        hitboxes.push((*tab, start, end));
        pos = end;
    }

    hitboxes
}

fn status_cell_width(app: &App) -> usize {
    if app.waiting {
        "⠋ working".chars().count()
    } else {
        format!("● {}", app.status).chars().count()
    }
}

fn render_placeholder(frame: &mut Frame, area: Rect, title: &str, content: &str) {
    let p = Paragraph::new(content)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

pub fn render_agent_tab(frame: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1), // conversation log (+ optional stderr)
            Constraint::Length(agent_input_height(app, area.width)), // input (+ optional fork context block)
        ])
        .split(area);

    // Build conversation lines.
    let mut conv_lines: Vec<Line> = Vec::new();
    for msg in &app.messages {
        match msg {
            Message::User(text) => {
                conv_lines.push(Line::from(vec![
                    Span::styled(
                        "> ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text),
                ]));
            }
            Message::Agent(text) => {
                for line in text.lines() {
                    conv_lines.push(Line::from(Span::styled(
                        line,
                        Style::default().fg(Color::Cyan),
                    )));
                }
            }
            Message::System(text) => {
                conv_lines.push(Line::from(vec![
                    Span::styled("[", Style::default().fg(Color::Yellow)),
                    Span::styled(text, Style::default().fg(Color::Yellow)),
                    Span::styled("]", Style::default().fg(Color::Yellow)),
                ]));
            }
        }
    }

    if app.show_stderr {
        // Split the main area horizontally: conversation left, stderr right.
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(60), // conversation
                Constraint::Percentage(40), // stderr
            ])
            .split(chunks[0]);

        let conv_len = conv_lines.len();
        let conversation = Paragraph::new(conv_lines)
            .wrap(Wrap { trim: false })
            .scroll((app.scroll_offset, 0));
        frame.render_widget(conversation, horiz[0]);

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight).symbols(Set {
            track: "│",
            thumb: "█",
            begin: "▲",
            end: "▼",
        });
        let mut scrollbar_state = ScrollbarState::default()
            .content_length(conv_len)
            .position(app.scroll_offset as usize);
        frame.render_stateful_widget(scrollbar, horiz[0], &mut scrollbar_state);

        // Stderr panel.
        let stderr_lines: Vec<Line> = app
            .stderr_lines
            .iter()
            .map(|l| Line::from(Span::styled(l.as_str(), Style::default().fg(Color::Red))))
            .collect();

        let stderr_border_style = if app.focused_panel == FocusedPanel::Stderr {
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
            .scroll((app.stderr_scroll, 0));
        frame.render_widget(stderr_panel, horiz[1]);

        let stderr_scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight).symbols(Set {
            track: "│",
            thumb: "█",
            begin: "▲",
            end: "▼",
        });
        let mut stderr_scrollbar_state = ScrollbarState::default()
            .content_length(stderr_len)
            .position(app.stderr_scroll as usize);
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
        let conv_len = conv_lines.len();
        let conversation = Paragraph::new(conv_lines)
            .wrap(Wrap { trim: false })
            .scroll((app.scroll_offset, 0));
        frame.render_widget(conversation, chunks[0]);

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight).symbols(Set {
            track: "│",
            thumb: "█",
            begin: "▲",
            end: "▼",
        });
        let mut scrollbar_state = ScrollbarState::default()
            .content_length(conv_len)
            .position(app.scroll_offset as usize);
        frame.render_stateful_widget(scrollbar, chunks[0], &mut scrollbar_state);
    }

    // Input area using TextArea widget.
    if let Some(fork_message) = app.pending_fork_message.as_deref() {
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
            .title(app.input_title());

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

        if app.waiting {
            let waiting_msg = Paragraph::new(" (waiting for agent...)").block(input_block);
            frame.render_widget(waiting_msg, input_area);
        } else {
            app.textarea.set_block(input_block);
            frame.render_widget(&app.textarea, input_area);
        }
    } else if app.waiting {
        let waiting_msg = Paragraph::new(" (waiting for agent...)").block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title(app.input_title()),
        );
        frame.render_widget(waiting_msg, chunks[1]);
    } else {
        app.textarea.set_block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title(app.input_title()),
        );
        frame.render_widget(&app.textarea, chunks[1]);
    }
}
