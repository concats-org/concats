use agent_client_protocol::SessionNotification;
use tokio::sync::mpsc;

/// Bridges `!Send` ACP notifications into a `Send`-safe channel.
///
/// Lives inside the `!Send` `LocalSet` context and forwards
/// `SessionNotification`s to a channel that can be received on any thread.
pub struct NotificationSender {
    tx: mpsc::UnboundedSender<SessionNotification>,
}

impl NotificationSender {
    pub fn new(tx: mpsc::UnboundedSender<SessionNotification>) -> Self {
        Self { tx }
    }

    pub fn send(&self, notification: SessionNotification) {
        // If the receiver is dropped, we just silently drop the notification.
        let _ = self.tx.send(notification);
    }
}
