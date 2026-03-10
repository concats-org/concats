use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use tokio::sync::mpsc;

use crate::{action::Action, components::Component};

pub struct AgentPickerComponent {
    action_tx: Option<mpsc::UnboundedSender<Action>>,
    pub agents: Vec<(String, String)>,
    pub selected: usize,
}

impl AgentPickerComponent {
    #[must_use]
    pub fn new(agents: Vec<(String, String)>) -> Self {
        Self {
            action_tx: None,
            agents,
            selected: 0,
        }
    }

    fn send_action(&self, action: Action) {
        if let Some(action_tx) = &self.action_tx {
            let _ = action_tx.send(action);
        }
    }
}

impl Component for AgentPickerComponent {
    fn register_action_handler(&mut self, tx: mpsc::UnboundedSender<Action>) {
        self.action_tx = Some(tx);
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> miette::Result<()> {
        match key.code {
            KeyCode::Up => self.send_action(Action::AgentPickerSelectPrev),
            KeyCode::Down => self.send_action(Action::AgentPickerSelectNext),
            KeyCode::Enter => self.send_action(Action::CreateSession(self.selected)),
            KeyCode::Esc => self.send_action(Action::CloseAgentPicker),
            _ => {}
        }
        Ok(())
    }

    fn update(&mut self, action: &Action) -> miette::Result<()> {
        match action {
            Action::AgentPickerSelectPrev => {
                self.selected = self.selected.saturating_sub(1);
            }
            Action::AgentPickerSelectNext => {
                if self.selected + 1 < self.agents.len() {
                    self.selected += 1;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame, _area: Rect) {
        let area = frame.area();
        let popup_width = 40u16.min(area.width.saturating_sub(4));
        let popup_height = u16::try_from(self.agents.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(popup_width)) / 2;
        let y = (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        frame.render_widget(Clear, popup_area);

        let lines: Vec<Line> = self
            .agents
            .iter()
            .enumerate()
            .map(|(index, (_id, display_name))| {
                let style = if index == self.selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                Line::from(Span::styled(format!(" {display_name} "), style))
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Select Agent")
            .border_style(Style::default().fg(Color::Cyan));
        let list = Paragraph::new(lines).block(block);
        frame.render_widget(list, popup_area);
    }
}
