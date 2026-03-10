use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tokio::sync::mpsc;

use crate::{action::Action, components::Component};

pub struct StaticPageComponent {
    title: &'static str,
    content: &'static str,
}

impl StaticPageComponent {
    #[must_use]
    pub fn new(title: &'static str, content: &'static str) -> Self {
        Self { title, content }
    }
}

impl Component for StaticPageComponent {
    fn register_action_handler(&mut self, _tx: mpsc::UnboundedSender<Action>) {}

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let paragraph = Paragraph::new(self.content)
            .block(Block::default().borders(Borders::ALL).title(self.title))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }
}
