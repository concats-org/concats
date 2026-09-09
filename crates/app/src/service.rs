//! The app worker owns review stores and retained repositories, and performs
//! disk operations for comments, buffers, staging, layouts, and recents.
//!
//! The UI sends commands and reads published review state through shared `Arc`
//! snapshots. Seen ticks are applied optimistically by [`toggle_seen`] so the
//! control can update before the worker persists the change.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use concats_highlight::Highlighter;
use concats_review::store::{self, Anchor, Comment, LineKey, Store};
use concats_syntax::LineSpans;
use gix::ObjectId;
use makepad_service::{Service, Shared, Worker, notify};
use makepad_widgets::{DockItem, LiveId};

/// Disk work requested by the UI, performed in order on the app worker.
pub(crate) enum ReviewCmd {
    CacheBuffers(Arc<crate::window::WindowState>),
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
        window: LiveId,
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
        window: LiveId,
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
    Status {
        window: LiveId,
        message: String,
    },
    FileSaved {
        window: LiveId,
        path: PathBuf,
        oid: ObjectId,
        version: concats_sync::Version,
    },
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
        let Some(current) = ({
            (doc.generation == generation)
                .then(|| doc.blobs.get(blob as usize))
                .flatten()
                .filter(|blob| blob.edit_rev == rev)
        }) else {
            return;
        };
        let spans = self.highlighter.compute(&current.ext, &current.text);
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
        Worker::spawn(AppService {
            stores: HashMap::new(),
            worktrees: HashMap::new(),
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

struct AppService {
    /// One store per repo, opened once. The UI never sees these.
    stores: HashMap<PathBuf, Store>,
    worktrees: HashMap<PathBuf, Worktree>,
    comments_rev: u64,
}

struct Worktree {
    repo: gix::Repository,
    fingerprint: Option<u64>,
    next_probe: Instant,
    interval: Duration,
}

fn probe_worktree(
    worktree: &mut Worktree,
    now: Instant,
) -> Result<Option<u64>, concats_diff::Error> {
    if now < worktree.next_probe {
        return Ok(worktree.fingerprint);
    }
    let fingerprint = concats_diff::stage::worktree_fingerprint(&worktree.repo);
    worktree.interval = if fingerprint
        .as_ref()
        .is_ok_and(|fp| Some(*fp) != worktree.fingerprint)
    {
        Duration::from_secs(1)
    } else {
        (worktree.interval * 2).min(Duration::from_secs(16))
    };
    worktree.next_probe = now + worktree.interval;
    worktree.fingerprint = Some(fingerprint?);
    Ok(worktree.fingerprint)
}

impl AppService {
    fn invalidate_worktree(&mut self, workdir: &Path) {
        if let Some(worktree) = self.worktrees.get_mut(workdir) {
            worktree.next_probe = Instant::now();
            worktree.interval = Duration::from_secs(1);
        }
    }

    fn store(&mut self, git_dir: &Path) -> &mut Store {
        self.stores
            .entry(git_dir.to_path_buf())
            .or_insert_with(|| Store::open(git_dir))
    }

    fn publish_comments_changed(&mut self, git_dir: &Path) {
        self.comments_rev += 1;
        self.publish(git_dir);
    }

    /// Publish the repo's state and wake the UI.
    fn publish(&mut self, git_dir: &Path) {
        let rev = self.comments_rev;
        let output = review_state(Some(git_dir));
        let published = output.load();
        let st = self.store(git_dir);
        // NOTE: unchanged seen state must not invalidate every list's card cache.
        let seen = if *published.seen == st.seen {
            published.seen.clone()
        } else {
            Arc::new(st.seen.clone())
        };
        let comments = st.comments.clone();
        let commented = comments
            .iter()
            .map(|comment| comment.anchor)
            .flat_map(|anchor| (anchor.start..=anchor.end).map(move |line| (anchor.blob, line)))
            .collect();
        let state = ReviewState {
            git_dir: Some(git_dir.to_path_buf()),
            seen,
            comments: Arc::new(comments),
            commented: Arc::new(commented),
            comments_rev: rev,
        };
        output.publish(state);
        notify(ReviewUpdate::State);
    }
}

impl Service for AppService {
    type Cmd = ReviewCmd;

    #[expect(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "This is the single ordered I/O dispatch boundary for review commands."
    )]
    fn handle(&mut self, cmd: ReviewCmd) {
        match cmd {
            ReviewCmd::CacheBuffers(state) => {
                let doc = state.snapshot();
                if let Some(git_dir) = &doc.git_dir {
                    let store = self.store(git_dir);
                    for blob in doc.blobs.iter().filter(|blob| blob.doc.is_some()) {
                        if let (Some(origin), Some(saved)) =
                            (blob.origin.as_deref(), blob.saved_state())
                        {
                            store.save_buffer(origin, &saved);
                        }
                    }
                }
            }
            ReviewCmd::Open(git_dir) => {
                self.publish_comments_changed(&git_dir);
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
                self.publish_comments_changed(&git_dir);
            }

            ReviewCmd::ReplyComment {
                git_dir,
                parent,
                body,
            } => {
                let author = store::git_user_name(&git_dir);
                self.store(&git_dir)
                    .reply_comment(parent, body, author, store::now(), None);
                self.publish_comments_changed(&git_dir);
            }
            ReviewCmd::DeleteComment { git_dir, id } => {
                self.store(&git_dir).delete_comment(id);
                self.publish_comments_changed(&git_dir);
            }
            ReviewCmd::SaveFile {
                window,
                git_dir,
                plan,
            } => {
                let save = || {
                    let relative = plan
                        .path
                        .strip_prefix(&plan.root)
                        .map_err(|_| concats_diff::Error::UnsafeWorktreePath(plan.path.clone()))?;
                    let path =
                        concats_diff::load::worktree_file(&plan.root, &relative.to_string_lossy())?;
                    std::fs::write(&path, &plan.text)
                        .map_err(|source| concats_diff::Error::Io { path, source })
                };
                if let Err(error) = save() {
                    notify(ReviewUpdate::Status {
                        window,
                        message: format!("cannot save {}: {error}", plan.path.display()),
                    });
                    return;
                }
                self.invalidate_worktree(&plan.root);
                // Only after the bytes landed: an anchor moved to a hash no
                // file has would be worse than one left behind.
                if self.store(&git_dir).rehome(plan.old, plan.new, &plan.lines) {
                    self.comments_rev += 1;
                }
                self.publish(&git_dir);
                notify(ReviewUpdate::Status {
                    window,
                    message: format!(
                        "saved {}",
                        plan.path.file_name().map_or_else(
                            || plan.path.display().to_string(),
                            |n| n.to_string_lossy().into_owned()
                        )
                    ),
                });
                notify(ReviewUpdate::FileSaved {
                    window,
                    path: plan.path,
                    oid: plan.new,
                    version: plan.version,
                });
            }
            ReviewCmd::Rehome {
                git_dir,
                old,
                new,
                lines,
            } => {
                if self.store(&git_dir).rehome(old, new, &lines) {
                    self.publish_comments_changed(&git_dir);
                }
            }
            ReviewCmd::HoldComments { git_dir, cursors } => {
                self.store(&git_dir).set_cursors(&cursors);
                // NOTE: The rows were placed before these cursors were minted.
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
                    self.publish_comments_changed(&git_dir);
                }
                if let Some(g) = guide {
                    match store::latest_guide(&git_dir, &g.merge_base, &g.head) {
                        Some(rec) if g.applied_at != Some(rec.created_at) => {
                            notify(ReviewUpdate::GuideReady { window });
                        }
                        // Something was submitted, but not for this range: say
                        // so, don't switch the diff under the reviewer.
                        None if !store::guides(&git_dir).is_empty() => {
                            notify(ReviewUpdate::Status { window, message:
                                "a guide was submitted for a different range — open it via the diff picker".into(),
                            });
                        }
                        Some(_) | None => {}
                    }
                }
            }
            ReviewCmd::WorktreeProbe {
                window,
                workdir,
                last,
            } => {
                let fingerprint = (|| {
                    let now = Instant::now();
                    let worktree = match self.worktrees.entry(workdir.clone()) {
                        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(Worktree {
                                repo: concats_diff::load::open_repo(&workdir)?,
                                fingerprint: None,
                                next_probe: now,
                                interval: Duration::from_secs(1),
                            })
                        }
                    };
                    probe_worktree(worktree, now)
                })();
                match fingerprint {
                    Ok(Some(fp)) if fp != last => {
                        notify(ReviewUpdate::WorktreeChanged { window, fp });
                    }
                    Ok(_) => {}
                    Err(error) => notify(ReviewUpdate::Status {
                        window,
                        message: format!("cannot check worktree {}: {error}", workdir.display()),
                    }),
                }
            }
            ReviewCmd::StageSeen {
                window,
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
                            write!(msg, "  ·  skipped: {}", rep.skipped.join("; "))
                                .expect("writing to a String cannot fail");
                        }
                        msg
                    }
                    Err(e) => format!("stage failed: {e}"),
                };
                self.invalidate_worktree(&workdir);
                notify(ReviewUpdate::Status {
                    window,
                    message: status,
                });
            }
            ReviewCmd::SaveLayout {
                git_dir,
                dock_items,
                restores,
            } => {
                if let Err(error) = crate::dock::save_layout(&git_dir, dock_items, restores) {
                    // NOTE: keep the last layout usable without interrupting the review.
                    eprintln!("cannot save dock layout in {}: {error}", git_dir.display());
                }
            }
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
    fn service() -> (tempfile::TempDir, AppService, Shared<ReviewState>) {
        let tmp = tempfile::tempdir().unwrap();
        let out = review_state(Some(tmp.path()));
        let svc = AppService {
            stores: HashMap::new(),
            worktrees: HashMap::new(),
            comments_rev: 0,
        };
        (tmp, svc, out)
    }

    #[test]
    fn idle_worktrees_back_off_and_changed_worktrees_resume_fast_probes() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut worktree = Worktree {
            repo: gix::init(tmp.path()).unwrap(),
            fingerprint: None,
            next_probe: now,
            interval: Duration::from_secs(1),
        };
        let original = probe_worktree(&mut worktree, now).unwrap();
        assert!(original.is_some());
        for seconds in [2, 4, 8, 16, 16] {
            let due = worktree.next_probe;
            assert_eq!(probe_worktree(&mut worktree, due).unwrap(), original);
            assert_eq!(worktree.next_probe, due + Duration::from_secs(seconds));
        }

        std::fs::write(tmp.path().join("new.txt"), "new file\n").unwrap();
        let due = worktree.next_probe;
        assert_eq!(
            probe_worktree(
                &mut worktree,
                due.checked_sub(Duration::from_secs(1)).unwrap()
            )
            .unwrap(),
            original
        );
        let changed = probe_worktree(&mut worktree, due).unwrap();
        assert_ne!(changed, original);
        assert_eq!(worktree.next_probe, due + Duration::from_secs(1));
        assert_eq!(probe_worktree(&mut worktree, due).unwrap(), changed);
    }

    #[test]
    fn app_writes_make_the_next_worktree_probe_due() {
        let (tmp, mut svc, _) = service();
        let repo = gix::init(tmp.path()).unwrap();
        let now = Instant::now();
        svc.worktrees.insert(
            tmp.path().to_path_buf(),
            Worktree {
                repo,
                fingerprint: None,
                next_probe: now + Duration::from_secs(16),
                interval: Duration::from_secs(16),
            },
        );
        svc.invalidate_worktree(tmp.path());
        let worktree = svc.worktrees.get_mut(tmp.path()).unwrap();
        assert!(worktree.next_probe <= Instant::now());
        assert_eq!(worktree.interval, Duration::from_secs(1));
        assert!(probe_worktree(worktree, Instant::now()).unwrap().is_some());
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
    fn unchanged_seen_state_keeps_its_published_snapshot() {
        let (tmp, mut svc, out) = service();
        let git_dir = tmp.path().to_path_buf();
        svc.handle(ReviewCmd::ToggleSeen {
            git_dir: git_dir.clone(),
            keys: vec![(oid(1), 0)],
        });
        let seen = out.load().seen.clone();

        svc.handle(ReviewCmd::AddComment {
            git_dir: git_dir.clone(),
            path: "a.rs".into(),
            anchor: Anchor {
                blob: oid(1),
                start: 0,
                end: 0,
            },
            body: "keep this seen".into(),
            cursors: None,
        });
        assert!(Arc::ptr_eq(&seen, &out.load().seen));

        svc.handle(ReviewCmd::ToggleSeen {
            git_dir,
            keys: vec![(oid(1), 0)],
        });
        assert!(!Arc::ptr_eq(&seen, &out.load().seen));
        assert!(out.load().seen.is_empty());
    }

    #[test]
    fn persisting_comment_cursors_does_not_invalidate_the_rendered_rows() {
        let (tmp, mut svc, out) = service();
        let git_dir = tmp.path().to_path_buf();
        svc.handle(ReviewCmd::AddComment {
            git_dir: git_dir.clone(),
            path: "a.rs".into(),
            anchor: Anchor {
                blob: oid(2),
                start: 0,
                end: 0,
            },
            body: "keep this line".into(),
            cursors: None,
        });
        let before = out.load();
        let mut blob = concats_diff::Blob::new(oid(2), "rs".into(), "line\n".into());
        let cursors = blob.cursors_at(0, 0).unwrap();
        svc.handle(ReviewCmd::HoldComments {
            git_dir: git_dir.clone(),
            cursors: vec![(before.comments[0].id, cursors.clone())],
        });
        let after = out.load();
        assert_eq!(after.comments_rev, before.comments_rev);
        assert_eq!(after.comments[0].cursors, Some(cursors.clone()));
        assert_eq!(after.commented, before.commented);
        assert_eq!(Store::open(&git_dir).comments[0].cursors, Some(cursors));
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
