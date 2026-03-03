use agent_client_protocol::SessionNotification;
use tokio::sync::mpsc;

/// Bridges `!Send` ACP notifications into a `Send`-safe channel.
///
/// Lives inside the `!Send` `LocalSet` context and forwards
/// `SessionNotification`s to a channel that can be received on any thread.
pub struct NotificationSender {
    tx: mpsc::Sender<SessionNotification>,
}

impl NotificationSender {
    pub fn new(tx: mpsc::Sender<SessionNotification>) -> Self {
        Self { tx }
    }

    pub fn send(&self, notification: SessionNotification) {
        // If the receiver is dropped or buffer is full, we just silently drop the notification.
        // For ACP notifications (like text chunks), dropping is better than blocking
        // the ACP IO loop if the UI is lagging.
        let _ = self.tx.try_send(notification);
    }
}
