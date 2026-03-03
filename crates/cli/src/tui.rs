use std::io;

use crossterm::{
    ExecutableCommand,
    event::{DisableMouseCapture, EnableMouseCapture},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::event::EventHandler;

/// Representation of a terminal user interface.
///
/// It is responsible for setting up the terminal,
/// initializing the interface and handling the event loop.
#[derive(Debug)]
pub struct Tui {
    /// Terminal interface.
    pub terminal: Terminal<CrosstermBackend<io::Stdout>>,
    /// Terminal event handler.
    pub events: EventHandler,
}

impl Tui {
    /// Constructs a new instance of [`Tui`].
    pub fn new(terminal: Terminal<CrosstermBackend<io::Stdout>>, events: EventHandler) -> Self {
        Self { terminal, events }
    }

    /// Initializes the terminal interface.
    ///
    /// It enables the raw mode and sets up the terminal properties.
    pub fn init(&mut self) -> miette::Result<()> {
        enable_raw_mode().map_err(|e| miette::miette!("failed to enable raw mode: {e}"))?;
        io::stdout()
            .execute(EnterAlternateScreen)
            .map_err(|e| miette::miette!("failed to enter alternate screen: {e}"))?;
        io::stdout()
            .execute(EnableMouseCapture)
            .map_err(|e| miette::miette!("failed to enable mouse capture: {e}"))?;
        self.terminal
            .hide_cursor()
            .map_err(|e| miette::miette!("failed to hide cursor: {e}"))?;
        self.terminal
            .clear()
            .map_err(|e| miette::miette!("failed to clear terminal: {e}"))?;
        Ok(())
    }

    /// Resets the terminal interface.
    ///
    /// This function is also used for the panic hook to revert
    /// the terminal properties if the application crashes.
    pub fn exit(&mut self) -> miette::Result<()> {
        disable_raw_mode().ok();
        io::stdout().execute(LeaveAlternateScreen).ok();
        io::stdout().execute(DisableMouseCapture).ok();
        self.terminal
            .show_cursor()
            .map_err(|e| miette::miette!("failed to show cursor: {e}"))?;
        Ok(())
    }
}
