//! Keeping a Makepad UI thread free: **commands out, snapshots in**.
//!
//! Makepad gives you the transport for this — widget actions upward,
//! [`Scope`](makepad_platform) data downward, `Cx::post_action` from a
//! background thread — but no opinion about *where state lives* or *who does
//! I/O*. Without one, the natural thing happens: a global `Mutex<Doc>` that the
//! render path locks and a worker holds for a second, and the window freezes.
//!
//! This crate is that missing opinion, in three types:
//!
//! - [`Service`] — owns the data and does every effect (files, sockets,
//!   databases, parsing). Runs on its own thread and never sees a `Cx`, so it
//!   is a plain state machine you can unit-test.
//! - [`Worker`] — the handle the UI holds. [`Worker::send`] hands a command to
//!   the service and returns immediately; there is no way to block on a reply.
//! - [`Shared`] — what the service publishes and the UI reads.
//!   [`Shared::load`] is one `Arc` clone, so the draw path never waits on a
//!   writer.
//!
//! …and one function, [`notify`], to wake the UI when a snapshot changes.
//!
//! ## The rules
//!
//! 1. The UI thread never performs an effect. If it would touch a disk, a
//!    socket or a parser, it sends a command instead.
//! 2. The service never touches a widget. It publishes a snapshot and calls
//!    [`notify`]; the UI decides what that means for pixels.
//! 3. A snapshot is immutable. Publishing replaces it wholesale — readers keep
//!    the `Arc` they already have and see a consistent world for the frame.
//!
//! Interactions that must feel instant (a checkbox, a toggle) apply the change
//! to the UI's own copy first and let the published snapshot overwrite it when
//! it lands. That optimistic overlay is the app's business, since only the app
//! knows what a half-applied edit means, so it is not modelled here.
//!
//! ```no_run
//! use makepad_service::{Service, Shared, Worker, notify};
//!
//! struct Counter { total: u32, out: Shared<u32> }
//! enum Cmd { Add(u32) }
//!
//! impl Service for Counter {
//!     type Cmd = Cmd;
//!     fn handle(&mut self, cmd: Cmd) {
//!         let Cmd::Add(n) = cmd;
//!         self.total += n;              // the effect would go here
//!         self.out.publish(self.total); // …then publish what the UI reads
//!     }
//! }
//! ```

use std::sync::{Arc, Mutex, PoisonError, mpsc};

use makepad_platform::{ActionTrait, Cx};

/// The data owner: one per concern, running on its own thread.
///
/// Commands arrive in the order they were sent, one at a time, so a service
/// needs no internal locking — it *is* the lock.
pub trait Service: Send + 'static {
    /// Everything the UI can ask for. One enum keeps the surface reviewable.
    type Cmd: Send + 'static;

    /// Apply one command. Slow is fine here, that is what the thread is for —
    /// but a command that takes minutes should publish progress as it goes
    /// rather than leave the queue stalled behind it.
    fn handle(&mut self, cmd: Self::Cmd);
}

/// The UI's handle on a [`Service`]. Cheap to clone, safe to keep in a widget.
pub struct Worker<C> {
    tx: mpsc::Sender<C>,
}

impl<C> Clone for Worker<C> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<C: Send + 'static> Worker<C> {
    /// Start `service` on its own thread. The thread ends when every `Worker`
    /// handle has been dropped, which for an app-lifetime service is exit.
    pub fn spawn<S: Service<Cmd = C>>(mut service: S) -> Self {
        let (tx, rx) = mpsc::channel::<C>();
        std::thread::spawn(move || {
            while let Ok(cmd) = rx.recv() {
                service.handle(cmd);
            }
        });
        Self { tx }
    }

    /// Hand a command to the service. Never blocks, never fails loudly: a
    /// dead service means the app is shutting down.
    pub fn send(&self, cmd: C) {
        let _ = self.tx.send(cmd);
    }
}

/// A snapshot slot: the service publishes, the UI reads.
///
/// The mutex is held only long enough to clone or replace an `Arc`, so a
/// reader never waits on the work that produced the value — only on the
/// pointer swap that published it.
pub struct Shared<T>(Arc<Mutex<Arc<T>>>);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Shared<T> {
    #[must_use]
    pub fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Arc::new(value))))
    }

    /// The current snapshot. One `Arc` clone — call it once per draw and use
    /// that value for the whole frame, so the frame is consistent even if the
    /// service publishes mid-draw.
    #[must_use]
    pub fn load(&self) -> Arc<T> {
        Arc::clone(&self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Replace the snapshot. Readers holding the old one are unaffected.
    pub fn publish(&self, value: T) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(value);
    }

    /// Replace the snapshot with one derived from the current value — for a
    /// UI-side optimistic edit, or a service that patches rather than rebuilds.
    pub fn update(&self, f: impl FnOnce(&T) -> T) {
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = Arc::new(f(&*slot));
    }
}

/// Wake the UI with a typed action, from any thread.
///
/// The app handles it in `MatchEvent::handle_actions`, where it can read the
/// new snapshot and redraw. Actions are the whole reply channel: a service
/// tells the UI *that* something changed, never *what to draw*.
pub fn notify(action: impl ActionTrait + Send) {
    Cx::post_action(action);
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    struct Adder {
        total: u32,
        out: Shared<u32>,
        done: mpsc::Sender<()>,
    }

    enum Cmd {
        Add(u32),
    }

    impl Service for Adder {
        type Cmd = Cmd;
        fn handle(&mut self, cmd: Cmd) {
            let Cmd::Add(n) = cmd;
            self.total += n;
            self.out.publish(self.total);
            let _ = self.done.send(());
        }
    }

    #[test]
    fn commands_apply_in_order_and_publish() {
        let out = Shared::new(0u32);
        let (done, ticks) = mpsc::channel();
        let worker = Worker::spawn(Adder {
            total: 0,
            out: out.clone(),
            done,
        });

        worker.send(Cmd::Add(2));
        worker.send(Cmd::Add(40));
        ticks.recv().unwrap();
        ticks.recv().unwrap();

        assert_eq!(*out.load(), 42);
    }

    #[test]
    fn readers_keep_a_consistent_snapshot_across_a_publish() {
        let shared = Shared::new(vec![1, 2, 3]);
        let held = shared.load();
        shared.publish(vec![9]);
        assert_eq!(*held, vec![1, 2, 3]);
        assert_eq!(*shared.load(), vec![9]);
    }

    #[test]
    fn update_derives_the_next_snapshot_from_the_current_one() {
        let shared = Shared::new(1u32);
        shared.update(|n| n + 1);
        assert_eq!(*shared.load(), 2);
    }
}
