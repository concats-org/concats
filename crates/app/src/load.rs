//! Driving a load, and turning what it produced into the window's row streams.
//!
//! A load runs on a worker thread — a huge diff must never freeze the window —
//! and lands as a whole new [`ReviewDoc`]. Everything the reviewer put into the
//! old one carries over: unsaved typing, the documents comment threads are held
//! in, the buffers cached between runs, the open File tabs, the caret. That
//! carry-over is most of this file.

use concats_diff::{
    Blob, Row, Side,
    load::{INDEX_REV, Loaded, WORKTREE_REV},
};
use concats_review::{guide, sessions, store};
use concats_state::Target;

use crate::{
    file_view::{open_file, open_settings, read_file_sides},
    makepad_widgets::makepad_platform::thread::SignalToUI,
    review_doc::{
        Caret, Compose, Composing, ReviewDoc, Stream, changed_keys, splice_comments,
        splice_composer,
    },
    service::{ReviewCmd, review, review_state},
    window::WindowState,
};

/// Load `target` into `state`'s window on a worker thread.
pub(crate) fn spawn_load(
    state: &std::sync::Arc<WindowState>,
    target: Target,
    guide: Option<String>,
) {
    let request = state.next_load();
    state.with(|d| {
        d.loading = true;
        d.error = None;
    });
    let state = state.clone();
    // Wake the UI now so the header's ↻ starts spinning immediately; the land
    // bumps generation and signals again, which stops it.
    SignalToUI::set_ui_signal();
    std::thread::spawn(move || {
        let mut next = load_document(&target, guide);
        if next.error.is_none() && state.load_is_current(request) {
            let open = state.read(|d| {
                d.files_open
                    .iter()
                    .filter(|_| d.repo == next.repo)
                    .map(|f| (f.tab, f.path.clone()))
                    .collect()
            });
            reopen_files(&mut next, open, &target);
            restore_cached_buffers(&mut next);
        }
        if !state.land(request, next) {
            return;
        }
        // NOTE: The document lock also orders new load requests, so an older
        // worker cannot publish its range after a newer one has started.
        let document = state.write();
        if state.load_is_current(request)
            && document.error.is_none()
            && let Some(conn) = concats_state::open_app_db()
        {
            concats_state::publish_window_range(&conn, &state.key, &target);
        }
        drop(document);
        SignalToUI::set_ui_signal();
    });
}

fn load_document(target: &Target, guide: Option<String>) -> ReviewDoc {
    let repo = std::fs::canonicalize(&target.repo)
        .unwrap_or_else(|_| std::path::PathBuf::from(&target.repo))
        .to_string_lossy()
        .into_owned();
    let mut next = ReviewDoc {
        repo,
        base: target.base.clone(),
        head: target.head.clone(),
        guide_path: guide.clone(),
        ..Default::default()
    };
    match concats_diff::load::load(
        std::path::Path::new(&target.repo),
        &target.base,
        &target.head,
    ) {
        Ok(loaded) => {
            next.stats = loaded.stats.clone();
            next.merge_base_oid = loaded.merge_base;
            next.head_oid = loaded.head;
            next.workdir = loaded.workdir.clone();
            next.stage = loaded.stage.clone();
            next.refs = picker_refs(&loaded);
            let guide = guide_for(guide.as_deref(), &loaded);
            next.applied_guide_at = guide.as_ref().and_then(|(_, at)| *at);
            build_review(&mut next, loaded, target, guide.map(|(md, _)| md));
        }
        Err(error) => {
            next.error = Some(error.to_string());
            next.tab = Stream::Files;
            next.files_rows = vec![
                Row::Title {
                    text: "# Could not load that diff".into(),
                },
                Row::Prose {
                    md: format!("```\n{error}\n```"),
                },
            ];
        }
    }
    next
}

/// The diff picker's candidates: the worktree presets, then branches and tags,
/// then the last few commits. Short names — the loader resolves them like git;
/// a `...` row is a full range.
fn picker_refs(loaded: &Loaded) -> Vec<String> {
    let mut refs = vec![
        format!("{INDEX_REV}...{WORKTREE_REV}"),
        format!("HEAD...{WORKTREE_REV}"),
    ];
    let repo = gix::open(&loaded.git_dir).ok();
    for prefix in ["refs/heads/", "refs/tags/"] {
        let mut names: Vec<String> = repo
            .as_ref()
            .and_then(|repo| {
                let platform = repo.references().ok()?;
                let iter = platform.prefixed(prefix).ok()?;
                Some(
                    iter.filter_map(|r| {
                        let name = r.ok()?.name().as_bstr().to_string();
                        Some(name.trim_start_matches(prefix).to_string())
                    })
                    .collect(),
                )
            })
            .unwrap_or_default();
        names.sort();
        refs.extend(names);
    }
    // "The last N commits" — a ref list alone cannot say that.
    refs.extend(["HEAD~1", "HEAD~5", "HEAD~10", "HEAD~20"].map(String::from));
    refs
}

/// The guide the Guide tab renders, with the stamp of the submission it came
/// from: an explicit `--guide` file (local iteration, beats the store), else
/// the newest guide an agent submitted for exactly this resolved range. `None`
/// means no Guide tab. The stamp is what the poll compares to notice a newer
/// submission.
fn guide_for(path: Option<&str>, loaded: &Loaded) -> Option<(String, Option<u64>)> {
    match path {
        Some(path) => Some((std::fs::read_to_string(path).ok()?, None)),
        None => {
            let (base, head) = store::guide_key(loaded.merge_base, loaded.head);
            let guide = store::latest_guide(&loaded.git_dir, &base, &head)?;
            Some((guide.markdown, Some(guide.created_at)))
        }
    }
}

/// Re-open the File tabs the previous document had, over the fresh blob table.
/// A WORKTREE review reloads on every save; a file that blanked out once a
/// second while being read would be unusable.
fn reopen_files(d: &mut ReviewDoc, open: Vec<(u64, String)>, target: &Target) {
    for (tab, path) in open {
        if tab == crate::dock::settings_tab_id().0 {
            // Not a path in this repo — its content comes from the config
            // file, and `read_file_sides` would fail and empty the tab.
            open_settings(d);
            continue;
        }
        // NOTE: A removed file has no fresh side; landing can still preserve
        // the open buffer and any unsaved edits.
        match read_file_sides(&target.repo, (d.merge_base_oid, d.head_oid), &path) {
            Ok(sides) => {
                open_file(d, &path, sides, &[]);
            }
            Err(error) => eprintln!("concats: could not reload {path}: {error}"),
        }
    }
}

/// Carry the caret across the reload. It is held by blob index and a load
/// rebuilds the blob table, so it travels by the identity of the content under
/// it: the blob's oid, or the file it came from when the buffer was edited (an
/// edited buffer has a hash no load produces). Inside a document it travels as
/// a cursor, which carries it through an external write landing above it;
/// clamping a line number would slide it onto other code.
fn carry_caret(d: &ReviewDoc, prev: &ReviewDoc, caret: Caret) -> Option<Caret> {
    let (oid, origin, cursor) = (|| {
        let blob = prev.blobs.get(caret.blob as usize)?;
        let at = blob
            .line_starts
            .get(caret.line as usize)
            .map(|start| *start as usize + caret.byte as usize);
        let cursor = blob
            .doc
            .as_ref()
            .zip(at)
            .and_then(|(doc, at)| concats_sync::cursor_at(doc, at));
        Some((blob.oid, blob.origin.clone(), cursor))
    })()?;
    let path = prev.blob_paths.get(&caret.blob);
    let i = d
        .blobs
        .iter()
        .enumerate()
        .position(|(i, b)| match origin.as_ref() {
            Some(origin) => b.origin.as_ref() == Some(origin),
            None => b.oid == oid && d.blob_paths.get(&(i as u32)) == path,
        })?;
    let blob = &d.blobs[i];
    let at = cursor
        .as_ref()
        .zip(blob.doc.as_ref())
        .and_then(|(cursor, doc)| concats_sync::byte_of(doc, cursor));
    let (line, byte) = if let Some(at) = at {
        let line = blob.line_of(at);
        let column = at.saturating_sub(blob.line_starts[line] as usize);
        (line as u32, column as u32)
    } else {
        let line = caret.line.min(blob.line_count().saturating_sub(1) as u32);
        (
            line,
            caret.byte.min(blob.line_text(line as usize).len() as u32),
        )
    };
    Some(Caret {
        blob: i as u32,
        line,
        byte,
    })
}

fn carry_side(d: &ReviewDoc, prev: &ReviewDoc, side: Side) -> Option<Side> {
    let [Some(start), Some(end)] = [side.start, side.end].map(|line| {
        carry_caret(
            d,
            prev,
            Caret {
                blob: side.blob,
                line,
                byte: 0,
            },
        )
    }) else {
        return None;
    };
    Some(Side {
        blob: start.blob,
        start: start.line,
        end: end.line,
    })
}

pub(crate) fn carry_forward(d: &mut ReviewDoc, prev: &ReviewDoc) {
    if d.repo == prev.repo {
        carry_live_buffers(d, &prev.blobs);
        let fresh_files = std::mem::take(&mut d.files_open);
        for view in &prev.files_open {
            let (base, head) = match fresh_files.iter().find(|f| f.tab == view.tab) {
                Some(fresh) => (fresh.base, fresh.head),
                None => (
                    view.base.map(|i| {
                        crate::file_view::intern(d, &view.path, prev.blobs[i as usize].clone())
                    }),
                    crate::file_view::intern(d, &view.path, prev.blobs[view.head as usize].clone()),
                ),
            };
            crate::file_view::place(
                d,
                crate::review_doc::FileView {
                    tab: view.tab,
                    path: view.path.clone(),
                    rows: Vec::new(),
                    base,
                    head,
                    heading: view.heading.clone(),
                },
                &[],
            );
        }
        for blob in prev.blobs.iter().filter(|blob| blob.dirty()) {
            let Some(origin) = &blob.origin else {
                continue;
            };
            if d.blobs
                .iter()
                .any(|candidate| candidate.origin.as_ref() == Some(origin))
            {
                continue;
            }
            let Ok(path) = origin.strip_prefix(&d.repo) else {
                continue;
            };
            let path = path.to_string_lossy().into_owned();
            open_file(d, &path, (None, blob.clone()), &[]);
        }
        d.tab = prev.tab;
        d.caret = prev.caret.and_then(|at| carry_caret(d, prev, at));
        d.selection_anchor = prev
            .selection_anchor
            .and_then(|at| carry_caret(d, prev, at));
        d.compose = prev.compose.map(|c| match c {
            Composing::Reply(id) => Composing::Reply(id),
            Composing::Lines(c) => Composing::Lines(Compose {
                old: c.old.and_then(|side| carry_side(d, prev, side)),
                new: c.new.and_then(|side| carry_side(d, prev, side)),
            }),
        });
        d.compose_anchor = prev.compose_anchor;
        d.compose_focus = prev.compose_focus;
    }
    resplice_comments(d, &review_state(d.git_dir.as_deref()).load().comments);
    d.changed_keys = changed_keys(d);
    if d.repo == prev.repo
        && let Some(owner) = prev.composer_tab
    {
        let gesture = d.tab;
        d.tab = owner;
        splice_composer(d);
        d.tab = gesture;
    } else if d.compose.is_none() {
        compose_from_env(d);
    }
}

/// Dev affordance, pairs with CONCATS_APP_SHOT: `CONCATS_APP_COMPOSE=path:start:end`
/// (0-based) pre-opens the composer on those lines, so the comment dialog can
/// be screenshotted without a pointer.
fn compose_from_env(d: &mut ReviewDoc) {
    let Ok(spec) = crate::dev_hooks::var("CONCATS_APP_COMPOSE") else {
        return;
    };
    let mut it = spec.rsplitn(3, ':');
    let e = it.next().and_then(|v| v.parse::<u32>().ok());
    let s = it.next().and_then(|v| v.parse::<u32>().ok());
    let (Some(path), Some(s), Some(e)) = (it.next(), s, e) else {
        return;
    };
    // One path can be interned several times (file diff, session diffs, commit
    // diffs). Anchor on the copy the active stream renders at the requested
    // line, or `splice_composer` finds no row and drops the compose.
    let candidates: Vec<u32> = d
        .blob_paths
        .iter()
        .filter(|(_, p)| p.as_str() == path)
        .map(|(b, _)| *b)
        .collect();
    let blob = d.active().iter().find_map(|r| match r {
        Row::Code { blob: b, line, .. } if *line == s.max(e) && candidates.contains(b) => Some(*b),
        _ => None,
    });
    let Some(b) = blob else {
        return;
    };
    d.compose = Some(Composing::Lines(Compose {
        old: None,
        new: Some(Side {
            blob: b,
            start: s.min(e),
            end: s.max(e),
        }),
    }));
    d.compose_anchor = 0;
    splice_composer(d);
    d.compose_focus = true;
}

/// Assemble the review document: every tab's stream over one blob table. The
/// Guide tab is the agent's guide when one exists for the range (`guide_md`);
/// without one there is no Guide tab.
pub(crate) fn build_review(
    d: &mut ReviewDoc,
    mut loaded: Loaded,
    target: &Target,
    guide_md: Option<String>,
) {
    let s = loaded.stats.clone();
    d.git_dir = Some(loaded.git_dir.clone());

    d.guide_rows.push(Row::Title {
        text: format!("# `{}...{}`", target.base, target.head),
    });
    let against = match (loaded.merge_base, loaded.head) {
        (Some(mb), Some(_)) => format!("merge-base `{}`", &mb.to_string()[..10]),
        (Some(mb), None) => format!("worktree vs `{}`", &mb.to_string()[..10]),
        (None, _) => "worktree vs the index — the unstaged changes".to_string(),
    };
    d.guide_rows.push(Row::Prose {
        md: format!(
            "`{}` · {against} · **{} files**, +{}/−{} · loaded in **{:.0} ms**",
            target.repo, s.files, s.adds, s.dels, s.total_ms,
        ),
    });
    // The recorded agent sessions next to the diff: the Sessions and Commits
    // tabs. Costs one refs listing when there are none.
    let mut mined = sessions::mine(std::path::Path::new(&target.repo), &mut loaded);
    d.has_guide = guide_md.is_some();

    // What the file browser lists, and which of those paths this range creates
    // — the rest of its status it reads off the File Diff stream, which covers
    // the range exactly once.
    d.tree = std::mem::take(&mut loaded.tree);
    d.added = loaded
        .files
        .iter()
        .filter(|f| f.is_new)
        .map(|f| f.path.clone())
        .collect();

    // blob -> path, for comment records and the composer's target label.
    d.blob_paths = std::mem::take(&mut mined.blob_paths);
    for f in &loaded.files {
        for h in &f.hunks {
            for r in &h.rows {
                if let Row::Code { blob, .. } = r {
                    d.blob_paths.entry(*blob).or_insert_with(|| f.path.clone());
                }
            }
        }
    }

    // The Files tab: the same FileChanges, plain, in path order.
    let mut order: Vec<_> = loaded.files.iter().collect();
    order.sort_by(|a, b| a.path.cmp(&b.path));
    for f in order {
        d.files_rows.push(Row::FileHeader {
            path: f.path.clone(),
            lang: f.lang,
            adds: f.adds,
            dels: f.dels,
            from: f.from.clone(),
            similarity: f.similarity,
        });
        d.files_rows.extend(f.default_rows());
    }

    // The Sessions tab renders this second stream over the same blobs. Empty
    // stream = no tab (the design hides what does not exist), but keep the
    // explainer rows in case the tab is ever forced visible.
    d.sessions_rows = std::mem::take(&mut mined.sessions);
    d.has_sessions = !d.sessions_rows.is_empty();
    if d.sessions_rows.is_empty() {
        d.sessions_rows.push(Row::Title {
            text: "# Sessions".into(),
        });
        d.sessions_rows.push(Row::Prose {
            md: "No agent sessions (`refs/agent/sessions/*`) link to this range. \
                 Sessions are only visible where they were recorded — `refs/agent/*` \
                 are not fetched by default refspecs."
                .into(),
        });
    }

    // The Commits tab: the range by commit, over the same blobs. Empty when
    // the range has fewer than two commits — no tab.
    d.commits_rows = std::mem::take(&mut mined.commits);
    d.has_commits = !d.commits_rows.is_empty();

    if let Some(md) = guide_md {
        let r = guide::render(&md, &loaded);
        d.guide_rows.push(Row::Prose {
            md: format!(
                "_Organized by the guide: {} of {} hunks placed{}._",
                r.hunks_placed,
                r.hunks_total,
                if r.unresolved.is_empty() {
                    String::new()
                } else {
                    format!(", {} reference(s) unresolved", r.unresolved.len())
                }
            ),
        });
        d.guide_rows.extend(r.rows);
    }

    d.blobs = loaded.blobs;

    // The default tab: the guide when one exists, otherwise the plain file
    // diff. A hidden tab is never left active.
    d.tab = if d.has_guide {
        Stream::Guide
    } else {
        Stream::Files
    };
}

/// Put the buffers that were already open back into a freshly loaded blob
/// table, with whatever is now on disk merged into them.
///
/// This has to happen before anything resolves a comment. A load rebuilds the
/// table from git; without this step every open buffer's document, and every
/// comment cursor and caret held in it, would be replaced by a fresh read,
/// and a conversation on a line the writer just edited would have nothing
/// left to hold it. That is how a comment on a heading disappears when an
/// agent rewords the heading.
///
/// Keyed by `origin`, the worktree path, because that is what identifies the
/// file; the oid names one revision of it and changes under us.
fn carry_live_buffers(d: &mut ReviewDoc, live: &[Blob]) -> bool {
    let mut carried = false;
    for prev in live.iter().filter(|blob| blob.doc.is_some()) {
        let Some(origin) = prev.origin.as_deref() else {
            continue;
        };
        let Some(at) = d
            .blobs
            .iter()
            .position(|b| b.origin.as_deref() == Some(origin))
        else {
            continue;
        };
        let fresh = &d.blobs[at];
        // NOTE: Cache restoration may have added an older unsaved draft.
        // Only the disk version belongs in the merge with the live buffer.
        let text = fresh.doc.as_ref().map_or_else(
            || fresh.text.clone(),
            |doc| {
                concats_sync::text_at(doc, &fresh.disk).expect("buffer contains its disk version")
            },
        );
        let oid = fresh.oid;
        let mut buffer = prev.clone();
        let (was_oid, was_disk) = (buffer.oid, buffer.disk.clone());
        buffer.merge_disk(&text, oid);
        if buffer.oid != was_oid {
            rehome_seen(d.git_dir.as_deref(), &buffer, was_oid, &was_disk);
        }
        d.blobs[at] = buffer;
        carried = true;
    }
    carried
}

/// The file moved on disk under an open buffer: carry the seen ticks of the
/// lines that only moved from the old oid to the new one. Through the
/// document, so what counts as "the same line" is what actually happened to
/// it; an edited line drops its tick.
fn rehome_seen(
    git_dir: Option<&std::path::Path>,
    buffer: &Blob,
    was: gix::ObjectId,
    was_disk: &concats_sync::Version,
) {
    let Some(git_dir) = git_dir else {
        return;
    };
    review().send(ReviewCmd::Rehome {
        git_dir: git_dir.to_path_buf(),
        old: was,
        new: buffer.oid,
        lines: buffer.line_moves(was_disk, &buffer.disk),
    });
}

/// Splice the comments in, and give the store the cursors the buffers minted
/// for comments that arrived without any, so the document carries them from
/// now on — in the CLI too.
pub(crate) fn resplice_comments(d: &mut ReviewDoc, comments: &[concats_review::store::Comment]) {
    let minted = splice_comments(d, comments);
    hold_minted(d.git_dir.as_deref(), minted);
}

pub(crate) fn hold_minted(git_dir: Option<&std::path::Path>, cursors: Vec<(u64, store::Cursors)>) {
    if let (false, Some(git_dir)) = (cursors.is_empty(), git_dir) {
        review().send(ReviewCmd::HoldComments {
            git_dir: git_dir.to_path_buf(),
            cursors,
        });
    }
}

// NOTE: Restoring the CRDT history lets held comment cursors follow disk edits
// made while the app was closed. A fresh document cannot resolve those cursors.
fn restore_cached_buffers(d: &mut ReviewDoc) -> bool {
    let Some(git_dir) = d.git_dir.clone() else {
        return false;
    };
    let candidates: Vec<(usize, std::path::PathBuf)> = d
        .blobs
        .iter()
        .enumerate()
        .filter(|(_, b)| b.doc.is_none())
        .filter_map(|(i, b)| Some((i, b.origin.clone()?)))
        .collect();
    let mut restored = false;
    for (at, origin) in candidates {
        let Some(saved) = store::load_buffer(&git_dir, &origin) else {
            continue;
        };
        let oid = d.blobs[at].oid;
        if !d.blobs[at].restore_state(&saved, oid) {
            continue;
        }
        restored = true;
        // The file may have been written while the app was closed: the cached
        // disk version is where its ticks still sit.
        let buffer = &d.blobs[at];
        let cached = concats_sync::decode_version(&saved.disk)
            .filter(|version| *version != buffer.disk)
            .and_then(|version| {
                let text = concats_sync::text_at(buffer.doc.as_ref()?, &version)?;
                Some((concats_sync::hash_object(text.as_bytes()), version))
            });
        if let Some((was_oid, was_disk)) = cached {
            rehome_seen(Some(&git_dir), buffer, was_oid, &was_disk);
        }
    }
    restored
}

#[cfg(test)]
mod tests {
    use concats_diff::Blob;
    use gix::ObjectId;

    use super::*;

    fn oid(n: u8) -> ObjectId {
        ObjectId::from_hex(format!("{n:040x}").as_bytes()).expect("valid hex")
    }

    /// Something else writes the file while the buffer holds unsaved typing:
    /// both edits land.
    #[test]
    fn a_reload_merges_an_external_write_into_the_unsaved_buffer() {
        let origin = std::path::Path::new("/repo/src.rs");
        let mut buffer = Blob::new(oid(1), "rs".into(), "fn a() {}\nfn b() {}\n".into());
        buffer.origin = Some(origin.to_path_buf());
        buffer.edit(0..0, "// mine\n");
        assert!(buffer.dirty());

        // What a fresh load rebuilds after something else wrote the file.
        let mut d = loaded_with(origin, "fn a() {}\nfn NEW() {}\nfn b() {}\n", oid(2));
        assert!(carry_live_buffers(&mut d, std::slice::from_ref(&buffer)));
        let head = &d.blobs[0];
        assert!(head.text.contains("// mine"), "the typing survived");
        assert!(head.text.contains("fn NEW"), "the external write survived");
        assert!(
            head.dirty(),
            "still unsaved — the typed line is not on disk"
        );
    }

    #[test]
    fn a_live_buffer_does_not_treat_an_older_cached_draft_as_disk_content() {
        let origin = std::path::Path::new("/repo/a.rs");
        let initial = "first\nlast\n";
        let mut buffer = Blob::new(
            concats_sync::hash_object(initial.as_bytes()),
            "rs".into(),
            initial.into(),
        );
        buffer.origin = Some(origin.into());
        buffer.edit(0..0, "cached\n");
        let cached = buffer.saved_state().unwrap();
        buffer.edit(0..7, "latest\n");
        let disk = "first\nexternal\nlast\n";
        let disk_oid = concats_sync::hash_object(disk.as_bytes());
        let mut d = loaded_with(origin, disk, disk_oid);
        assert!(d.blobs[0].restore_state(&cached, disk_oid));
        assert!(d.blobs[0].text.contains("cached"));
        assert!(carry_live_buffers(&mut d, &[buffer]));
        let landed = &d.blobs[0];
        assert!(landed.text.contains("latest"));
        assert!(landed.text.contains("external"));
        assert!(!landed.text.contains("cached"));
        assert_eq!(
            concats_sync::text_at(landed.doc.as_ref().unwrap(), &landed.disk).unwrap(),
            disk
        );
        assert!(landed.dirty());
    }

    /// The case that detached conversations: nothing was unsaved, so the buffer
    /// used to be thrown away — and with it every cursor holding a thread.
    #[test]
    fn a_reload_keeps_a_clean_buffers_document_and_its_held_threads() {
        let origin = std::path::Path::new("/repo/README.md");
        let mut buffer = Blob::new(
            oid(1),
            "md".into(),
            "# Project\n## Getting Started\n".into(),
        );
        buffer.origin = Some(origin.to_path_buf());
        buffer.hold(7, 1, 1);
        assert!(!buffer.dirty(), "clean, and still worth carrying");

        // An agent rewords the very heading the thread is on.
        let mut d = loaded_with(origin, "# Project\n## Getting Started Quickly\n", oid(2));
        assert!(carry_live_buffers(&mut d, std::slice::from_ref(&buffer)));
        assert_eq!(
            d.blobs[0].held_line(7),
            Some(1),
            "the thread stayed on its reworded line"
        );
        assert_eq!(d.blobs[0].text, "# Project\n## Getting Started Quickly\n");
    }

    #[test]
    fn a_reload_takes_the_fresh_read_when_no_buffer_was_open() {
        let origin = std::path::Path::new("/repo/src.rs");
        let mut d = loaded_with(origin, "fn a() {}\n", oid(2));
        assert!(!carry_live_buffers(&mut d, &[]));
        assert_eq!(d.blobs[0].text, "fn a() {}\n");
        assert!(!d.blobs[0].dirty());
    }

    /// A worktree file whose buffer is already open, as a fresh load rebuilds it.
    fn loaded_with(origin: &std::path::Path, text: &str, at: ObjectId) -> ReviewDoc {
        let mut fresh = Blob::new(at, "rs".into(), text.into());
        fresh.origin = Some(origin.to_path_buf());
        ReviewDoc {
            blobs: vec![fresh],
            ..Default::default()
        }
    }
}
