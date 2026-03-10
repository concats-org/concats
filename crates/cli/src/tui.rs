use std::io;

use crossterm::{
    ExecutableCommand,
    event::{DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Size};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

const TICK_RATE: std::time::Duration = std::time::Duration::from_millis(80);
const FRAME_RATE: std::time::Duration = std::time::Duration::from_millis(33);

#[derive(Clone, Copy, Debug)]
pub enum Event {
    Tick,
    Render,
    Key(crossterm::event::KeyEvent),
    Mouse(crossterm::event::MouseEvent),
    Resize(Size),
}

#[derive(Debug)]
pub struct Tui {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    receiver: mpsc::UnboundedReceiver<Event>,
    sender: mpsc::UnboundedSender<Event>,
    cancel_token: CancellationToken,
    event_task: Option<JoinHandle<()>>,
    entered: bool,
}

impl Tui {
    #[must_use]
    pub fn new(terminal: Terminal<CrosstermBackend<io::Stdout>>) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            terminal,
            receiver,
            sender,
            cancel_token: CancellationToken::new(),
            event_task: None,
            entered: false,
        }
    }

    /// Enter raw-mode TUI operation and start background event polling.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal cannot be switched into alternate
    /// screen raw mode.
    pub fn enter(&mut self) -> miette::Result<()> {
        enable_raw_mode().map_err(|error| miette::miette!("failed to enable raw mode: {error}"))?;
        io::stdout()
            .execute(EnterAlternateScreen)
            .map_err(|error| miette::miette!("failed to enter alternate screen: {error}"))?;
        io::stdout()
            .execute(EnableMouseCapture)
            .map_err(|error| miette::miette!("failed to enable mouse capture: {error}"))?;
        self.terminal
            .hide_cursor()
            .map_err(|error| miette::miette!("failed to hide cursor: {error}"))?;
        self.terminal
            .clear()
            .map_err(|error| miette::miette!("failed to clear terminal: {error}"))?;
        self.start_event_task();
        self.entered = true;
        Ok(())
    }

    pub async fn next(&mut self) -> Option<Event> {
        self.receiver.recv().await
    }

    /// Read the current terminal size.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal backend cannot report its size.
    pub fn size(&self) -> miette::Result<Size> {
        self.terminal
            .size()
            .map_err(|error| miette::miette!("failed to read terminal size: {error}"))
    }

    /// Draw a single frame.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal backend cannot render the frame.
    pub fn draw<F>(&mut self, render: F) -> miette::Result<()>
    where
        F: FnOnce(&mut ratatui::Frame),
    {
        self.terminal
            .draw(render)
            .map(|_| ())
            .map_err(|error| miette::miette!("draw error: {error}"))
    }

    pub fn stop(&mut self) {
        self.cancel_token.cancel();
        if let Some(task) = self.event_task.take() {
            task.abort();
        }
    }

    /// Restore the terminal and stop background event polling.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal cannot be restored to its normal
    /// state.
    pub fn exit(&mut self) -> miette::Result<()> {
        self.stop();
        self.entered = false;

        disable_raw_mode()
            .map_err(|error| miette::miette!("failed to disable raw mode: {error}"))?;
        io::stdout()
            .execute(LeaveAlternateScreen)
            .map_err(|error| miette::miette!("failed to leave alternate screen: {error}"))?;
        io::stdout()
            .execute(DisableMouseCapture)
            .map_err(|error| miette::miette!("failed to disable mouse capture: {error}"))?;
        self.terminal
            .show_cursor()
            .map_err(|error| miette::miette!("failed to show cursor: {error}"))?;
        Ok(())
    }

    fn start_event_task(&mut self) {
        let sender = self.sender.clone();
        let cancel = self.cancel_token.clone();
        self.event_task = Some(tokio::spawn(async move {
            let mut reader = crossterm::event::EventStream::new();
            let mut tick = tokio::time::interval(TICK_RATE);
            let mut frame_timer = tokio::time::interval(FRAME_RATE);

            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    _ = tick.tick() => {
                        let _ = sender.send(Event::Tick);
                    }
                    _ = frame_timer.tick() => {
                        let _ = sender.send(Event::Render);
                    }
                    maybe_event = reader.next() => {
                        match maybe_event {
                            Some(Ok(CrosstermEvent::Key(key))) => {
                                let _ = sender.send(Event::Key(key));
                            }
                            Some(Ok(CrosstermEvent::Mouse(mouse))) => {
                                let _ = sender.send(Event::Mouse(mouse));
                            }
                            Some(Ok(CrosstermEvent::Resize(width, height))) => {
                                let _ = sender.send(Event::Resize(Size::new(width, height)));
                            }
                            Some(Ok(_)) => {}
                            Some(Err(error)) => {
                                tracing::warn!("terminal event stream error: {error}");
                                break;
                            }
                            None => break,
                        }
                    }
                }
            }
        }));
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        self.stop();
        if self.entered {
            best_effort_restore_terminal(&mut self.terminal);
        }
    }
}

pub fn install_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        if let Err(error) = disable_raw_mode() {
            eprintln!("failed to disable raw mode during panic: {error}");
        }
        if let Err(error) = io::stdout().execute(LeaveAlternateScreen) {
            eprintln!("failed to leave alternate screen during panic: {error}");
        }
        if let Err(error) = io::stdout().execute(DisableMouseCapture) {
            eprintln!("failed to disable mouse capture during panic: {error}");
        }
        previous_hook(panic_info);
    }));
}

fn best_effort_restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) {
    if let Err(error) = disable_raw_mode() {
        tracing::warn!("failed to disable raw mode during tui drop: {error}");
    }
    if let Err(error) = io::stdout().execute(LeaveAlternateScreen) {
        tracing::warn!("failed to leave alternate screen during tui drop: {error}");
    }
    if let Err(error) = io::stdout().execute(DisableMouseCapture) {
        tracing::warn!("failed to disable mouse capture during tui drop: {error}");
    }
    if let Err(error) = terminal.show_cursor() {
        tracing::warn!("failed to show cursor during tui drop: {error}");
    }
}
