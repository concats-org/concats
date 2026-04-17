use std::{io, path::PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;

use crate::{
    action::Action,
    components::{Component, sessions::SessionsBrowserComponent},
    tui::{Event, Tui},
};

pub struct App {
    should_quit: bool,
    action_tx: mpsc::UnboundedSender<Action>,
    action_rx: mpsc::UnboundedReceiver<Action>,
    sessions_browser: SessionsBrowserComponent,
}

impl App {
    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        let (action_tx, action_rx) = mpsc::unbounded_channel();

        let mut sessions_browser = SessionsBrowserComponent::new(workspace_root);
        sessions_browser.register_action_handler(action_tx.clone());

        Self {
            should_quit: false,
            action_tx,
            action_rx,
            sessions_browser,
        }
    }

    /// Run the TUI event loop until the app exits.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal cannot be initialized or restored, or
    /// if event routing or drawing fails.
    pub async fn run(&mut self) -> miette::Result<()> {
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = Terminal::new(backend)
            .map_err(|error| miette::miette!("failed to create terminal: {error}"))?;
        let mut tui = Tui::new(terminal);
        tui.enter()?;

        self.send_action(Action::SessionsRefresh);
        self.send_action(Action::Render);

        while !self.should_quit {
            tokio::select! {
                maybe_event = tui.next() => {
                    match maybe_event {
                        Some(Event::Tick) => self.send_action(Action::Tick),
                        Some(Event::Render) => self.send_action(Action::Render),
                        Some(Event::Resize(size)) => self.send_action(Action::Resize(size)),
                        Some(Event::Key(key)) => self.route_key(key)?,
                        Some(Event::Mouse(mouse)) => self.route_mouse(mouse)?,
                        None => break,
                    }
                }
                maybe_action = self.action_rx.recv() => {
                    match maybe_action {
                        Some(action) => self.handle_action(&action, &mut tui)?,
                        None => break,
                    }
                }
            }
        }

        tui.exit()?;
        Ok(())
    }

    fn draw(&mut self, frame: &mut ratatui::Frame) {
        self.sessions_browser.draw(frame, frame.area());
    }

    fn send_action(&self, action: Action) {
        let _ = self.action_tx.send(action);
    }

    fn route_key(&mut self, key: KeyEvent) -> miette::Result<()> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.send_action(Action::Quit);
            return Ok(());
        }
        self.sessions_browser.handle_key_event(key)
    }

    fn route_mouse(&mut self, mouse: MouseEvent) -> miette::Result<()> {
        self.sessions_browser
            .handle_mouse_event(mouse, ratatui::layout::Rect::default())
    }

    fn handle_action(&mut self, action: &Action, tui: &mut Tui) -> miette::Result<()> {
        match action {
            Action::Render => tui.draw(|frame| self.draw(frame))?,
            Action::Quit => self.should_quit = true,
            _ => {}
        }
        self.sessions_browser.update(action)?;
        Ok(())
    }
}
