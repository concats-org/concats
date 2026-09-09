//! One window's own state: the review document it renders, the load that
//! produced it, and the identity its terminals carry.
//!
//! All of this was process-wide while there was one window. What stayed global
//! is what is genuinely shared — the worker threads, and the review store's
//! published state, which is keyed by repo because two windows on one repo
//! should see one set of comments and ticks.

use std::sync::{
    Arc, OnceLock, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use crate::{
    makepad_widgets::{LiveId, WindowId},
    review_doc::ReviewDoc,
};

/// Handed to the window's `ReviewPane` when the window opens, and from there
/// to its rows through `Scope`. Cloning one is an `Arc` clone.
pub(crate) struct WindowState {
    /// The id `Root` opened this window under, and what a worker's reply names
    /// to say which document it belongs to.
    pub id: LiveId,
    /// This window's row in `app.db`, exported to its terminals as
    /// `CONCATS_APP_WINDOW` so a bare `concats` command follows this window.
    pub key: String,
    /// The platform's id for this window, which is what a pointer event names.
    pub platform: Option<WindowId>,
    /// The published document. A draw clones the `Arc` and releases the lock
    /// before painting; writers replace or mutate the uniquely held snapshot.
    doc: RwLock<Arc<ReviewDoc>>,
    pub compose_draft: RwLock<String>,
    /// Loads are superseded rather than cancelled: one that is no longer the
    /// newest drops its result instead of landing it.
    load_request: AtomicU64,
    /// Whether this window has the OS focus.
    ///
    /// Keyboard events carry no window — makepad routes them through one
    /// global key focus — so every window's widgets see every keystroke, and
    /// anything that reads them without asking whose window it is acts on all
    /// of them at once. This is that question.
    focused: AtomicBool,
}

impl WindowState {
    pub(crate) fn new(id: LiveId, platform: Option<WindowId>) -> Arc<Self> {
        Arc::new(Self {
            id,
            key: concats_state::new_window_id(),
            platform,
            doc: RwLock::new(Arc::new(ReviewDoc::default())),
            compose_draft: RwLock::new(String::new()),
            load_request: AtomicU64::new(0),
            focused: AtomicBool::new(false),
        })
    }

    pub(crate) fn detached() -> &'static Arc<Self> {
        static DETACHED: OnceLock<Arc<WindowState>> = OnceLock::new();
        DETACHED.get_or_init(|| Self::new(LiveId(0), None))
    }

    pub(crate) fn is_focused(&self) -> bool {
        self.focused.load(Ordering::Relaxed)
    }

    pub(crate) fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::Relaxed);
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
        // NOTE: Starting a load must not race with landing or publishing one.
        let _document = self.doc.read().unwrap();
        self.load_request.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn load_is_current(&self, request: u64) -> bool {
        self.load_request.load(Ordering::Acquire) == request
    }

    /// Land a freshly built document, carrying the view state that belongs to
    /// the window rather than to the load.
    pub(crate) fn land(&self, request: u64, mut next: ReviewDoc) -> bool {
        let mut snapshot = self.doc.write().unwrap();
        if !self.load_is_current(request) {
            return false;
        }
        if next.error.is_some() && snapshot.git_dir.is_some() {
            let current = Arc::make_mut(&mut snapshot);
            current.error = next.error;
            current.loading = false;
            current.generation += 1;
            return true;
        }
        next.folded.clone_from(&snapshot.folded);
        next.show_all_comments
            .clone_from(&snapshot.show_all_comments);
        crate::load::carry_forward(&mut next, &snapshot);
        if next.repo != snapshot.repo {
            self.compose_draft.write().unwrap().clear();
        }
        next.generation = snapshot.generation + 1;
        *snapshot = Arc::new(next);
        true
    }
}

#[cfg(test)]
mod tests {
    use concats_diff::{Blob, Row, Side};

    use super::*;
    use crate::{
        file_view::open_file,
        review_doc::{Caret, Compose, Composing, Stream, splice_composer},
    };

    fn buffer(path: &str, text: &str) -> Blob {
        let mut blob = Blob::new(
            concats_sync::hash_object(text.as_bytes()),
            "rs".into(),
            text.into(),
        );
        blob.origin = Some(path.into());
        blob
    }

    #[test]
    #[expect(
        clippy::cognitive_complexity,
        reason = "The assertions cover one reload preserving the complete live editing state."
    )]
    fn a_successful_reload_merges_latest_typing_selection_and_composition() {
        let state = WindowState::new(LiveId(1), None);
        state.with(|d| {
            d.repo = "/repo".into();
            open_file(
                d,
                "a.rs",
                (None, buffer("/repo/a.rs", "first\nlast\n")),
                &[],
            );
        });
        let request = state.next_load();
        let mut next = ReviewDoc {
            repo: "/repo".into(),
            ..Default::default()
        };
        next.blobs.push(buffer("/repo/other.rs", "other\n"));
        open_file(
            &mut next,
            "a.rs",
            (None, buffer("/repo/a.rs", "first\nexternal\nlast\n")),
            &[],
        );
        state.with(|d| {
            let blob = d.files_open[0].head;
            d.blobs[blob as usize].edit(0..0, "typed\n");
            crate::file_view::relower_edited(d, &[]);
            d.tab = Stream::File(d.files_open[0].tab);
            d.caret = Some(Caret {
                blob,
                line: 2,
                byte: 1,
            });
            d.selection_anchor = Some(Caret {
                blob,
                line: 1,
                byte: 0,
            });
            d.compose = Some(Composing::Lines(Compose {
                old: None,
                new: Some(Side {
                    blob,
                    start: 2,
                    end: 2,
                }),
            }));
            splice_composer(d);
            d.compose_focus = true;
        });
        *state.compose_draft.write().unwrap() = "unfinished comment".into();
        assert!(state.land(request, next));
        state.read(|d| {
            let blob = &d.blobs[d.files_open[0].head as usize];
            assert!(blob.text.contains("typed\n"));
            assert!(blob.text.contains("external\n"));
            assert!(blob.dirty());
            let caret = d.caret.unwrap();
            assert_eq!(caret.blob, d.files_open[0].head);
            assert_eq!(blob.line_text(caret.line as usize), "last");
            assert_eq!(caret.byte, 1);
            assert_eq!(
                blob.line_text(d.selection_anchor.unwrap().line as usize),
                "first"
            );
            let Some(Composing::Lines(compose)) = d.compose else {
                panic!()
            };
            assert_eq!(compose.new.unwrap().start, caret.line);
            assert_eq!(d.composer_tab, Some(d.tab));
            assert!(d.compose_focus);
            assert!(d.active().iter().any(|row| matches!(row, Row::Composer)));
        });
        assert_eq!(*state.compose_draft.read().unwrap(), "unfinished comment");
    }

    #[test]
    fn landing_keeps_tab_changes_made_after_the_worker_read_them() {
        let state = WindowState::new(LiveId(1), None);
        state.with(|d| {
            d.repo = "/repo".into();
            open_file(
                d,
                "closed.rs",
                (None, buffer("/repo/closed.rs", "closed\n")),
                &[],
            );
        });
        let request = state.next_load();
        let next = state.read(Clone::clone);
        state.with(|d| {
            d.files_open.clear();
            open_file(
                d,
                "opened.rs",
                (None, buffer("/repo/opened.rs", "opened\n")),
                &[],
            );
        });
        assert!(state.land(request, next));
        state.read(|d| {
            assert_eq!(d.files_open.len(), 1);
            assert_eq!(d.files_open[0].path, "opened.rs");
            assert_eq!(d.blobs[d.files_open[0].head as usize].text, "opened\n");
        });
    }

    #[test]
    fn a_dirty_buffer_missing_from_the_new_diff_stays_open() {
        let state = WindowState::new(LiveId(1), None);
        state.with(|d| {
            d.repo = "/repo".into();
            let mut blob = buffer("/repo/a.rs", "before\n");
            blob.edit(0..0, "typed\n");
            d.blobs.push(blob);
        });
        let request = state.next_load();
        assert!(state.land(
            request,
            ReviewDoc {
                repo: "/repo".into(),
                ..Default::default()
            }
        ));
        state.read(|d| {
            assert_eq!(d.files_open.len(), 1);
            assert_eq!(d.files_open[0].path, "a.rs");
            let blob = &d.blobs[d.files_open[0].head as usize];
            assert_eq!(blob.text, "typed\nbefore\n");
            assert!(blob.dirty());
        });
    }

    #[test]
    fn a_failed_reload_keeps_edits_made_while_loading() {
        let state = WindowState::new(LiveId(1), None);
        state.with(|d| {
            d.git_dir = Some("/repo/.git".into());
            d.repo = "/repo".into();
            let text = "before\n";
            let mut blob = concats_diff::Blob::new(
                concats_sync::hash_object(text.as_bytes()),
                "rs".into(),
                text.into(),
            );
            blob.origin = Some("/repo/a.rs".into());
            d.blobs.push(blob);
            d.loading = true;
        });
        let request = state.next_load();
        state.with(|d| d.blobs[0].edit(0..0, "typed\n"));
        *state.compose_draft.write().unwrap() = "unfinished comment".into();
        assert!(state.land(
            request,
            ReviewDoc {
                error: Some("index temporarily unavailable".into()),
                ..Default::default()
            }
        ));
        state.read(|d| {
            assert_eq!(d.repo, "/repo");
            assert_eq!(d.blobs[0].text, "typed\nbefore\n");
            assert!(d.blobs[0].dirty());
            assert_eq!(d.error.as_deref(), Some("index temporarily unavailable"));
            assert!(!d.loading);
        });
        assert_eq!(*state.compose_draft.read().unwrap(), "unfinished comment");
    }

    #[test]
    fn a_superseded_load_cannot_replace_the_current_document() {
        let state = WindowState::new(LiveId(1), None);
        let old = state.next_load();
        let current = state.next_load();
        assert!(state.land(
            current,
            ReviewDoc {
                repo: "current".into(),
                ..Default::default()
            }
        ));
        assert!(!state.land(
            old,
            ReviewDoc {
                repo: "stale".into(),
                ..Default::default()
            }
        ));
        assert!(!state.land(
            old,
            ReviewDoc {
                error: Some("stale error".into()),
                ..Default::default()
            }
        ));
        state.read(|d| {
            assert_eq!(d.repo, "current");
            assert!(d.error.is_none());
        });
    }

    #[test]
    fn typing_a_comment_does_not_clone_a_shared_review_document() {
        let state = WindowState::new(LiveId(1), None);
        let drawing = state.snapshot();
        state
            .compose_draft
            .write()
            .unwrap()
            .push_str("unfinished comment");
        assert!(Arc::ptr_eq(&drawing, &state.snapshot()));
        assert_eq!(*state.compose_draft.read().unwrap(), "unfinished comment");
    }
}
