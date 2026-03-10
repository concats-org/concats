use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use throbber_widgets_tui::{Throbber, ThrobberState};
use tokio::sync::mpsc;

use crate::{
    action::Action::{self, Tick},
    components::Component,
    tabs::{ActiveTab, ClickTarget, TabBarEntry},
};

pub const TAB_BAR_HEIGHT: u16 = 1;

#[derive(Debug, Clone)]
pub struct ChromeModel {
    pub active_tab: ActiveTab,
    pub entries: Vec<TabBarEntry>,
    pub waiting: bool,
    pub status: String,
}

pub struct ChromeComponent {
    action_tx: Option<mpsc::UnboundedSender<Action>>,
    model: ChromeModel,
    throbber_state: ThrobberState,
}

impl ChromeComponent {
    #[must_use]
    pub fn new() -> Self {
        Self {
            action_tx: None,
            model: ChromeModel {
                active_tab: ActiveTab::Sessions,
                entries: Vec::new(),
                waiting: false,
                status: String::from("no session"),
            },
            throbber_state: ThrobberState::default(),
        }
    }

    pub fn sync(&mut self, model: ChromeModel) {
        self.model = model;
    }

    fn send_action(&self, action: Action) {
        if let Some(action_tx) = &self.action_tx {
            let _ = action_tx.send(action);
        }
    }

    #[must_use]
    pub fn hit_test(&self, x: u16) -> Option<ClickTarget> {
        let mut pos = status_cell_width(&self.model, &self.throbber_state) + 2;
        let x = x as usize;

        for entry in &self.model.entries {
            match entry {
                TabBarEntry::Session { id, label } => {
                    let label_text = format!(" {label} ");
                    let label_start = pos;
                    let label_end = label_start + label_text.chars().count();
                    if x >= label_start && x < label_end {
                        return Some(ClickTarget::SwitchTab(ActiveTab::Session(*id)));
                    }
                    pos = label_end;

                    let close_text = "✕ ";
                    let close_start = pos;
                    let close_end = close_start + close_text.chars().count();
                    if x >= close_start && x < close_end {
                        return Some(ClickTarget::CloseSession(*id));
                    }
                    pos = close_end;
                }
                TabBarEntry::NewButton => {
                    let text = " [+] ";
                    let start = pos;
                    let end = start + text.chars().count();
                    if x >= start && x < end {
                        return Some(ClickTarget::NewSession);
                    }
                    pos = end;
                }
                TabBarEntry::Utility { tab, label } => {
                    let text = format!(" {label} ");
                    let start = pos;
                    let end = start + text.chars().count();
                    if x >= start && x < end {
                        return Some(ClickTarget::SwitchTab(*tab));
                    }
                    pos = end;
                }
            }
        }

        None
    }
}

impl Default for ChromeComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for ChromeComponent {
    fn register_action_handler(&mut self, tx: mpsc::UnboundedSender<Action>) {
        self.action_tx = Some(tx);
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent, area: Rect) -> miette::Result<()> {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Ok(());
        }
        if mouse.row != area.y {
            return Ok(());
        }
        if let Some(target) = self.hit_test(mouse.column) {
            match target {
                ClickTarget::SwitchTab(tab) => self.send_action(Action::SwitchTab(tab)),
                ClickTarget::CloseSession(id) => self.send_action(Action::CloseSession(id)),
                ClickTarget::NewSession => self.send_action(Action::OpenAgentPicker),
            }
        }
        Ok(())
    }

    fn update(&mut self, action: &Action) -> miette::Result<()> {
        if matches!(action, Tick) && self.model.waiting {
            self.throbber_state.calc_next();
        }
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let mut spans = status_spans(&self.model, &self.throbber_state);
        spans.push(Span::raw("  "));

        for entry in &self.model.entries {
            match entry {
                TabBarEntry::Session { id, label } => {
                    let is_active = self.model.active_tab == ActiveTab::Session(*id);
                    if is_active {
                        spans.push(Span::styled(
                            format!(" {label} "),
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ));
                        spans.push(Span::styled(
                            "✕ ",
                            Style::default().fg(Color::DarkGray).bg(Color::White),
                        ));
                    } else {
                        spans.push(Span::styled(
                            format!(" {label} "),
                            Style::default().fg(Color::DarkGray),
                        ));
                        spans.push(Span::styled(
                            "✕ ",
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
                    let is_active = self.model.active_tab == *tab;
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

        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

fn status_spans(model: &ChromeModel, throbber_state: &ThrobberState) -> Vec<Span<'static>> {
    if model.waiting {
        Throbber::default()
            .label(Span::styled(
                "working".to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::DIM),
            ))
            .throbber_style(Style::default().fg(Color::Yellow))
            .to_line(throbber_state)
            .spans
    } else {
        vec![
            Span::styled(
                "●".to_string(),
                Style::default().fg(status_color(&model.status)),
            ),
            Span::styled(
                format!(" {}", model.status),
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

fn status_cell_width(model: &ChromeModel, throbber_state: &ThrobberState) -> usize {
    status_spans(model, throbber_state)
        .iter()
        .map(|span| span.content.chars().count())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_uses_session_and_utility_ranges() {
        let mut chrome = ChromeComponent::new();
        chrome.sync(ChromeModel {
            active_tab: ActiveTab::Sessions,
            entries: vec![
                TabBarEntry::Session {
                    id: 1,
                    label: "one".into(),
                },
                TabBarEntry::NewButton,
                TabBarEntry::Utility {
                    tab: ActiveTab::Sessions,
                    label: "Sessions",
                },
            ],
            waiting: false,
            status: "connected".into(),
        });

        assert!(matches!(
            chrome.hit_test(14),
            Some(ClickTarget::SwitchTab(ActiveTab::Session(1)))
        ));
    }
}
