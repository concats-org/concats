//! The review service: the one owner of the review store.
//!
//! The store's I/O used to run on the thread that draws — a SQLite commit per
//! tick box, a `git config` read per comment, a git index write behind the
//! Share menu — and the window stuttered. Now the UI sends a [`ReviewCmd`] and
//! moves on; the service applies it on its own thread and publishes a fresh
//! [`ReviewState`], which the UI reads as an `Arc` with no lock on the draw
//! path.
//!
//! What you must see right away (a tick box flipping) the UI applies to the
//! published state itself, optimistically — see [`toggle_seen`] — and the
//! service's version overwrites it a millisecond later.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use concats_highlight::Highlighter;
use concats_review::store::{self, Anchor, Comment, LineKey, Store};
use concats_syntax::LineSpans;
use gix::ObjectId;
use makepad_service::{notify, Service, Shared, Worker};
use makepad_widgets::{DockItem, LiveId};

/// What the UI can ask of the store. Every variant is an effect the UI thread
/// must not perform itself.
pub(crate) enum ReviewCmd {
    /// Adopt a repo: open its store and publish what is already recorded.
    Open(PathBuf),
    ToggleSeen {
        git_dir: PathBuf,
        keys: Vec<LineKey>,
    },
    AddComment {
        git_dir: PathBuf,
        path: String,
        anchor: Anchor,
        body: String,
        /// Minted on the UI thread, where the buffer is, when the file is a
        /// worktree file: the comment's lines as a cursor pair in its document.
        cursors: Option<store::Cursors>,
    },
    /// Answer a comment. The reply takes its thread root's anchor, so unlike
    /// `AddComment` this needs no range and no loaded diff.
    ReplyComment {
        git_dir: PathBuf,
        parent: u64,
        body: String,
    },
    DeleteComment {
        git_dir: PathBuf,
        id: u64,
    },
    /// Write an edited buffer back to the file it came from, and carry
    /// everything anchored to its old content across to its new hash. Both
    /// halves belong to the one owner of the store, and both are I/O.
    SaveFile {
        git_dir: PathBuf,
        plan: crate::file_view::SavePlan,
    },
    /// A file changed on disk under an open buffer (an agent, the terminal, a
    /// checkout): carry the seen ticks of the lines that only moved from the
    /// old oid to the new one. `lines` comes from the document — see
    /// `Blob::line_moves`.
    Rehome {
        git_dir: PathBuf,
        old: ObjectId,
        new: ObjectId,
        lines: HashMap<u32, u32>,
    },
    /// The buffer took hold of comments that arrived without cursors — older
    /// ones — from their exact lines. Store what it minted, so the document
    /// carries them from now on, in the CLI too.
    HoldComments {
        git_dir: PathBuf,
        cursors: Vec<(u64, store::Cursors)>,
    },
    /// One poll tick: pick up what other processes committed — CLI comments
    /// and seen state in the store, and a `submit`ted guide on disk. Both are
    /// file/DB probes, which is why they are not on the UI thread.
    Poll {
        git_dir: PathBuf,
        /// The pane's guide key and the guide it already applied — absent
        /// while an explicit `--guide` overrides the store.
        guide: Option<GuideProbe>,
        /// The window that asked; echoed back so only it reloads.
        window: LiveId,
    },
    /// Is a WORKTREE review's diff still current? `last` is the fingerprint
    /// the pane has; a different one comes back as `WorktreeChanged`.
    WorktreeProbe {
        /// The window that asked; echoed back so only it reloads.
        window: LiveId,
        workdir: PathBuf,
        last: u64,
    },
    /// Share → "Stage seen hunks": `git add -p` driven by the seen ticks.
    StageSeen {
        git_dir: PathBuf,
        workdir: PathBuf,
        files: Vec<concats_diff::stage::StageFile>,
    },
    SaveLayout {
        git_dir: PathBuf,
        dock_items: HashMap<LiveId, DockItem>,
        /// (bottom, sidebar): the size each panel reopens at.
        restores: (f64, f64),
    },
    RecordRecent(String),
    LoadRecents,
}

/// What a poll tick should compare a stored guide against.
pub(crate) struct GuideProbe {
    pub merge_base: ObjectId,
    pub head: ObjectId,
    pub applied_at: Option<u64>,
}

/// What the UI reads while drawing: the review state of the open repo.
/// Immutable — the service publishes a new one rather than mutating this.
#[derive(Default, Clone)]
pub(crate) struct ReviewState {
    pub git_dir: Option<PathBuf>,
    pub seen: Arc<HashSet<LineKey>>,
    pub comments: Arc<Vec<Comment>>,
    /// Lines covered by any comment, precomputed when the service publishes so
    /// a visible code row does one hash lookup rather than scanning comments.
    pub commented: Arc<HashSet<LineKey>>,
    /// Bumped only when the comment list moves. Splicing comments into the
    /// document walks every row of every stream, so a tick box — which
    /// publishes just as often — must not trigger one.
    pub comments_rev: u64,
}

impl ReviewState {
    /// (all seen, any seen) for a set of line keys — the tick box's state, and
    /// the "partially viewed" hint next to it. One shared answer with the
    /// store, so the optimistic overlay and the authoritative write agree.
    pub fn state(&self, keys: &[LineKey]) -> (bool, bool) {
        store::seen_state(keys, &self.seen)
    }
}

/// Posted whenever the service publishes. The app reads the new snapshot in
/// `handle_actions` — the action says *that* something changed, never what to
/// draw with it.
#[derive(Clone, Debug)]
pub(crate) enum ReviewUpdate {
    /// Seen state and/or comments moved.
    State,
    /// A newer guide exists for this range: the pane should reload, which is
    /// the one path that applies a guide.
    GuideReady {
        window: LiveId,
    },
    /// The worktree moved; the payload is the new fingerprint.
    WorktreeChanged {
        window: LiveId,
        fp: u64,
    },
    /// A staging run finished, or a guide landed for another range — either
    /// way, one line for the status bar.
    Status(String),
    /// A blob finished highlighting off the UI thread. `rev` is the blob's
    /// edit counter as it was when the work started — spans computed against
    /// text that has since been typed over are dropped, not drawn.
    HighlightReady {
        window: LiveId,
        generation: u64,
        blob: u32,
        rev: u64,
        spans: LineSpans,
    },
    Recents(Vec<String>),
}

pub(crate) enum HighlightCmd {
    Request {
        /// The window whose document this blob belongs to; the reply carries
        /// it back so the spans land in the document they were computed from.
        window: LiveId,
        /// The document as it stood when the request went out. Carried rather
        /// than looked up: the worker has no window to look one up for, and an
        /// `Arc` clone costs nothing.
        doc: Arc<crate::review_doc::ReviewDoc>,
        generation: u64,
        blob: u32,
        rev: u64,
    },
}

pub(crate) fn highlight() -> &'static Worker<HighlightCmd> {
    static W: OnceLock<Worker<HighlightCmd>> = OnceLock::new();
    W.get_or_init(|| {
        Worker::spawn(HighlightService {
            highlighter: Highlighter::new(),
            generation: 0,
            completed: HashSet::new(),
        })
    })
}

struct HighlightService {
    highlighter: Highlighter,
    generation: u64,
    /// Which `(blob, rev)` pairs are done. Keyed by rev as well as blob because
    /// an edit has to be able to ask for the same blob again, and an edit does
    /// not bump `generation`: that marks a landed load, and bumping it
    /// mid-typing would reconcile the dock's tabs.
    completed: HashSet<(u32, u64)>,
}

impl Service for HighlightService {
    type Cmd = HighlightCmd;

    fn handle(&mut self, cmd: HighlightCmd) {
        let HighlightCmd::Request {
            window,
            doc,
            generation,
            blob,
            rev,
        } = cmd;
        if self.generation != generation {
            self.generation = generation;
            self.completed.clear();
        }
        if !self.completed.insert((blob, rev)) {
            return;
        }
        let Some((ext, text)) = ({
            (doc.generation == generation)
                .then(|| doc.blobs.get(blob as usize))
                .flatten()
                .filter(|blob| blob.edit_rev == rev)
                .map(|blob| (blob.ext.clone(), blob.text.clone()))
        }) else {
            return;
        };
        let spans = self.highlighter.compute(&ext, &text);
        notify(ReviewUpdate::HighlightReady {
            window,
            generation,
            blob,
            rev,
            spans,
        });
    }
}

/// The published snapshot for one repo. `load()` is one `Arc` clone — safe to
/// call from a draw.
///
/// Keyed by repo rather than by window because that is what it is: the seen
/// set and comments of a store. Two windows on one repo share this on purpose
/// — a tick in one shows in the other. `None` (nothing loaded yet) gets an
/// empty state of its own.
pub(crate) fn review_state(git_dir: Option<&Path>) -> Shared<ReviewState> {
    static S: OnceLock<Mutex<HashMap<PathBuf, Shared<ReviewState>>>> = OnceLock::new();
    let states = S.get_or_init(Mutex::default);
    let key = git_dir.map(Path::to_path_buf).unwrap_or_default();
    states.lock().unwrap().entry(key).or_default().clone()
}

/// The handle every UI-side mutation goes through.
pub(crate) fn review() -> &'static Worker<ReviewCmd> {
    static W: OnceLock<Worker<ReviewCmd>> = OnceLock::new();
    W.get_or_init(|| {
        Worker::spawn(ReviewService {
            stores: HashMap::new(),
            comments_rev: 0,
        })
    })
}

/// Flip a card's lines and show it immediately: the published state gets the
/// change now, the service confirms it (and the write reaches disk) next.
/// Returns the new state, so the caller can update anything derived from it.
pub(crate) fn toggle_seen(git_dir: &Path, keys: Vec<LineKey>) {
    let out = review_state(Some(git_dir));
    let (all, _) = out.load().state(&keys);
    out.update(|s| {
        let mut next = s.clone();
        let seen = Arc::make_mut(&mut next.seen);
        for k in &keys {
            if all {
                seen.remove(k);
            } else {
                seen.insert(*k);
            }
        }
        next
    });
    review().send(ReviewCmd::ToggleSeen {
        git_dir: git_dir.to_path_buf(),
        keys,
    });
}

struct ReviewService {
    /// One store per repo, opened once. The UI never sees these.
    stores: HashMap<PathBuf, Store>,
    comments_rev: u64,
}

impl ReviewService {
    fn store(&mut self, git_dir: &Path) -> &mut Store {
        self.stores
            .entry(git_dir.to_path_buf())
            .or_insert_with(|| Store::open(git_dir))
    }

    /// Publish the repo's state and wake the UI.
    fn publish(&mut self, git_dir: &Path) {
        let rev = self.comments_rev;
        let st = self.store(git_dir);
        let comments = st.comments.clone();
        let commented = comments
            .iter()
            .map(|comment| comment.anchor)
            .flat_map(|anchor| (anchor.start..=anchor.end).map(move |line| (anchor.blob, line)))
            .collect();
        let state = ReviewState {
            git_dir: Some(git_dir.to_path_buf()),
            seen: Arc::new(st.seen.clone()),
            comments: Arc::new(comments),
            commented: Arc::new(commented),
            comments_rev: rev,
        };
        review_state(Some(git_dir)).publish(state);
        notify(ReviewUpdate::State);
    }
}

impl Service for ReviewService {
    type Cmd = ReviewCmd;

    fn handle(&mut self, cmd: ReviewCmd) {
        match cmd {
            ReviewCmd::Open(git_dir) => {
                self.comments_rev += 1;
                self.publish(&git_dir);
            }
            ReviewCmd::ToggleSeen { git_dir, keys } => {
                self.store(&git_dir).toggle(&keys);
                self.publish(&git_dir);
            }
            ReviewCmd::AddComment {
                git_dir,
                path,
                anchor,
                body,
                cursors,
            } => {
                // The author read hits git's config files, so it belongs here
                // and not in the click handler.
                let author = store::git_user_name(&git_dir);
                self.store(&git_dir).add_comment(store::Comment {
                    id: 0,
                    path,
                    anchor,
                    body,
                    author,
                    created_at: store::now(),
                    parent: None,
                    external: None,
                    cursors,
                });
                self.comments_rev += 1;
                self.publish(&git_dir);
            }

            ReviewCmd::ReplyComment {
                git_dir,
                parent,
                body,
            } => {
                let author = store::git_user_name(&git_dir);
                self.store(&git_dir)
                    .reply_comment(parent, body, author, store::now(), None);
                self.comments_rev += 1;
                self.publish(&git_dir);
            }
            ReviewCmd::DeleteComment { git_dir, id } => {
                self.store(&git_dir).delete_comment(id);
                self.comments_rev += 1;
                self.publish(&git_dir);
            }
            ReviewCmd::SaveFile { git_dir, plan } => {
                if let Err(error) = std::fs::write(&plan.path, &plan.text) {
                    notify(ReviewUpdate::Status(format!(
                        "cannot save {}: {error}",
                        plan.path.display()
                    )));
                    return;
                }
                // Only after the bytes landed: an anchor moved to a hash no
                // file has would be worse than one left behind.
                if self.store(&git_dir).rehome(plan.old, plan.new, &plan.lines) {
                    self.comments_rev += 1;
                }
                self.publish(&git_dir);
                notify(ReviewUpdate::Status(format!(
                    "saved {}",
                    plan.path.file_name().map_or_else(
                        || plan.path.display().to_string(),
                        |n| n.to_string_lossy().into_owned()
                    )
                )));
            }
            ReviewCmd::Rehome {
                git_dir,
                old,
                new,
                lines,
            } => {
                if self.store(&git_dir).rehome(old, new, &lines) {
                    self.comments_rev += 1;
                    self.publish(&git_dir);
                }
            }
            ReviewCmd::HoldComments { git_dir, cursors } => {
                self.store(&git_dir).set_cursors(&cursors);
                self.comments_rev += 1;
                self.publish(&git_dir);
            }
            ReviewCmd::Poll {
                git_dir,
                guide,
                window,
            } => {
                let st = self.store(&git_dir);
                // Has another connection committed, and did that change
                // anything we hold?
                if st.external_change() && st.refresh() {
                    // Another writer's change could be either half; assume the
                    // comments moved (it is a once-a-second path at worst).
                    self.comments_rev += 1;
                    self.publish(&git_dir);
                }
                if let Some(g) = guide {
                    match store::latest_guide(&git_dir, &g.merge_base, &g.head) {
                        Some(rec) if g.applied_at != Some(rec.created_at) => {
                            notify(ReviewUpdate::GuideReady { window });
                        }
                        Some(_) => {}
                        // Something was submitted, but not for this range: say
                        // so, don't switch the diff under the reviewer.
                        None if !store::guides(&git_dir).is_empty() => {
                            notify(ReviewUpdate::Status(
                                "a guide was submitted for a different range — open it via the diff picker".into(),
                            ));
                        }
                        None => {}
                    }
                }
            }
            ReviewCmd::WorktreeProbe {
                window,
                workdir,
                last,
            } => {
                let fp = concats_diff::stage::worktree_fingerprint(&workdir);
                if fp != 0 && fp != last {
                    notify(ReviewUpdate::WorktreeChanged { window, fp });
                }
            }
            ReviewCmd::StageSeen {
                git_dir,
                workdir,
                files,
            } => {
                let st = self.store(&git_dir);
                let status = match concats_diff::stage::stage_seen(&workdir, &files, &st.seen) {
                    Ok(rep) => {
                        let mut msg = if rep.hunks == 0 {
                            "nothing staged — tick hunks as seen first".to_string()
                        } else {
                            format!("staged {} hunk(s) across {} file(s)", rep.hunks, rep.files)
                        };
                        if !rep.skipped.is_empty() {
                            msg.push_str(&format!("  ·  skipped: {}", rep.skipped.join("; ")));
                        }
                        msg
                    }
                    Err(e) => format!("stage failed: {e}"),
                };
                notify(ReviewUpdate::Status(status));
            }
            ReviewCmd::SaveLayout {
                git_dir,
                dock_items,
                restores,
            } => crate::dock::save_layout(&git_dir, dock_items, restores),
            ReviewCmd::RecordRecent(repo) => crate::recents::record_recent(&repo),
            ReviewCmd::LoadRecents => notify(ReviewUpdate::Recents(crate::recents::recents())),
        }
    }
}

#[cfg(test)]
mod tests {
    use concats_review::store::Anchor;
    use gix::ObjectId;

    use super::*;

    fn oid(n: u8) -> ObjectId {
        ObjectId::from_hex(format!("{n:040x}").as_bytes()).unwrap()
    }

    /// A service publishing into its own repo's slot — no `Cx`, no window, no
    /// frame to wait for. Each test gets a fresh tempdir, so each gets a slot
    /// of its own.
    fn service() -> (tempfile::TempDir, ReviewService, Shared<ReviewState>) {
        let tmp = tempfile::tempdir().unwrap();
        let out = review_state(Some(tmp.path()));
        let svc = ReviewService {
            stores: HashMap::new(),
            comments_rev: 0,
        };
        (tmp, svc, out)
    }

    #[test]
    fn toggling_publishes_the_new_seen_set() {
        let (tmp, mut svc, out) = service();
        let git_dir = tmp.path().to_path_buf();
        let keys = vec![(oid(1), 0), (oid(1), 1)];

        svc.handle(ReviewCmd::ToggleSeen {
            git_dir: git_dir.clone(),
            keys: keys.clone(),
        });
        assert_eq!(out.load().state(&keys), (true, true));

        svc.handle(ReviewCmd::ToggleSeen { git_dir, keys });
        assert_eq!(out.load().seen.len(), 0);
    }

    #[test]
    fn comments_round_trip_through_the_published_state() {
        let (tmp, mut svc, out) = service();
        let git_dir = tmp.path().to_path_buf();
        svc.handle(ReviewCmd::AddComment {
            git_dir: git_dir.clone(),
            path: "a.rs".into(),
            anchor: Anchor {
                blob: oid(2),
                start: 3,
                end: 5,
            },
            body: "why though?".into(),
            cursors: None,
        });
        let state = out.load();
        assert_eq!(state.comments.len(), 1);
        assert_eq!(state.comments[0].body, "why though?");
        assert_eq!(
            *state.commented,
            [(oid(2), 3), (oid(2), 4), (oid(2), 5)]
                .into_iter()
                .collect()
        );

        let id = state.comments[0].id;
        svc.handle(ReviewCmd::DeleteComment { git_dir, id });
        assert!(out.load().comments.is_empty());
        assert!(out.load().commented.is_empty());
    }

    #[test]
    fn poll_picks_up_another_process_write() {
        let (tmp, mut svc, out) = service();
        let git_dir = tmp.path().to_path_buf();
        svc.handle(ReviewCmd::Open(git_dir.clone()));
        assert!(out.load().comments.is_empty());

        // Another connection — a CLI `comments add`, or a second window.
        let mut other = Store::open(&git_dir);
        other.add_comment(store::Comment {
            id: 0,
            path: "a.rs".into(),
            anchor: Anchor {
                blob: oid(3),
                start: 1,
                end: 1,
            },
            body: "from the CLI".into(),
            author: Some("agent".into()),
            created_at: store::now(),
            parent: None,
            external: None,
            cursors: None,
        });

        svc.handle(ReviewCmd::Poll {
            git_dir,
            guide: None,
            window: LiveId(0),
        });
        assert_eq!(out.load().comments.len(), 1);
    }
}
