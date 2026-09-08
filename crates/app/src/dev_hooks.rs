//! UI automation used by visual tests, enabled with `--features dev-hooks`.
use super::*;

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
    /// (CONCATS_APP_COMBO/SHARE/TAB/SCROLL/TERM/SETTINGS/SHOT) fire once per
    /// run, after a load lands; SHOT_EVERY re-arms them on every load. Each
    /// opens the UI a pointer would, so a headless test can capture it. Nothing
    /// in production reads them.
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
            };
        }
        // CONCATS_APP_PICK_REPO=1: pre-open the repo picker (recent repos +
        // "Open dir…") so its dropdown can be screenshotted without a pointer.
        if crate::dev_hooks::var("CONCATS_APP_PICK_REPO").is_ok_and(|v| !v.is_empty()) {
            let pane = self.ui.widget(cx, ids!(pane_a));
            if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                p.repo_open(cx);
            };
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
        // CONCATS_APP_TAB=guide|sessions|commits|comments|files: land on a
        // specific tab, so each one can be screenshotted.
        if let Ok(tab) = crate::dev_hooks::var("CONCATS_APP_TAB") {
            let t = match tab.as_str() {
                "guide" => Some(Tab::Guide),
                "sessions" => Some(Tab::Sessions),
                "commits" => Some(Tab::Commits),
                "comments" => Some(Tab::Comments),
                "files" => Some(Tab::Files),
                _ => None,
            };
            if let Some(t) = t {
                let pane = self.ui.widget(cx, ids!(pane_a));
                pane.dock(cx, ids!(dock))
                    .select_tab(cx, stream_tab_spec(t).0);
                if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                    p.set_gesture_tab(cx, t);
                };
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
            let content = pane.dock(cx, ids!(dock)).item(stream_tab_spec(t).0);
            let list = content.portal_list(cx, ids!(list));
            if let Some(mut pl) = list.borrow_mut() {
                pl.set_first_id_and_scroll(n, 0.0);
            };
        }
        // CONCATS_APP_TERM=1: pre-open the terminal panel (and its
        // shell) so it can be screenshotted without a pointer.
        if crate::dev_hooks::var("CONCATS_APP_TERM").is_ok_and(|v| !v.is_empty()) {
            let pane = self.ui.widget(cx, ids!(pane_a));
            if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                p.reveal_terminal(cx);
            };
        }
        // CONCATS_APP_SETTINGS=1: pre-open the settings editor so it
        // can be screenshotted without a pointer.
        if crate::dev_hooks::var("CONCATS_APP_SETTINGS").is_ok_and(|v| !v.is_empty()) {
            let pane = self.ui.widget(cx, ids!(pane_a));
            if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                p.open_settings_tab(cx);
            };
        }
        // CONCATS_APP_FILE=path[,path…]: open those files of the head tree,
        // as picking them in the browser would — one tab each.
        if let Ok(paths) = crate::dev_hooks::var("CONCATS_APP_FILE") {
            for path in paths.split(',').filter(|p| !p.is_empty()) {
                let pane = self.ui.widget(cx, ids!(pane_a));
                if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                    p.open_file_tab(cx, path.to_string());
                };
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
    pub(super) fn click_hook(&mut self, cx: &mut Cx) {
        // The pointer position, when there is one. This sequence drives the type,
        // save, find, capture and exit hooks as well, so it runs with no click to
        // dispatch — a scenario that only opens a file still has to reach the
        // tick that captures and the one that leaves.
        let at = crate::dev_hooks::var("CONCATS_APP_CLICK")
            .ok()
            .and_then(|spec| {
                let (x, y) = spec.split_once(',')?;
                Some((x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?))
            });
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
            3 => self.type_hook(cx),
            4 => self.save_hook(cx),
            5 => self.find_hook(cx),
            // Two captures, and the second is the one kept. A request is
            // answered by the next frame to PRESENT, and that frame can have
            // been encoded before the last hook's redraw was painted — which
            // is how a caret that was placed failed to appear roughly one run
            // in six. Landing the first proves the pipeline is flushed, so the
            // frame behind the second is drawn after everything settled.
            6 | 7 => {
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
                if crate::dev_hooks::var("CONCATS_APP_EXIT_AFTER_SHOT").is_ok_and(|v| !v.is_empty())
                {
                    std::process::exit(0);
                }
            }
            _ => {}
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
        self.ui.handle_event(
            cx,
            &Event::TextInput(TextInputEvent {
                input: query,
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
    pub(super) fn type_hook(&mut self, cx: &mut Cx) {
        let Ok(text) = crate::dev_hooks::var("CONCATS_APP_TYPE") else {
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
