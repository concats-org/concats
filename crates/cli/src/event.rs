use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, MouseEvent};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

/// Terminal events.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// Terminal tick.
    Tick,
    /// Key press.
    Key(KeyEvent),
    /// Mouse click/scroll.
    Mouse(MouseEvent),
    /// Terminal resize.
    Resize(u16, u16),
}

/// Terminal event handler.
#[derive(Debug)]
pub struct EventHandler {
    /// Event receiver.
    receiver: mpsc::UnboundedReceiver<Event>,
}

impl EventHandler {
    /// Constructs a new instance of [`EventHandler`].
    pub fn new(tick_rate: Duration) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let _sender = sender.clone();

        tokio::spawn(async move {
            let mut reader = event::EventStream::new();
            let mut tick = tokio::time::interval(tick_rate);

            loop {
                let tick_delay = tick.tick();
                let crossterm_event = reader.next();

                tokio::select! {
                    _ = tick_delay => {
                        sender.send(Event::Tick).ok();
                    }
                    Some(Ok(evt)) = crossterm_event => {
                        match evt {
                            CrosstermEvent::Key(key) => {
                                sender.send(Event::Key(key)).ok();
                            }
                            CrosstermEvent::Mouse(mouse) => {
                                sender.send(Event::Mouse(mouse)).ok();
                            }
                            CrosstermEvent::Resize(x, y) => {
                                sender.send(Event::Resize(x, y)).ok();
                            }
                            _ => {}
                        }
                    }
                };
            }
        });

        Self { receiver }
    }

    /// Receive the next event from the handler.
    ///
    /// This function will always block the current thread if there is no data available and it's
    /// possible for more data to be sent.
    pub async fn next(&mut self) -> Option<Event> {
        self.receiver.recv().await
    }
}
