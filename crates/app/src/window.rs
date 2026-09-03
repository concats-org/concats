//! One window's own state: the review document it renders, the load that
//! produced it, and the identity its terminals carry.
//!
//! All of this was process-wide while there was one window. What stayed global
//! is what is genuinely shared — the worker threads, and the review store's
//! published state, which is keyed by repo because two windows on one repo
//! should see one set of comments and ticks.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};

use crate::{makepad_widgets::LiveId, review_doc::ReviewDoc};

/// Handed to the window's `ReviewPane` when the window opens, and from there
/// to its rows through `Scope`. Cloning one is an `Arc` clone.
pub(crate) struct WindowState {
    /// The id `Root` opened this window under, and what a worker's reply names
    /// to say which document it belongs to.
    pub id: LiveId,
    /// This window's row in `app.db`, exported to its terminals as
    /// `CONCATS_APP_WINDOW` so a bare `concats` command follows this window.
    pub key: String,
    /// The published document. A draw clones the `Arc` and releases the lock
    /// before painting; writers replace or mutate the uniquely held snapshot.
    doc: RwLock<Arc<ReviewDoc>>,
    /// Loads are superseded rather than cancelled: one that is no longer the
    /// newest drops its result instead of landing it.
    load_request: AtomicU64,
}

impl WindowState {
    pub(crate) fn new(id: LiveId) -> Arc<Self> {
        Arc::new(Self {
            id,
            key: concats_state::new_window_id(),
            doc: RwLock::new(Arc::new(ReviewDoc::default())),
            load_request: AtomicU64::new(0),
        })
    }

    pub(crate) fn read<R>(&self, f: impl FnOnce(&ReviewDoc) -> R) -> R {
        let document = self.doc.read().unwrap().clone();
        f(&document)
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut ReviewDoc) -> R) -> R {
        f(Arc::make_mut(&mut self.doc.write().unwrap()))
    }

    /// A write guard, for the handful of gestures that mutate the document
    /// across an early return and cannot be expressed as a closure.
    pub(crate) fn write(&self) -> std::sync::RwLockWriteGuard<'_, Arc<ReviewDoc>> {
        self.doc.write().unwrap()
    }

    /// The document as it stands, for a draw or for a worker that has to read
    /// it off the UI thread.
    pub(crate) fn snapshot(&self) -> Arc<ReviewDoc> {
        self.doc.read().unwrap().clone()
    }

    /// Claim the next load. The number comes back so the load can ask whether
    /// it is still the newest when it finishes.
    pub(crate) fn next_load(&self) -> u64 {
        self.load_request.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn load_is_current(&self, request: u64) -> bool {
        self.load_request.load(Ordering::Acquire) == request
    }

    /// Land a freshly built document, carrying the view state that belongs to
    /// the window rather than to the load.
    pub(crate) fn land(&self, mut next: ReviewDoc) {
        let mut snapshot = self.doc.write().unwrap();
        next.folded.clone_from(&snapshot.folded);
        next.show_all_comments
            .clone_from(&snapshot.show_all_comments);
        next.generation = snapshot.generation + 1;
        *snapshot = Arc::new(next);
    }
}
