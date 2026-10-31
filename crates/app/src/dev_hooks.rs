//! UI automation for the visual tests, compiled in with `--features dev-hooks`.
//!
//! Each `CONCATS_APP_*` variable does what a pointer or a keyboard would, so a
//! headless run can capture it. Empty reads as unset; without the feature every
//! one does.
//!
//! - At startup: `SIZE=WxH` opens at that logical size, `THEME=name` overrides
//!   the theme, and `TERM_SCRIPT=text` runs that script where a shell would
//!   start.
//! - On each load: `COMPOSE=path:start:end` opens the composer on those lines.
//! - Once the first load lands, in this order: `WINDOWS=N` opens N windows
//!   (`WINDOWS_SIZE=WxH` sizes the ones it adds), `SCROLL=N` starts the list
//!   at row N, `TERM` opens the terminal, `FILE=path[:line][,path…]` opens a File tab
//!   each, `TAB=name` (or `file:path`) selects a tab, and `SHOT=path.png`
//!   captures the frame.
//! - Then one step per poll tick: `STEPS=…` runs its `;`-separated steps
//!   first (`click x,y`, `cmd-click x,y`, `key name`, `menu command`,
//!   `drag tab x,y`, `drop tab x,y`, `find text`, `type text`), then
//!   `SHOT_BEFORE=path.png` captures the frame before any other input,
//!   `CLICK=x,y` presses there (`HOVER=x,y` only moves,
//!   `DRAG_TO=x,y` drags, `CLICK_SEEN` presses the pinned tick box), `TYPE=text`
//!   types, `SAVE` presses Cmd-S, `COMPOSER_REFRESH` lands a comment mid-draft,
//!   `FIND=text` searches (`FIND_KEEP_FOCUS` leaves the focus alone),
//!   `TYPE_AFTER_REFRESH=text` types again, `SHOT` captures again,
//!   `STATE=path` writes what the tests assert on, and `EXIT_AFTER_SHOT` quits.
use concats_diff::{Row, Side};

#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use super::*;
use crate::review_doc::{Compose, Composing, splice_composer};

/// Dev affordance, pairs with `CONCATS_APP_SHOT`: `CONCATS_APP_COMPOSE=path:start:end`
/// (0-based) pre-opens the composer on those lines, so the comment dialog can
/// be screenshotted without a pointer.
// NOTE: sets the compose the way a gesture does; splice_composer drops one it cannot place.
// coland-ignore: @coland/mixed_abstraction_levels
pub(crate) fn compose_from_env(d: &mut ReviewDoc) {
    let Some(spec) = crate::dev_hooks::var("CONCATS_APP_COMPOSE") else {
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
        span: None,
    }));
    d.compose_anchor = 0;
    splice_composer(d);
    d.compose_focus = true;
}

impl App {
    /// The hooks that fire once a load lands, in their order; see the module
    /// docs.
    #[expect(
        clippy::cognitive_complexity,
        reason = "Screenshot setup follows the same ordered phases as native startup."
    )]
    pub(super) fn apply_screenshot_hooks(&mut self, cx: &mut Cx) {
        if self.shot_done {
            return;
        }
        // NOTE: ⌘N is a menu item, which cannot be driven headlessly.
        if let Some(n) = var("CONCATS_APP_WINDOWS") {
            let n = n.parse::<usize>().unwrap_or(1);
            while self.windows.len() < n {
                let before = self.windows.len();
                self.open_new_window(cx);
                if self.windows.len() == before {
                    break;
                }
            }
            let size = var("CONCATS_APP_WINDOWS_SIZE").and_then(|spec| {
                let (w, h) = spec.split_once('x')?;
                Some(dvec2(w.parse().ok()?, h.parse().ok()?))
            });
            if let Some(size) = size {
                for window in self.windows.iter().skip(1) {
                    window.window.as_window().resize(cx, size);
                }
            }
            // Opening took the keyboard, and every other hook drives the
            // window the app started on. Hand it back, so a capture types
            // where the rest of the hooks are looking.
            if let Some(primary) = self.windows.first().map(|w| w.state.clone()) {
                self.focus_window(&primary);
            }
        }
        let Some(window) = self.primary() else {
            return;
        };
        let pane = window.pane(cx);
        if let Some(n) = var("CONCATS_APP_SCROLL").and_then(|n| n.parse::<usize>().ok()) {
            let list =
                tab_item(window, cx, window.state.read(|d| d.tab)).portal_list(cx, ids!(list));
            if let Some(mut pl) = list.borrow_mut() {
                pl.set_first_id_and_scroll(n, 0.0);
            }
        }
        if var("CONCATS_APP_TERM").is_some()
            && let Some(mut p) = pane.borrow_mut::<ReviewPane>()
        {
            p.reveal_terminal(cx);
        }
        if let Some(paths) = var("CONCATS_APP_FILE") {
            for path in paths.split(',').filter(|p| !p.is_empty()) {
                // `path:line` opens it at that line.
                let at = path
                    .rsplit_once(':')
                    .and_then(|(file, line)| Some((file, line.parse().ok()?)));
                if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                    match at {
                        Some((file, line)) => p.open_file_tab(cx, file, Some((line, 1))),
                        None => p.open_file_tab(cx, path, None),
                    }
                }
            }
        }
        // NOTE: Select after opening File tabs so file:<path> can name one.
        if let Some(tab) = var("CONCATS_APP_TAB") {
            let t = match tab.as_str() {
                "guide" => Some(Stream::Guide),
                "sessions" => Some(Stream::Sessions),
                "commits" => Some(Stream::Commits),
                "comments" => Some(Stream::Comments),
                "files" => Some(Stream::Files),
                _ => tab.strip_prefix("file:").and_then(|path| {
                    window.state.read(|d| {
                        d.files_open
                            .iter()
                            .find(|file| file.path == path)
                            .map(|file| Stream::File(file.tab))
                    })
                }),
            };
            if let Some(t) = t {
                pane.dock(cx, ids!(dock))
                    .select_tab(cx, stream_tab_spec(t).id);
                if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                    p.set_gesture_tab(cx, t);
                }
            }
        }
        if let Some(path) = var("CONCATS_APP_SHOT") {
            self.shot_done = true;
            cx.capture_next_frame_to_file(path.into());
        }
    }

    /// The release half of `CONCATS_APP_CLICK`, where its press left the
    /// pointer, once the press's timeout fires.
    pub(super) fn release_hook(&mut self, cx: &mut Cx, e: &TimerEvent) {
        if self.release_timer.is_timer(e).is_none() {
            return;
        }
        let Some(abs) = self.release.take() else {
            return;
        };
        self.ui.handle_event(
            cx,
            &Event::MouseUp(MouseUpEvent {
                abs,
                button: MouseButton::PRIMARY,
                window_id: CxWindowPool::id_zero(),
                modifiers: KeyModifiers::default(),
                time: cx.seconds_since_app_start(),
            }),
            &mut Scope::empty(),
        );
        cx.end_mouse_up(MouseButton::PRIMARY);
    }

    /// The hooks that need input, one step per poll tick; see the module docs.
    ///
    /// Interactions are otherwise unreachable from a test: the app has no
    /// accessibility tree to drive, and a synthetic `MouseDown`/`MouseUp` pair
    /// goes through the same hit test a real pointer does. The click waits a
    /// tick for a laid-out frame to hit.
    ///
    /// The dispatches are bracketed by `Cx::begin_mouse_down` /
    /// `end_mouse_move` / `end_mouse_up`, the same pointer bookkeeping the
    /// platform event loop does around a real mouse event. Without it the hits
    /// still fire but the digit is captured and never released, the hover never
    /// leaves, and the frame after the gesture is wrong.
    pub(super) fn click_hook(&mut self, cx: &mut Cx) {
        // This sequence drives the type, save, find, capture and exit hooks as
        // well as the click, so it runs with no click to dispatch: a scenario
        // that only opens a file still has to reach the tick that captures and
        // the one that leaves.
        //
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
        if let Some(path) = &self.shot_pending {
            if !path.exists() {
                self.ui.redraw(cx);
                return;
            }
            self.shot_pending = None;
        }
        self.click_tick += 1;
        match self.click_tick {
            // The frame as the load left it, before any of the hooks below
            // touch it.
            //
            // With both frames a test can ask two questions of an interaction:
            // did it change anything, and is the change right. Neither alone is
            // enough. An expected frame can't tell a state that renders from
            // one that never happened, and a difference can't tell a right
            // answer from a wrong one.
            1 => {
                if let Some(path) = var("CONCATS_APP_SHOT_BEFORE") {
                    self.request_shot(cx, path.into());
                    // Guarantee the draw that writes it, and the tick after.
                    self.ui.redraw(cx);
                }
            }
            2 => self.click(cx, &state),
            3 => self.event_hook(cx, "CONCATS_APP_TYPE", typed),
            4 => {
                self.state_hook(cx, "state-before-refresh.json");
                self.event_hook(cx, "CONCATS_APP_SAVE", |_| shortcut(KeyCode::KeyS));
                let Some(window) = self.primary() else {
                    return;
                };
                let content = tab_item(window, cx, state.read(|d| d.tab));
                if var("CONCATS_APP_FIND").is_some() && var("CONCATS_APP_FIND_KEEP_FOCUS").is_none()
                {
                    cx.set_key_focus(content.portal_list(cx, ids!(list)).area());
                }
                if var("CONCATS_APP_COMPOSER_REFRESH").is_some() {
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
                            quote: None,
                            cursors: None,
                        };
                        // NOTE: This fixture stays in memory; its cursors must
                        // not be published to the review worker.
                        drop(comments::splice_comments(d, &[comment]));
                    });
                    content.redraw(cx);
                }
            }
            5 => {
                self.event_hook(cx, "CONCATS_APP_FIND", |_| shortcut(KeyCode::KeyF));
                self.event_hook(cx, "CONCATS_APP_TYPE_AFTER_REFRESH", typed);
            }
            // Two captures, and the second is the one kept. A request is
            // answered by the next frame to PRESENT, and that frame can have
            // been encoded before the last hook's redraw was painted — which
            // is how a caret that was placed failed to appear roughly one run
            // in six. Landing the first proves the pipeline is flushed, so the
            // frame behind the second is drawn after everything settled.
            6 | 7 => {
                if self.click_tick == 6 {
                    self.event_hook(cx, "CONCATS_APP_FIND", typed);
                    // Last, so it acts on whatever the hooks before it left.
                    self.event_hook(cx, "CONCATS_APP_KEY", key);
                }
                if let Some(path) = var("CONCATS_APP_SHOT") {
                    self.request_shot(cx, path.into());
                }
                // Guarantee one more draw, so the tick that exits below is
                // reached: these ticks advance per DRAW, and once the capture is
                // requested nothing else is necessarily dirty.
                self.ui.redraw(cx);
            }
            // Leave once the frame above has been written — the hold at the
            // top of this function is what guarantees it has. A test then
            // waits for the process to exit, which is an exact signal. Waiting
            // for the file to appear is not: it catches the capture taken when
            // the load landed, before any of these ticks ran.
            8 => {
                self.state_hook(cx, "state.json");
                if var("CONCATS_APP_EXIT_AFTER_SHOT").is_some() {
                    std::process::exit(0);
                }
            }
            _ => {}
        }
    }

    /// The click hooks, in the window `state` renders: `CLICK=x,y` presses
    /// there, `HOVER=x,y` only moves there, `CLICK_SEEN` presses the pinned
    /// tick box, and `DRAG_TO=x,y` drags on before the release.
    fn click(&mut self, cx: &mut Cx, state: &WindowState) {
        let hover_only = var("CONCATS_APP_HOVER").is_some();
        let at = var("CONCATS_APP_CLICK")
            .or_else(|| var("CONCATS_APP_HOVER"))
            .and_then(|spec| pointer_position(&spec))
            .or_else(|| {
                var("CONCATS_APP_CLICK_SEEN")?;
                let tab = state.read(|d| d.tab);
                let area = tab_item(self.primary()?, cx, tab)
                    .check_box(cx, ids!(sticky.seen_box))
                    .area();
                let rect = area.rect(cx);
                (rect.size.x > 0.0 && rect.size.y > 0.0).then_some(rect.center())
            });
        let Some(abs) = at else {
            return;
        };
        if hover_only {
            let now = cx.seconds_since_app_start();
            self.pointer_move(cx, abs, now);
            return;
        }
        self.press(cx, abs, KeyModifiers::default());
        if let Some(end) = var("CONCATS_APP_DRAG_TO").and_then(|spec| pointer_position(&spec)) {
            self.pointer_move(cx, end, cx.seconds_since_app_start() + 0.05);
            self.release = Some(end);
        }
    }

    /// Press at `abs` holding `modifiers`, the pointer arriving there first,
    /// and release a moment later from `release_hook`.
    fn press(&mut self, cx: &mut Cx, abs: Vec2d, modifiers: KeyModifiers) {
        // Move the pointer there before pressing, the way a real one arrives.
        // Without this no hover state was reachable from a capture at all —
        // press and release alone never raise FingerHoverIn, so the gutter's +
        // affordance could not be screenshotted.
        let now = cx.seconds_since_app_start();
        self.pointer_move(cx, abs, now);
        let down = MouseDownEvent {
            abs,
            button: MouseButton::PRIMARY,
            window_id: CxWindowPool::id_zero(),
            modifiers,
            handled: std::cell::Cell::new(Area::Empty),
            time: now,
        };
        cx.begin_mouse_down(&down);
        self.ui
            .handle_event(cx, &Event::MouseDown(down), &mut Scope::empty());
        // NOTE: a button reads the first action its batch holds for it, and a
        // press and release sent in one event arrive as one batch that reads
        // as a press. A real click spans two events, close enough together to
        // be a tap; so does this one.
        self.release = Some(abs);
        self.release_timer = cx.start_timeout(0.05);
    }

    /// Run the next of `CONCATS_APP_STEPS` once the first load has drawn,
    /// each click letting go before the step after it. `find text` presses
    /// Cmd-F and leaves `type text` for the next tick, once the field is open.
    /// Whether the steps still hold back the per-tick hooks.
    pub(super) fn step(&mut self, cx: &mut Cx) -> bool {
        let steps = self.steps.get_or_insert_with(|| {
            var("CONCATS_APP_STEPS")
                .unwrap_or_default()
                .split(';')
                .map(|step| step.trim().to_string())
                .filter(|step| !step.is_empty())
                .collect()
        });
        if steps.is_empty() {
            return false;
        }
        let drawn = self
            .primary()
            .is_some_and(|window| window.state.read(|d| !d.files_rows.is_empty()));
        if !drawn || self.release.is_some() {
            self.ui.redraw(cx);
            return true;
        }
        let Some(step) = self.steps.as_mut().and_then(VecDeque::pop_front) else {
            return false;
        };
        let (verb, arg) = step.split_once(' ').unwrap_or((&step, ""));
        let event = match verb {
            "click" | "cmd-click" => {
                let modifiers = KeyModifiers {
                    logo: verb == "cmd-click",
                    ..KeyModifiers::default()
                };
                if let Some(abs) = pointer_position(arg) {
                    self.press(cx, abs, modifiers);
                }
                return true;
            }
            "find" => {
                if let Some(steps) = self.steps.as_mut() {
                    steps.push_front(format!("type {arg}"));
                }
                shortcut(KeyCode::KeyF)
            }
            "key" => key(arg),
            // A dock tab dragged to `x,y` and held there (`drag tab x,y`), or
            // dropped (`drop tab x,y`): the events the platform sends while a
            // tab is dragged, which a pointer can't raise headlessly.
            "drag" | "drop" => {
                let Some((tab, at)) = arg.split_once(' ') else {
                    return true;
                };
                let Some(abs) = pointer_position(at) else {
                    return true;
                };
                let items = std::sync::Arc::new(vec![DragItem::FilePath {
                    path: String::new(),
                    internal_id: Some(LiveId::from_str(tab)),
                }]);
                let window_id = CxWindowPool::id_zero();
                let handled = std::sync::Arc::new(std::sync::Mutex::new(false));
                let modifiers = KeyModifiers::default();
                if verb == "drag" {
                    Event::Drag(DragEvent {
                        window_id,
                        modifiers,
                        handled,
                        abs,
                        items,
                        response: std::sync::Arc::new(std::sync::Mutex::new(DragResponse::None)),
                    })
                } else {
                    Event::Drop(DropEvent {
                        window_id,
                        modifiers,
                        handled,
                        abs,
                        items,
                    })
                }
            }
            // What the menu item `arg` does, through the path the menu takes.
            "menu" => {
                self.menu_command(cx, LiveId::from_str(arg));
                return true;
            }
            "type" => typed(arg),
            _ => {
                eprintln!("concats-app: unknown step `{step}`");
                return true;
            }
        };
        self.ui.handle_event(cx, &event, &mut Scope::empty());
        true
    }

    /// Move the pointer to `abs`, with the bookkeeping a real move gets.
    fn pointer_move(&mut self, cx: &mut Cx, abs: Vec2d, time: f64) {
        self.ui.handle_event(
            cx,
            &Event::MouseMove(MouseMoveEvent {
                abs,
                window_id: CxWindowPool::id_zero(),
                modifiers: KeyModifiers::default(),
                handled: std::cell::Cell::new(Area::Empty),
                lock_delta: DVec2::default(),
                time,
            }),
            &mut Scope::empty(),
        );
        cx.end_mouse_move();
    }

    /// Capture the next frame to present into `path`. Whatever was there is
    /// cleared first: the ticks hold until the file exists, and a stale one
    /// would let them through before this capture presents.
    fn request_shot(&mut self, cx: &mut Cx, path: PathBuf) {
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "concats-app: cannot remove the last capture {}: {error}",
                path.display()
            );
        }
        cx.capture_next_frame_to_file(path.clone());
        self.shot_pending = Some(path);
    }

    /// When `name` is set, dispatch the event made from its value.
    fn event_hook(&mut self, cx: &mut Cx, name: &str, event: impl FnOnce(&str) -> Event) {
        if let Some(value) = var(name) {
            self.ui
                .handle_event(cx, &event(&value), &mut Scope::empty());
        }
    }

    /// What a press in each window drags, asked the way macOS asks: in the
    /// middle of its header, of each of its buttons, and below it.
    // NOTE: split out of state_hook, which clippy holds to 100 lines.
    fn captions(&self, cx: &Cx) -> Vec<serde_json::Value> {
        self.windows
            .iter()
            .map(|window| {
                let pane = window.pane(cx);
                let id = window.window.as_window().window_id();
                let drags = |at: Vec2d| id.is_some_and(|id| self.caption_at(cx, id, at));
                let middle = |r: Rect| dvec2(r.pos.x + r.size.x / 2.0, r.pos.y + r.size.y / 2.0);
                let header = middle(pane.view(cx, ids!(header)).area().rect(cx));
                let buttons: Vec<_> = [
                    ids!(repo_button),
                    ids!(base_button),
                    ids!(head_button),
                    ids!(load_button),
                    ids!(share_button),
                ]
                .into_iter()
                .map(|button| drags(middle(pane.widget(cx, button).area().rect(cx))))
                .collect();
                serde_json::json!({
                    "header": drags(header),
                    "buttons": buttons,
                    "below": drags(header + dvec2(0.0, 200.0)),
                })
            })
            .collect()
    }

    fn state_hook(&self, cx: &mut Cx, name: &str) {
        let (Some(path), Some(window)) = (var("CONCATS_APP_STATE"), self.primary()) else {
            return;
        };
        let state = &window.state;
        let owner = state.read(|d| d.composer_tab);
        let input = owner.and_then(|tab| {
            tab_item(window, cx, tab)
                .borrow::<widgets::ReviewList>()
                .and_then(|list| list.composer_input)
                .map(|uid| uid.0)
        });
        let active = tab_item(window, cx, state.read(|d| d.tab));
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
        let browser = dock.item(id!(browser_tab));
        let file_query = browser.text_input(cx, ids!(find_input)).text();
        let file_count = browser.label(cx, ids!(find_count)).text();
        let bars = dock.clone_state().map(|items| tab_bars(&items));
        let panels = window
            .pane(cx)
            .borrow_mut::<ReviewPane>()
            .map(|mut pane| pane.panel_sizes(cx));
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
        let captions = self.captions(cx);
        let data = state.read(|d| {
            serde_json::json!({
                "windows": windows,
                "captions": captions,
                "terminal_cursor": terminal_cursor,
                "terminal_find_query": terminal_query,
                "terminal_selection": terminal_selection,
                "terminal_offset": terminal_offset,
                "terminal_height": terminal_rect.size.y,
                "bars": bars,
                "panels": panels.map(|[left, right, bottom]| {
                    serde_json::json!({"left": left, "right": right, "bottom": bottom})
                }),
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
}

// NOTE: click_hook reads both the click and the drag-to point through it.
// coland-ignore: @coland/single_caller_private_function
fn pointer_position(spec: &str) -> Option<Vec2d> {
    let (x, y) = spec.split_once(',')?;
    Some(dvec2(x.trim().parse().ok()?, y.trim().parse().ok()?))
}

// NOTE: the one gate every hook reads the environment through; its fan-in and input are the point.
// coland-ignore: @coland/high_call_degree, @coland/implicit_global_input
pub(crate) fn var(name: &str) -> Option<String> {
    cfg!(feature = "dev-hooks")
        .then(|| std::env::var(name).ok())
        .flatten()
        .filter(|value| !value.is_empty())
}

/// The dock item that renders `tab` in `window`.
fn tab_item(window: &AppWindow, cx: &Cx, tab: Stream) -> WidgetRef {
    window
        .pane(cx)
        .dock(cx, ids!(dock))
        .item(stream_tab_spec(tab).id)
}

/// Cmd plus `key_code`, the way a shortcut arrives.
// NOTE: how a shortcut arrives, for each one a hook presses.
// coland-ignore: @coland/single_caller_private_function
fn shortcut(key_code: KeyCode) -> Event {
    Event::KeyDown(KeyEvent {
        key_code,
        modifiers: KeyModifiers {
            logo: true,
            ..Default::default()
        },
        ..Default::default()
    })
}

/// Each area's tab bar, by tab title: where a view landed.
// NOTE: split out of state_hook, which clippy holds to 100 lines.
fn tab_bars(items: &std::collections::HashMap<LiveId, DockItem>) -> serde_json::Value {
    let titles = |bar| match items.get(&bar) {
        Some(DockItem::Tabs { tabs, .. }) => tabs
            .iter()
            .filter_map(|tab| match items.get(tab) {
                Some(DockItem::Tab { name, .. }) => Some(name.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    serde_json::json!({
        "center": titles(id!(main_tabs)),
        "left": titles(id!(left_tabs)),
        "right": titles(id!(right_tabs)),
        "bottom": titles(id!(bottom_tabs)),
    })
}

/// The key `name` names, pressed on its own: `Escape` closes a find bar,
/// `Return` posts a composer.
// NOTE: the KEY hook takes it as its event, and a `key` step calls it.
// coland-ignore: @coland/single_caller_private_function
fn key(name: &str) -> Event {
    let key_code = match name {
        "Escape" => KeyCode::Escape,
        "Return" => KeyCode::ReturnKey,
        _ => KeyCode::Unknown,
    };
    Event::KeyDown(KeyEvent {
        key_code,
        ..Default::default()
    })
}

/// Text at the caret, through the same `Event::TextInput` an IME or a keyboard
/// delivers. `\n` in it is a real newline, so a multi-line edit is drivable too.
// NOTE: the TYPE and FIND hooks take it as their event, and a `type` step calls it.
// coland-ignore: @coland/single_caller_private_function
fn typed(text: &str) -> Event {
    Event::TextInput(TextInputEvent {
        input: text.replace("\\n", "\n"),
        ..Default::default()
    })
}
