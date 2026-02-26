use ratatui::widgets::{Widget, StatefulWidget};
use ratatui::layout::Rect;
use throbber_widgets_tui::{Throbber, ThrobberState};

pub fn dummy() {
    let _t = Throbber::default();
    let _s = ThrobberState::default();
}
