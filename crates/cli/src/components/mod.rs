use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Frame, layout::Rect};
use tokio::sync::mpsc;

use crate::action::Action;

pub mod agent_picker;
pub mod chrome;
pub(crate) mod list_navigation;
pub mod session;
pub mod sessions;
pub mod static_page;

pub trait Component {
    fn register_action_handler(&mut self, tx: mpsc::UnboundedSender<Action>);

    /// Handle a keyboard event for the component.
    ///
    /// # Errors
    ///
    /// Returns an error if the component cannot process the event.
    fn handle_key_event(&mut self, _key: KeyEvent) -> miette::Result<()> {
        Ok(())
    }

    /// Handle a mouse event for the component.
    ///
    /// # Errors
    ///
    /// Returns an error if the component cannot process the event.
    fn handle_mouse_event(&mut self, _mouse: MouseEvent, _area: Rect) -> miette::Result<()> {
        Ok(())
    }

    /// Apply an action update to the component state.
    ///
    /// # Errors
    ///
    /// Returns an error if the component cannot apply the action.
    fn update(&mut self, _action: &Action) -> miette::Result<()> {
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect);
}
