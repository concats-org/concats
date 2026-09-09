//! UI automation used by visual tests, enabled with `--features dev-hooks`.
#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use super::*;

fn pointer_position(spec: &str) -> Option<(f64, f64)> {
    let (x, y) = spec.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

pub(crate) fn var(name: &str) -> Result<String, std::env::VarError> {
    #[cfg(feature = "dev-hooks")]
    {
        std::env::var(name)
    }
    #[cfg(not(feature = "dev-hooks"))]
    {
        let _ = name;
        Err(std::env::VarError::NotPresent)
    }
}

impl App {
    /// The dev/screenshot hooks
    /// (`CONCATS_APP_COMBO/SHARE/TAB/SCROLL/TERM/SETTINGS/SHOT`) fire once per
    /// run, after a load lands; `SHOT_EVERY` re-arms them on every load. Each
    /// opens the UI a pointer would, so a headless test can capture it. Nothing
    /// in production reads them.
    #[expect(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "Screenshot setup follows the same ordered phases as native startup."
    )]
    pub(super) fn apply_screenshot_hooks(&mut self, cx: &mut Cx) {
        let rearm = crate::dev_hooks::var("CONCATS_APP_SHOT_EVERY").is_ok_and(|v| !v.is_empty());
        if self.shot_done && !rearm {
            return;
        }
        // Companion to CONCATS_APP_SHOT: pre-open the diff picker
        // so the dropdown can be screenshotted without a pointer.
        if crate::dev_hooks::var("CONCATS_APP_COMBO").is_ok_and(|v| !v.is_empty()) {
            let pane = self.ui.widget(cx, ids!(pane_a));
            if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                p.combo_open(cx);
            }
        }
        // CONCATS_APP_PICK_REPO=1: pre-open the repo picker (recent repos +
        // "Open dir…") so its dropdown can be screenshotted without a pointer.
        if crate::dev_hooks::var("CONCATS_APP_PICK_REPO").is_ok_and(|v| !v.is_empty()) {
            let pane = self.ui.widget(cx, ids!(pane_a));
            if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                p.repo_open(cx);
            }
        }
        // CONCATS_APP_SHARE=1: likewise for the share dropdown —
        // with the same worktree-only stage row the click path shows.
        if crate::dev_hooks::var("CONCATS_APP_SHARE").is_ok_and(|v| !v.is_empty()) {
            let worktree = self
                .primary()
                .is_some_and(|w| w.state.read(|d| d.workdir.is_some()));
            self.ui
                .button(cx, ids!(share_stage))
                .set_visible(cx, worktree);
            self.ui.view(cx, ids!(share_panel)).set_visible(cx, true);
        }
        // CONCATS_APP_WINDOWS=N: open N-1 more windows on the same range, the
        // way ⌘N does. A menu item cannot be driven headlessly, so this is how
        // a multi-window run gets captured.
        if let Ok(n) = crate::dev_hooks::var("CONCATS_APP_WINDOWS") {
            let n = n.parse::<usize>().unwrap_or(1);
            while self.windows.len() < n {
                let before = self.windows.len();
                self.open_new_window(cx);
                if self.windows.len() == before {
                    break;
                }
            }
            // Opening took the keyboard, and every other hook drives the
            // window the app started on. Hand it back, so a capture types
            // where the rest of the hooks are looking.
            if let Some(primary) = self.windows.first().map(|w| w.state.clone()) {
                self.focus_window(&primary);
            }
        }
        // CONCATS_APP_FOLD=path[,path…]: shut those file cards, so the
        // folded state can be screenshotted without a pointer.
        if let (Ok(paths), Some(state)) = (
            crate::dev_hooks::var("CONCATS_APP_FOLD"),
            self.primary().map(|w| w.state.clone()),
        ) {
            state.with(|d| {
                d.folded = paths
                    .split(',')
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect();
            });
        }
        // CONCATS_APP_SCROLL=N: start the list at row N — lets a
        // test screenshot the sticky header without a pointer.
        if let Ok(n) = crate::dev_hooks::var("CONCATS_APP_SCROLL")
            && let Ok(n) = n.parse::<usize>()
        {
            let Some(t) = self.primary().map(|w| w.state.read(|d| d.tab)) else {
                return;
            };
            let pane = self.ui.widget(cx, ids!(pane_a));
            let content = pane.dock(cx, ids!(dock)).item(stream_tab_spec(t).id);
            let list = content.portal_list(cx, ids!(list));
            if let Some(mut pl) = list.borrow_mut() {
                pl.set_first_id_and_scroll(n, 0.0);
            }
        }
        // CONCATS_APP_TERM=1: pre-open the terminal panel (and its
        // shell) so it can be screenshotted without a pointer.
        if crate::dev_hooks::var("CONCATS_APP_TERM").is_ok_and(|v| !v.is_empty()) {
            let pane = self.ui.widget(cx, ids!(pane_a));
            if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                p.reveal_terminal(cx);
            }
        }
        // CONCATS_APP_SETTINGS=1: pre-open the settings editor so it
        // can be screenshotted without a pointer.
        if crate::dev_hooks::var("CONCATS_APP_SETTINGS").is_ok_and(|v| !v.is_empty()) {
            let pane = self.ui.widget(cx, ids!(pane_a));
            if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                p.open_settings_tab(cx);
            }
        }
        // CONCATS_APP_FILE=path[,path…]: open those files of the head tree,
        // as picking them in the browser would — one tab each.
        if let Ok(paths) = crate::dev_hooks::var("CONCATS_APP_FILE") {
            for path in paths.split(',').filter(|p| !p.is_empty()) {
                let pane = self.ui.widget(cx, ids!(pane_a));
                if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                    p.open_file_tab(cx, path);
                }
            }
        }
        // NOTE: Select after opening File tabs so file:<path> can name one.
        if let Ok(tab) = crate::dev_hooks::var("CONCATS_APP_TAB") {
            let t = match tab.as_str() {
                "guide" => Some(Stream::Guide),
                "sessions" => Some(Stream::Sessions),
                "commits" => Some(Stream::Commits),
                "comments" => Some(Stream::Comments),
                "files" => Some(Stream::Files),
                _ => tab.strip_prefix("file:").and_then(|path| {
                    self.primary()?.state.read(|d| {
                        d.files_open
                            .iter()
                            .find(|file| file.path == path)
                            .map(|file| Stream::File(file.tab))
                    })
                }),
            };
            if let Some(t) = t {
                let pane = self.ui.widget(cx, ids!(pane_a));
                pane.dock(cx, ids!(dock))
                    .select_tab(cx, stream_tab_spec(t).id);
                if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                    p.set_gesture_tab(cx, t);
                }
            }
        }
        if let Ok(path) = crate::dev_hooks::var("CONCATS_APP_SHOT")
            && !path.is_empty()
        {
            self.shot_done = true;
            cx.capture_next_frame_to_file(path.into());
        }
    }
    /// `CONCATS_APP_CLICK=x,y`: hover, press and release at those logical
    /// window coordinates, one tick after startup (the widgets need a laid-out
    /// frame to be hit), then re-arm the shot a tick later so the capture shows
    /// what the click did. Interactions are otherwise unreachable from a test:
    /// the app has no accessibility tree to drive, and a synthetic
    /// `MouseDown`/`MouseUp` pair goes through the same hit test a real pointer
    /// does.
    ///
    /// The dispatches are bracketed by `Cx::begin_mouse_down` /
    /// `end_mouse_move` / `end_mouse_up`, the same pointer bookkeeping the
    /// platform event loop does around a real mouse event. Without it the hits
    /// still fire but the digit is captured and never released, the hover never
    /// leaves, and the frame after the gesture is wrong.
    #[expect(
        clippy::too_many_lines,
        reason = "Pointer setup, move, press, and release must stay in event order."
    )]
    pub(super) fn click_hook(&mut self, cx: &mut Cx) {
        // The pointer position, when there is one. This sequence drives the type,
        // save, find, capture and exit hooks as well, so it runs with no click to
        // dispatch — a scenario that only opens a file still has to reach the
        // tick that captures and the one that leaves.
        let hover_only = var("CONCATS_APP_HOVER").is_ok();
        let at = crate::dev_hooks::var("CONCATS_APP_CLICK")
            .or_else(|_| var("CONCATS_APP_HOVER"))
            .ok()
            .and_then(|spec| pointer_position(&spec));
        // Nothing to hit until a load has landed and drawn.
        let Some(state) = self.primary().map(|w| w.state.clone()) else {
            return;
        };
        if state.read(|d| d.files_rows.is_empty()) {
            return;
        }
        // A requested capture only answers from a frame that PRESENTS, and a
        // draw is not a present — under timer pacing several of these ticks can
        // pass between two of them. Left to run, the before-frame and the
        // after-frame drain on the same present and come out byte-identical,
        // which reads as an interaction that never happened. So hold until the
        // file exists: that it does is the proof its frame presented.
        if let Some(path) = self.shot_pending.clone() {
            if !path.exists() {
                self.ui.redraw(cx);
                return;
            }
            self.shot_pending = None;
        }
        self.click_tick += 1;
        match self.click_tick {
            // `CONCATS_APP_SHOT_BEFORE=/path.png`: the frame as the load left
            // it, before any of the hooks below touch it.
            //
            // With both frames a test can ask two questions of an interaction:
            // did it change anything, and is the change right. Neither alone is
            // enough. An expected frame can't tell a state that renders from
            // one that never happened, and a difference can't tell a right
            // answer from a wrong one.
            1 => {
                if let Ok(path) = crate::dev_hooks::var("CONCATS_APP_SHOT_BEFORE")
                    && !path.is_empty()
                {
                    let path = PathBuf::from(path);
                    let _ = std::fs::remove_file(&path);
                    cx.capture_next_frame_to_file(path.clone());
                    self.shot_pending = Some(path);
                    // Guarantee the draw that writes it, and the tick after.
                    self.ui.redraw(cx);
                }
            }
            2 => {
                let at = at.or_else(|| {
                    if !var("CONCATS_APP_CLICK_SEEN").is_ok_and(|value| !value.is_empty()) {
                        return None;
                    }
                    let tab = state.read(|d| d.tab);
                    let area = self
                        .primary()?
                        .pane(cx)
                        .dock(cx, ids!(dock))
                        .item(stream_tab_spec(tab).id)
                        .check_box(cx, ids!(st_seen))
                        .area();
                    let rect = area.rect(cx);
                    (rect.size.x > 0.0 && rect.size.y > 0.0).then_some((
                        rect.pos.x + rect.size.x * 0.5,
                        rect.pos.y + rect.size.y * 0.5,
                    ))
                });
                let Some((x, y)) = at else {
                    return;
                };
                let abs = dvec2(x, y);
                let window_id = CxWindowPool::id_zero();
                // Move the pointer there before pressing, the way a real one
                // arrives. Without this no hover state was reachable from a
                // capture at all — press and release alone never raise
                // FingerHoverIn, so the gutter's + affordance could not be
                // screenshotted.
                let now = cx.seconds_since_app_start();
                self.ui.handle_event(
                    cx,
                    &Event::MouseMove(MouseMoveEvent {
                        abs,
                        window_id,
                        modifiers: KeyModifiers::default(),
                        handled: std::cell::Cell::new(Area::Empty),
                        lock_delta: DVec2::default(),
                        time: now,
                    }),
                    &mut Scope::empty(),
                );
                cx.end_mouse_move();
                if hover_only {
                    return;
                }
                let down = MouseDownEvent {
                    abs,
                    button: MouseButton::PRIMARY,
                    window_id,
                    modifiers: KeyModifiers::default(),
                    handled: std::cell::Cell::new(Area::Empty),
                    time: now,
                };
                cx.begin_mouse_down(&down);
                self.ui
                    .handle_event(cx, &Event::MouseDown(down), &mut Scope::empty());
                let abs = if let Some((x, y)) = var("CONCATS_APP_DRAG_TO")
                    .ok()
                    .and_then(|spec| pointer_position(&spec))
                {
                    let end = dvec2(x, y);
                    self.ui.handle_event(
                        cx,
                        &Event::MouseMove(MouseMoveEvent {
                            abs: end,
                            window_id,
                            modifiers: KeyModifiers::default(),
                            handled: std::cell::Cell::new(Area::Empty),
                            lock_delta: DVec2::default(),
                            time: now + 0.05,
                        }),
                        &mut Scope::empty(),
                    );
                    cx.end_mouse_move();
                    end
                } else {
                    abs
                };
                self.ui.handle_event(
                    cx,
                    &Event::MouseUp(MouseUpEvent {
                        abs,
                        button: MouseButton::PRIMARY,
                        window_id,
                        modifiers: KeyModifiers::default(),
                        time: now + 0.1,
                    }),
                    &mut Scope::empty(),
                );
                cx.end_mouse_up(MouseButton::PRIMARY);
            }
            3 => self.type_hook(cx, "CONCATS_APP_TYPE"),
            4 => {
                self.state_hook(cx, "state-before-refresh.json");
                self.save_hook(cx);
                if var("CONCATS_APP_FIND").is_ok_and(|query| !query.is_empty())
                    && var("CONCATS_APP_FIND_KEEP_FOCUS").is_err()
                {
                    let Some(window) = self.primary() else {
                        return;
                    };
                    let tab = window.state.read(|d| d.tab);
                    let content = window
                        .pane(cx)
                        .dock(cx, ids!(dock))
                        .item(stream_tab_spec(tab).id);
                    cx.set_key_focus(content.portal_list(cx, ids!(list)).area());
                }
                if var("CONCATS_APP_COMPOSER_REFRESH").is_ok_and(|value| !value.is_empty()) {
                    state.with(|d| {
                        let Some(review_doc::Composing::Lines(compose)) = d.compose else {
                            return;
                        };
                        let Some(side) = review_doc::comment_anchor(compose) else {
                            return;
                        };
                        let comment = concats_review::store::Comment {
                            id: 999,
                            path: review_doc::blob_label(d, side.blob),
                            anchor: concats_review::store::Anchor {
                                blob: d.blobs[side.blob as usize].oid,
                                start: side.start,
                                end: side.start,
                            },
                            body: "A comment arrived while typing".into(),
                            author: Some("Fixture".into()),
                            created_at: 0,
                            parent: None,
                            external: None,
                            cursors: None,
                        };
                        // NOTE: This fixture stays in memory; its cursors must
                        // not be published to the review worker.
                        drop(review_doc::splice_comments(d, &[comment]));
                    });
                    self.ui
                        .widget(cx, ids!(pane_a))
                        .dock(cx, ids!(dock))
                        .item(stream_tab_spec(state.read(|d| d.tab)).id)
                        .redraw(cx);
                }
            }
            5 => {
                self.find_hook(cx);
                self.type_hook(cx, "CONCATS_APP_TYPE_AFTER_REFRESH");
            }
            // Two captures, and the second is the one kept. A request is
            // answered by the next frame to PRESENT, and that frame can have
            // been encoded before the last hook's redraw was painted — which
            // is how a caret that was placed failed to appear roughly one run
            // in six. Landing the first proves the pipeline is flushed, so the
            // frame behind the second is drawn after everything settled.
            6 | 7 => {
                if self.click_tick == 6 {
                    self.type_hook(cx, "CONCATS_APP_FIND");
                }
                if let Ok(path) = crate::dev_hooks::var("CONCATS_APP_SHOT")
                    && !path.is_empty()
                {
                    let path = PathBuf::from(path);
                    let _ = std::fs::remove_file(&path);
                    cx.capture_next_frame_to_file(path.clone());
                    self.shot_pending = Some(path);
                }
                // Guarantee one more draw, so the tick that exits below is
                // reached: these ticks advance per DRAW, and once the capture is
                // requested nothing else is necessarily dirty.
                self.ui.redraw(cx);
            }
            // `CONCATS_APP_EXIT_AFTER_SHOT=1`: leave once the frame above has
            // been written — the hold at the top of this function is what
            // guarantees it has. A test then waits for the process to exit,
            // which is an exact signal. Waiting for the file to appear is not:
            // it catches the capture taken when the load landed, before any of
            // these ticks ran.
            8 => {
                self.state_hook(cx, "state.json");
                if crate::dev_hooks::var("CONCATS_APP_EXIT_AFTER_SHOT").is_ok_and(|v| !v.is_empty())
                {
                    std::process::exit(0);
                }
            }
            _ => {}
        }
    }

    fn state_hook(&self, cx: &mut Cx, name: &str) {
        let (Ok(path), Some(window)) = (var("CONCATS_APP_STATE"), self.primary()) else {
            return;
        };
        let state = &window.state;
        let owner = state.read(|d| d.composer_tab);
        let input = owner.and_then(|tab| {
            self.ui
                .widget(cx, ids!(pane_a))
                .dock(cx, ids!(dock))
                .item(stream_tab_spec(tab).id)
                .borrow::<widgets::ReviewList>()
                .and_then(|list| list.composer_input)
                .map(|uid| uid.0)
        });
        let active = window
            .pane(cx)
            .dock(cx, ids!(dock))
            .item(stream_tab_spec(state.read(|d| d.tab)).id);
        let query = active.text_input(cx, ids!(find_input)).text();
        let count = active.label(cx, ids!(find_count)).text();
        let titles = window
            .pane(cx)
            .dock(cx, ids!(dock))
            .clone_state()
            .map(|items| {
                items
                    .into_values()
                    .filter_map(|item| match item {
                        DockItem::Tab { name, .. } => Some(name),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let dock = window.pane(cx).dock(cx, ids!(dock));
        let browser = dock.item(id!(sidebar_tab));
        let file_query = browser.text_input(cx, ids!(find_input)).text();
        let file_count = browser.label(cx, ids!(find_count)).text();
        let terminal_tab = dock.item(id!(terminal_tab));
        let terminal_rect = terminal_tab.area().rect(cx);
        let terminal_query = terminal_tab.text_input(cx, ids!(find_input)).text();
        let (terminal_cursor, terminal_selection, terminal_offset) =
            terminal::term(terminal::Session {
                window: state.id,
                tab: id!(terminal_tab),
            })
            .map(|shared| {
                let term = shared.lock();
                (
                    Some(term.grid()[term.grid().cursor.point].c),
                    term.selection_to_string(),
                    Some(term.grid().display_offset()),
                )
            })
            .unwrap_or_default();
        let windows: Vec<_> = self.windows.iter().map(|window| window.state.read(|d| {
            serde_json::json!({"loaded": d.generation > 0, "dirty_buffers": d.blobs.iter().filter(|blob| blob.dirty()).count()})
        })).collect();
        let data = state.read(|d| {
            serde_json::json!({
                "windows": windows,
                "terminal_cursor": terminal_cursor,
                "terminal_find_query": terminal_query,
                "terminal_selection": terminal_selection,
                "terminal_offset": terminal_offset,
                "terminal_height": terminal_rect.size.y,
                "tab_titles": titles,
                "caret": d.caret.map(|caret| (caret.blob, caret.line, caret.byte)),
                "seen_lines": service::review_state(d.git_dir.as_deref()).load().seen.len(),
                "find_query": query,
                "find_count": count,
                "file_find_query": file_query,
                "file_find_count": file_count,
                "has_caret": d.caret.is_some(),
                "draft": *state.compose_draft.read().unwrap(),
                "composer_open": d.composer_tab.is_some(),
                "compose_ranges": match d.compose {
                    Some(review_doc::Composing::Lines(compose)) => Some([compose.old, compose.new].map(|side| side.map(|side| (side.start, side.end)))),
                    _ => None,
                },
                "composer_input": input,
                "focus_pending": d.compose_focus,
                "dirty_buffers": d.blobs.iter().filter(|blob| blob.dirty()).count(),
            })
        });
        let path = PathBuf::from(path).with_file_name(name);
        if let Err(error) = std::fs::write(&path, data.to_string()) {
            eprintln!(
                "concats: could not write test state to {}: {error}",
                path.display()
            );
            std::process::exit(1);
        }
    }

    /// `CONCATS_APP_FIND=text`: open the find bar and search for `text`, so
    /// the search path is drivable from a capture like the others.
    pub(super) fn find_hook(&mut self, cx: &mut Cx) {
        let Ok(query) = crate::dev_hooks::var("CONCATS_APP_FIND") else {
            return;
        };
        if query.is_empty() {
            return;
        }
        self.ui.handle_event(
            cx,
            &Event::KeyDown(KeyEvent {
                key_code: KeyCode::KeyF,
                modifiers: KeyModifiers {
                    logo: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
            &mut Scope::empty(),
        );
    }

    /// `CONCATS_APP_SAVE=1`: press Cmd-S the tick after `CONCATS_APP_TYPE`
    /// typed, so the write-back path is drivable from a capture too.
    pub(super) fn save_hook(&mut self, cx: &mut Cx) {
        if !crate::dev_hooks::var("CONCATS_APP_SAVE").is_ok_and(|v| !v.is_empty()) {
            return;
        }
        self.ui.handle_event(
            cx,
            &Event::KeyDown(KeyEvent {
                key_code: KeyCode::KeyS,
                modifiers: KeyModifiers {
                    logo: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
            &mut Scope::empty(),
        );
    }

    /// `CONCATS_APP_TYPE=text`: feed that text at the caret the tick after
    /// `CONCATS_APP_CLICK` placed one. Companion to the click hook and for the
    /// same reason — typing is otherwise unreachable from a capture, and this
    /// goes through the same `Event::TextInput` an IME or a keyboard delivers.
    ///
    /// `\n` in the value is a real newline, so a multi-line edit (the thing
    /// that moves every line after it) is drivable too.
    pub(super) fn type_hook(&mut self, cx: &mut Cx, name: &str) {
        let Ok(text) = crate::dev_hooks::var(name) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        self.ui.handle_event(
            cx,
            &Event::TextInput(TextInputEvent {
                input: text.replace("\\n", "\n"),
                ..Default::default()
            }),
            &mut Scope::empty(),
        );
    }
}
