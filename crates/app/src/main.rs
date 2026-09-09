//! Concats App — the desktop review GUI.
//!
//! Open any diff: point it at a repo and two revisions, and it renders a
//! tree-sitter-highlighted review document an agent can organize.
//!
//!   cargo run -p concats-app --release -- [repo] [base] [head]
//!
//! The chrome follows the shared Concats design. A caption-height header
//! carries the repo name (click it to pick another repo), the diff-picker chip,
//! and a ↻ that reloads the open repo and spins while a load is in flight; the
//! native traffic lights float over its left edge. Below it a dock, whose tab
//! strip holds Guide, Sessions, Commits, Comments and File Diff: tabs without
//! content don't exist, tabs close via their ✕ and come back via the status-bar
//! view buttons, and they drag to reorder, split and merge (makepad-studio's
//! dock, restyled). File cards carry a per-file viewed tick box. The status bar
//! shows error/warning counts, the load stats, the view buttons, the terminal
//! toggle and its `+` (more shell sessions as closable tabs), and a `{ }`
//! button that opens a Settings tab (theme and font, as editable highlighted
//! JSON). The chip opens an autocomplete over the repo's refs: pick one to diff
//! it against HEAD, or type an explicit `base...head`.
//!
//! Perf: F3 toggles the live frame graph. Loading happens on a worker thread,
//! so a huge diff slows the load, not the window.
//!
//! With `--features dev-hooks`: CONCATS_APP_SHOT=/path.png writes a PNG of the frame after the
//! first load lands (no macOS screen-recording permission needed).
//! CONCATS_APP_COMPOSE=path:s:e pre-opens the comment composer,
//! CONCATS_APP_COMBO=1 the diff picker, CONCATS_APP_PICK_REPO=1 the repo picker,
//! CONCATS_APP_SHARE=1 the share dropdown, CONCATS_APP_SETTINGS=1 the Settings
//! tab. CONCATS_APP_FILE=path[,path…] opens a File tab per named file of the
//! head tree, as picking them in the browser would. CONCATS_APP_TYPE=text types
//! that text at the caret a tick after CONCATS_APP_CLICK placed one.
//! CONCATS_APP_SCROLL=N starts the list at row N, which screenshots the sticky
//! header without a pointer. CONCATS_APP_WINDOWS=N opens N windows on the
//! range, the way ⌘N does. CONCATS_APP_THEME=name overrides the startup theme
//! (else the persisted config, else the built-in Concats theme).

use std::{path::PathBuf, sync::Arc};

use concats_diff::LineKind;
use concats_review::store;
use concats_state::Target;
use dock::{load_layout, stream_tab_spec, sync_stream_tab};
use load::resplice_comments;
pub use makepad_widgets;
use makepad_widgets::*;
use review_doc::{ReviewDoc, Stream, status_line};
use service::{ReviewCmd, ReviewUpdate, review, review_state};
use widgets::ReviewPane;
use window::WindowState;

use crate::theme::paint;

mod dev_hooks;
mod dock;
mod editor;
/// One open file per tab, and the Settings tab that rides the same mechanism.
mod file_view;
mod links;
/// Driving a load on a worker thread and landing it as a whole new document.
mod load;
/// The recent-repos list behind the header's picker.
mod recents;
mod review_doc;
mod service;
/// The in-app terminal: PTY session + emulation (terminal.rs) and the
/// renderer copied from makepad-studio (terminal_view.rs).
mod terminal;
mod terminal_view;
/// The active theme and font: `concats_theme`'s palette, plus where this app
/// keeps its own.
mod theme;
mod widgets;
/// One window's own state: its document, its load, its identity.
mod window;

/// `app_main!` always defines `fn main()`. Putting it in a module demotes that to
/// an ordinary (unused) function, so the crate root can own the real entry point
/// and answer a headless caller before any window exists.
mod gui {
    use super::*;
    app_main!(App);
}

/// What this binary takes. The review subcommands belong to `concats`, not to
/// this binary: one CLI, and it needs no window.
#[derive(clap::Parser)]
#[command(
    version,
    about = "Review a Git diff",
    after_help = "Ranges accept Git revisions, INDEX, and WORKTREE. Review commands live on `concats`."
)]
struct Args {
    #[arg(long)]
    repo: Option<String>,
    #[arg(long)]
    base: Option<String>,
    #[arg(long)]
    head: Option<String>,
    #[arg(long)]
    guide: Option<String>,
    #[arg(value_name = "REPO", conflicts_with = "repo")]
    repo_path: Option<String>,
    #[arg(value_name = "BASE", conflicts_with = "base")]
    base_rev: Option<String>,
    #[arg(value_name = "HEAD", conflicts_with = "head")]
    head_rev: Option<String>,
}

fn main() {
    use clap::Parser;
    Args::parse();
    gui::app_main();
}

/// What one window's rows are drawn from, and what its event handlers reach
/// for. Put in `Scope` by that window's `ReviewPane` — the pane is where a
/// per-window scope starts, because `Root` hands every window the same one.
pub(crate) struct FrameData {
    document: Arc<ReviewDoc>,
    review: Arc<service::ReviewState>,
    theme: Arc<theme::Theme>,
    focus_composer: Option<Stream>,
    compose_draft: String,
    /// The window these rows belong to, so a handler that mutates the document
    /// mutates its own.
    pub(crate) state: Arc<WindowState>,
}

/// The event-path counterpart of [`FrameData`]. A draw needs the whole frame;
/// an event only needs to know whose document it is, and mouse moves are far
/// too frequent to rebuild the rest for.
pub(crate) struct WindowScope(pub(crate) Arc<WindowState>);

/// The window behind the rows being handled, or `None` outside a pane.
pub(crate) fn frame_state(scope: &mut Scope) -> Option<Arc<WindowState>> {
    if let Some(window) = scope.data.get::<WindowScope>() {
        return Some(window.0.clone());
    }
    scope.data.get::<FrameData>().map(|f| f.state.clone())
}

pub(crate) struct FrameTheme(Arc<theme::Theme>);

pub(crate) fn frame_theme<'a>(scope: &'a mut Scope) -> Option<&'a theme::Theme> {
    if let Some(frame) = scope.props.get::<FrameTheme>() {
        return Some(&frame.0);
    }
    if let Some(frame) = scope.data.get::<FrameData>() {
        return Some(&frame.theme);
    }
    None
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.ReviewPane
    // The window clear color; the full palette lives in widgets/styles.rs.
    use mod.widgets.C_BG

    // Defined once and used twice: the window the app starts on, and the
    // template ⌘N opens more of. A window declared under `window_templates` is
    // not a child of Root, so nothing opens it until `open_window` asks.
    mod.widgets.ReviewWindow = Window {
        window.inner_size: vec2(1280, 940)
        window.title: "concats app"
        show_caption_bar: false
        pass.clear_color: C_BG
        body +: {
            width: Fill
            height: Fill
            flow: Down
            pane_a := ReviewPane {}
        }
    }

    load_all_resources() do #(App::script_component(vm)) {
        ui: Root {
            window_a := mod.widgets.ReviewWindow {}
            window_templates: { review := mod.widgets.ReviewWindow {} }
        }
    }
}

// ---------------------------------------------------------------------------
// Diff row colors, resolved from the active theme (see theme.rs). The draw code
// clones the theme Arc once per pass and hands `&Theme` to these helpers.
// ---------------------------------------------------------------------------

/// The design's row tints are 8% marker color over the card background
/// (`rgba(77,208,126,0.08)`). Pre-mixed to opaque here so the quads never
/// depend on blend state.
fn mix4(a: Vec4f, b: Vec4f, t: f32) -> Vec4f {
    vec4(
        a.x + (b.x - a.x) * t,
        a.y + (b.y - a.y) * t,
        a.z + (b.z - a.z) * t,
        1.0,
    )
}

/// The row tint over the card background: additions greenish, deletions
/// reddish, context transparent (the card shows through).
fn row_bg(theme: &theme::Theme, kind: LineKind) -> Vec4f {
    match kind {
        LineKind::Add => mix4(paint(theme.surface), paint(theme.added), 0.08),
        LineKind::Del => mix4(paint(theme.surface), paint(theme.deleted), 0.08),
        LineKind::Context => paint(theme.surface),
    }
}

/// A row inside the range being commented on, over its own change tint. Wanted
/// strong enough to read the moment a drag crosses a line: at 0.10 the shift was
/// technically there and easy to miss, which made selecting feel unresponsive.
fn row_selected_bg(theme: &theme::Theme, kind: LineKind) -> Vec4f {
    mix4(row_bg(theme, kind), paint(theme.accent), 0.22)
}

/// The 6px change marker at the card's left edge.
fn row_marker(theme: &theme::Theme, kind: LineKind) -> Option<Vec4f> {
    match kind {
        LineKind::Add => Some(paint(theme.added)),
        LineKind::Del => Some(paint(theme.deleted)),
        LineKind::Context => None,
    }
}

/// Vertical padding of one code row. With the 9pt mono this lands near the
/// design's 20px line rhythm.
const ROW_PAD: f64 = 2.75;

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    show_perf: bool,
    /// Every open window, in the order they were opened. The first is the one
    /// the app started on; the dev hooks and the CLI both mean that one.
    #[rust]
    windows: Vec<AppWindow>,
    /// The window that last took focus — the one ⌘N copies its range from.
    #[rust]
    focused: Option<LiveId>,
    /// The range the app was started on, which ⌘N falls back to while the
    /// first load is still in flight and no document has a range yet.
    #[rust]
    launched_on: Target,
    /// Dev affordance: `CONCATS_APP_SHOT=/path.png` captures the frame
    /// after the first load lands — visual verification without macOS screen
    /// recording permissions. Once per run.
    #[rust]
    shot_done: bool,
    /// The ~1s poll that picks up what other writers left: a `submit`ted
    /// guide, a CLI-added comment. One `data_version` pragma read per tick
    /// when idle — no git access, no table reads until something changed.
    #[rust]
    poll: Timer,
    /// Ticks since `CONCATS_APP_CLICK` was seen: the click needs a laid-out
    /// frame to hit, and the capture needs a frame after the click.
    #[rust]
    click_tick: u32,
    /// A capture asked for but not yet on disk. The ticks below hold here
    /// until it lands, so each one answers from its own presented frame.
    #[rust]
    shot_pending: Option<PathBuf>,
}

/// One open window: the document it renders, the widget subtree it renders
/// into, and what the App remembers about it.
struct AppWindow {
    state: Arc<WindowState>,
    /// This window's `Window` widget. Every widget lookup for this window goes
    /// through it rather than through the root.
    window: WidgetRef,
    /// The document generation last reflected into the chrome; `reconcile` is
    /// a no-op until it changes.
    seen: Option<u64>,
    /// Sum of the open buffers' edit counters when they were last cached, so
    /// the poll writes a snapshot only after something actually changed.
    cached_rev: u64,
    /// WORKTREE reviews only: the stat-only worktree fingerprint as last seen.
    /// The worktree moves under the review as the user edits (or stages); a
    /// change here reloads the pane. 0 = not yet baselined.
    worktree_fp: u64,
    /// The repo whose persisted dock layout is currently applied; layouts load
    /// once per repo, when its first load lands.
    layout_git_dir: Option<PathBuf>,
    /// The comment revision already spliced into the document. A tick box
    /// publishes as often as a comment does, and splicing walks every row of
    /// every stream — so it only runs when this moves.
    spliced_rev: u64,
}

impl AppWindow {
    fn new(state: Arc<WindowState>, window: WidgetRef) -> Self {
        Self {
            state,
            window,
            seen: None,
            cached_rev: 0,
            worktree_fp: 0,
            layout_git_dir: None,
            spliced_rev: 0,
        }
    }
}

impl MatchEvent for App {
    /// Replies from the review service. The action only says that something
    /// changed; what it means for pixels is decided here.
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            match action.downcast_ref::<ReviewUpdate>() {
                // Repo state, not window state: every window on that repo
                // wants it, and `review_state_changed` no-ops on the rest.
                Some(ReviewUpdate::State) => {
                    for window in &mut self.windows {
                        window.review_state_changed(cx);
                    }
                }
                Some(ReviewUpdate::GuideReady { window }) => {
                    if let Some(w) = self.window_mut(*window) {
                        w.guide_ready(cx);
                    }
                }
                Some(ReviewUpdate::WorktreeChanged { window, fp }) => {
                    if let Some(w) = self.window_mut(*window) {
                        w.worktree_changed(cx, *fp);
                    }
                }
                Some(ReviewUpdate::Status { window, message }) => {
                    if let Some(window) = self.window_mut(*window) {
                        window
                            .pane(cx)
                            .label(cx, ids!(status_label))
                            .set_text(cx, message);
                        window.window.redraw(cx);
                    }
                }
                Some(ReviewUpdate::FileSaved {
                    window,
                    path,
                    oid,
                    version,
                }) => {
                    if let Some(window) = self.window_mut(*window) {
                        window.state.with(|d| {
                            if let Some(blob) =
                                d.blobs.iter_mut().find(|b| b.origin.as_ref() == Some(path))
                            {
                                blob.saved_at(*oid, version.clone());
                                d.rows_rev += 1;
                            }
                        });
                        window.window.redraw(cx);
                    }
                }
                Some(ReviewUpdate::HighlightReady {
                    window,
                    generation,
                    blob,
                    rev,
                    spans,
                }) => {
                    let Some(state) = self.window_mut(*window).map(|w| w.state.clone()) else {
                        continue;
                    };
                    let landed = state.with(|document| {
                        if document.generation != *generation {
                            return false;
                        }
                        // Spans describe the text as it was when the work
                        // started. A keystroke since then moved every byte
                        // after it, so landing these would paint the line in
                        // colours belonging to characters that have moved.
                        if let Some(target) = document
                            .blobs
                            .get_mut(*blob as usize)
                            .filter(|b| b.edit_rev == *rev)
                        {
                            target.spans = Some(spans.clone());
                            target.spans_rev = *rev;
                        }
                        true
                    });
                    if landed {
                        self.ui.redraw(cx);
                    }
                }
                // App-wide, not per-window: the recents list is one list.
                Some(ReviewUpdate::Recents(recents)) => {
                    for window in &self.windows {
                        if let Some(mut pane) = window.pane(cx).borrow_mut::<ReviewPane>() {
                            pane.set_recents(cx, recents.clone());
                        }
                    }
                }
                None => {}
            }
        }
    }

    fn handle_startup(&mut self, cx: &mut Cx) {
        // CONCATS_APP_SIZE=WxH: open at an exact logical size, so a capture can
        // be laid over a design frame of the same size without rescaling — the
        // layout itself changes with width, so a resized screenshot is not the
        // same picture. Startup only; makepad ignores it later.
        if let Ok(spec) = crate::dev_hooks::var("CONCATS_APP_SIZE")
            && let Some((w, h)) = spec.split_once('x')
            && let (Ok(w), Ok(h)) = (w.parse::<f64>(), h.parse::<f64>())
        {
            self.ui.window(cx, ids!(window_a)).configure_window(
                cx,
                Vec2d { x: w, y: h },
                Vec2d { x: 80.0, y: 80.0 },
                false,
                "concats app".to_string(),
            );
        }
        // The platform installs a menu bar holding nothing but Quit
        // (`init_quit_menu`), and `update_macos_menu` replaces it whole — so
        // Quit has to be re-declared here beside what we add. macOS titles the
        // first submenu from the bundle's CFBundleName whatever we pass; only
        // the Quit item's own label is ours to keep in step with it.
        #[cfg(target_os = "macos")]
        cx.update_macos_menu(MacosMenu::Main {
            items: vec![
                MacosMenu::Sub {
                    name: "Concats".to_string(),
                    items: vec![MacosMenu::Item {
                        command: live_id!(quit),
                        key: KeyCode::KeyQ,
                        shift: false,
                        enabled: true,
                        name: "Quit Concats".to_string(),
                    }],
                },
                MacosMenu::Sub {
                    name: "File".to_string(),
                    items: vec![MacosMenu::Item {
                        command: live_id!(new_window),
                        key: KeyCode::KeyN,
                        shift: false,
                        enabled: true,
                        name: "New Window".to_string(),
                    }],
                },
            ],
        });

        use clap::Parser;
        let args = Args::parse();
        let guide = args.guide;
        let repo = args.repo.or(args.repo_path).unwrap_or_else(|| ".".into());
        let base = args
            .base
            .or(args.base_rev)
            .unwrap_or_else(|| "HEAD~5".into());
        let head = args.head.or(args.head_rev).unwrap_or_else(|| "HEAD".into());

        self.adopt_window(cx, id!(window_a), self.ui.widget(cx, ids!(window_a)));
        self.launched_on = Target { repo, base, head };
        let target = self.launched_on.clone();
        let window = &self.windows[0];
        if let Some(mut pane) = window.pane(cx).borrow_mut::<ReviewPane>() {
            pane.start_load(cx, target, guide, Some("loading…"));
        }
        self.poll = cx.start_interval(1.0);
    }

    fn handle_window_got_focus(&mut self, _cx: &mut Cx, window_id: &WindowId) {
        let Some(state) = self
            .windows
            .iter()
            .find(|w| w.window.as_window().window_id() == Some(*window_id))
            .map(|w| w.state.clone())
        else {
            return;
        };
        self.focused = Some(state.id);
        self.focus_window(&state);
    }

    fn handle_timer(&mut self, cx: &mut Cx, e: &TimerEvent) {
        if self.poll.is_timer(e).is_some() {
            self.click_hook(cx);
            for window in &mut self.windows {
                window.poll_tick(cx);
            }
        }
    }

    /// A worker finished. Refresh whichever pane(s) changed: status line,
    /// error/warning counts, the header's dir + range chips, and which tabs
    /// exist at all (no Guide without a guide, no Sessions without sessions).
    fn handle_signal(&mut self, cx: &mut Cx) {
        // Terminal output pumps through the same UI signal as the loader.
        let dirty_terminals = terminal::take_dirty();
        if !dirty_terminals.is_empty() {
            for window in &self.windows {
                let dock = window.pane(cx).dock(cx, ids!(dock));
                for session in dirty_terminals
                    .iter()
                    .filter(|s| s.window == window.state.id)
                {
                    dock.item(session.tab).redraw(cx);
                    // What the program in there said about itself: the name it
                    // wants on its tab, and anything it asked to copy.
                    let report = terminal::take_report(*session);
                    if let Some(text) = report.clipboard {
                        cx.copy_to_clipboard(&text);
                    }
                    if let Some(title) = report.title.filter(|t| !t.is_empty()) {
                        dock.set_tab_title(cx, session.tab, title);
                    }
                }
            }
            // Pairs with CONCATS_APP_TERM: re-capture on terminal frames
            // so the shot shows the shell's actual output, not the pre-spawn
            // blank panel.
            if self.shot_done
                && crate::dev_hooks::var("CONCATS_APP_TERM").is_ok_and(|v| !v.is_empty())
                && let Ok(path) = crate::dev_hooks::var("CONCATS_APP_SHOT")
                && !path.is_empty()
            {
                cx.capture_next_frame_to_file(path.into());
            }
        }
        let mut dirty = false;
        for window in &mut self.windows {
            dirty |= window.reconcile(cx);
        }
        if dirty {
            self.apply_screenshot_hooks(cx);
        }
    }

    /// F3 toggles the live perf-graph overlay.
    fn handle_key_down(&mut self, cx: &mut Cx, e: &KeyEvent) {
        if e.key_code == KeyCode::F3 {
            self.show_perf = !self.show_perf;
            for window in &self.windows {
                window
                    .pane(cx)
                    .view(cx, ids!(perf_overlay))
                    .set_visible(cx, self.show_perf);
            }
            self.ui.redraw(cx);
        }
    }
}

impl App {
    /// Take a window into the App's care: mint its state, hand it to its pane
    /// (the pane is what puts it in `Scope` for the rows below), and remember
    /// it. Called for the declared window at startup and for each ⌘N window.
    /// The window the dev hooks and the CLI mean: the one the app started on.
    fn primary(&self) -> Option<&AppWindow> {
        self.windows.first()
    }

    fn adopt_window(&mut self, cx: &mut Cx, id: LiveId, window: WidgetRef) {
        let state = WindowState::new(id, window.as_window().window_id());
        if let Some(mut pane) = window.widget(cx, ids!(pane_a)).borrow_mut::<ReviewPane>() {
            pane.adopt(state.clone());
        }
        self.focus_window(&state);
        self.windows.push(AppWindow::new(state, window));
    }

    /// One window has the keyboard at a time. Opening a window takes it, and
    /// the platform's focus events keep it in step after that.
    fn focus_window(&self, state: &Arc<WindowState>) {
        for window in &self.windows {
            window.state.set_focused(false);
        }
        state.set_focused(true);
    }

    fn window_mut(&mut self, id: LiveId) -> Option<&mut AppWindow> {
        self.windows.iter_mut().find(|w| w.state.id == id)
    }

    /// The window ⌘N copies from: whichever last had focus, else the newest.
    fn focused_window(&self) -> Option<&AppWindow> {
        self.focused
            .and_then(|id| self.windows.iter().find(|w| w.state.id == id))
            .or_else(|| self.windows.last())
    }

    /// ⌘N: another window on the range the focused one is showing, in this
    /// process. Its document, its load and its terminals are its own; the
    /// review store underneath is shared, so a tick in one shows in the other.
    fn open_new_window(&mut self, cx: &mut Cx) {
        let target = self
            .focused_window()
            .map(|window| window.state.read(ReviewDoc::target))
            .filter(|t| !t.repo.is_empty())
            // Nothing loaded yet: the range the app was asked for is the same
            // answer, and an empty one would open on the process cwd.
            .unwrap_or_else(|| self.launched_on.clone());

        let id = LiveId::unique();
        let Some(widget) = self.ui.as_root().open_window(cx, id, live_id!(review)) else {
            eprintln!("concats-app: could not open another window");
            return;
        };
        self.adopt_window(cx, id, widget);
        let Some(window) = self.windows.last() else {
            return;
        };
        if let Some(mut pane) = window.pane(cx).borrow_mut::<ReviewPane>() {
            pane.start_load(cx, target, None, Some("loading…"));
        }
    }

    /// A window went away — dismissed by the user, or by the app. `Root` drops
    /// the widget; this drops what the App knew about it, so nothing keeps
    /// polling a document nobody can see.
    fn retire_window(&mut self, window_id: WindowId) {
        self.windows
            .retain(|w| w.window.as_window().window_id() != Some(window_id));
    }

    // Per-window work lives on `AppWindow`.
}

impl AppWindow {
    /// This window's pane. Every lookup goes through the window's own subtree:
    /// `ids!(pane_a)` from the root answers with the FIRST match, and with two
    /// windows open there are two panes answering to it.
    fn pane(&self, cx: &Cx) -> WidgetRef {
        self.window.widget(cx, ids!(pane_a))
    }

    /// Reflect this window's document into its chrome. Returns whether
    /// anything changed, which is the App's cue to run the capture hooks.
    fn reconcile(&mut self, cx: &mut Cx) -> bool {
        let s = self.state.snapshot();
        // Spin the header's ↻ whenever a load is in flight. This runs on every
        // signal (load start and land both signal), not only on a generation
        // change, so the spinner starts the moment a load begins.
        if let Some(mut p) = self.pane(cx).borrow_mut::<ReviewPane>() {
            p.set_loading(cx, s.loading);
        }
        let mut dirty = false;
        // Reconcile the chrome only when the document actually changed
        // (generation bumps on each landed load), so redraws stay rare.
        // Generation 0 is the empty document every run starts on: reconciling
        // it would close the three stream tabs before the first load can say
        // whether it has them, and they would come back appended — the tab
        // strip out of stream order for the rest of the session.
        if s.generation > 0 && self.seen != Some(s.generation) {
            self.seen = Some(s.generation);
            let pane = self.pane(cx);
            pane.label(cx, ids!(status_label))
                .set_text(cx, &status_line(&s));
            // The header chips: the repo's dir name and the loaded range. The
            // canonical path also seeds the picker's recents (so "." resolves
            // to a stable absolute path the list can dedup on).
            review().send(ReviewCmd::RecordRecent(s.repo.clone()));
            let dir = std::path::Path::new(&s.repo)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| s.repo.clone());
            pane.button(cx, ids!(repo_button))
                .set_text(cx, if dir.is_empty() { "concats app" } else { &dir });
            pane.button(cx, ids!(range_button))
                .set_text(cx, &format!("{}…{}", s.base, s.head));
            // The load rebuilt every stream, so nothing is spliced any more.
            self.spliced_rev = 0;
            // Adopt the repo the load landed on: the service opens its store
            // (sqlite, on its thread) and publishes what is already recorded.
            if let Some(git_dir) = s.git_dir.clone() {
                review().send(ReviewCmd::Open(git_dir));
            }
            let dock = pane.dock(cx, ids!(dock));
            // Restore this repo's persisted dock layout, once per repo —
            // before reconciliation, which then closes any restored tab
            // whose stream this particular load doesn't have.
            if s.git_dir.is_some() && self.layout_git_dir != s.git_dir {
                self.layout_git_dir = s.git_dir.clone();
                if let Some(layout) = s.git_dir.as_deref().and_then(load_layout) {
                    let open = matches!(
                        layout.dock_items.get(&id!(root)),
                        Some(DockItem::Splitter {
                            align: SplitterAlign::FromB(h),
                            ..
                        }) if *h > 1.0
                    );
                    if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                        p.bottom_restore = layout.bottom_restore;
                        p.sidebar_restore = layout.sidebar_restore;
                        // Stream tabs absent from the saved layout stay
                        // closed. (A stream that was merely unavailable at
                        // save time lands here too — self-healing, its
                        // status-bar button brings it back.)
                        for t in crate::review_doc::STREAMS {
                            let tab_id = stream_tab_spec(t).id;
                            if !layout.dock_items.contains_key(&tab_id) {
                                p.user_closed.insert(tab_id);
                            }
                        }
                    }
                    dock.load_state(cx, layout.dock_items);
                    if open && let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                        // A restored-open panel needs its shell running;
                        // extra session tabs respawn when pressed.
                        p.open_terminal(cx, id!(terminal_tab));
                    }
                }
            }
            // Tabs that have nothing to show don't exist — and tabs the user
            // closed stay closed. Reconcile the dock against this load:
            // close absent streams, (re)create the ones that appeared, keep
            // the user's layout otherwise.
            let closed = pane
                .borrow::<ReviewPane>()
                .map(|p| p.user_closed.clone())
                .unwrap_or_default();
            for (t, available) in [
                (Stream::Guide, s.has_guide),
                (Stream::Sessions, s.has_sessions),
                (Stream::Commits, s.has_commits),
                (Stream::Comments, s.has_comments),
                (Stream::Files, true),
            ] {
                let tab_id = stream_tab_spec(t).id;
                let want = available && !closed.contains(&tab_id);
                sync_stream_tab(cx, &dock, t, want);
            }
            // The view buttons mirror stream availability, like the tabs.
            pane.button(cx, ids!(guide_button))
                .set_visible(cx, s.has_guide);
            pane.button(cx, ids!(sessions_button))
                .set_visible(cx, s.has_sessions);
            pane.button(cx, ids!(commits_button))
                .set_visible(cx, s.has_commits);
            pane.button(cx, ids!(comments_button))
                .set_visible(cx, s.has_comments);
            // The default tab for this load: the guide when one exists, the
            // plain diff otherwise (build_review set d.tab accordingly).
            dock.select_tab(cx, stream_tab_spec(s.tab).id);
            // Seen state is content-addressed, so a fresh load can already be
            // part-reviewed: re-tally the progress bar for this range.
            if let Some(mut p) = pane.borrow_mut::<ReviewPane>() {
                p.refresh_progress(cx);
            }
            dirty = true;
        }
        if dirty {
            self.window.redraw(cx);
        }
        dirty
    }

    /// One tick of the ~1s poll: pick up guides another process `submit`ted
    /// and review-state (comments) another process wrote. Skipped mid-load —
    /// and the fingerprints are only advanced when a tick actually runs, so a
    /// submit landing during a load is processed by the next tick, not
    /// dropped.
    fn poll_tick(&mut self, cx: &mut Cx) {
        // Cache the open buffers' documents, so the comment anchors and any
        // unsaved typing in them outlive the process. Snapshot-per-buffer, so
        // it rides the poll's 1s cadence and only when a buffer actually moved.
        {
            // Every mutation worth caching moves one of three things: a buffer
            // becomes a document, it is edited, or it takes hold of a thread.
            // `rev` alone misses the first and the last, which is what a
            // freshly opened file with comments on it does.
            let rev: u64 = self.state.read(|d| {
                d.blobs
                    .iter()
                    .filter(|b| b.doc.is_some())
                    .map(|b| 1 + b.edit_rev + b.held.len() as u64)
                    .sum()
            });
            if self.cached_rev != rev {
                self.cached_rev = rev;
                review().send(ReviewCmd::CacheBuffers(self.state.clone()));
            }
        }
        // Persist the dock layout when it changed (tab moves/selection,
        // splits, panel resizes) — the poll's 1s cadence caps file writes.
        {
            let pane = self.pane(cx);
            let dock = pane.dock(cx, ids!(dock));
            if dock.check_and_clear_need_save()
                && let Some(git_dir) = self.layout_git_dir.clone()
            {
                let restores = pane
                    .borrow::<ReviewPane>()
                    .map(|p| (p.bottom_restore, p.sidebar_restore))
                    .unwrap_or((0.0, 0.0));
                if let Some(items) = dock.clone_state() {
                    review().send(ReviewCmd::SaveLayout {
                        git_dir,
                        dock_items: items,
                        restores,
                    });
                }
            }
        }
        let snap = self.state.snapshot();
        if snap.loading {
            return;
        }
        let Some(git_dir) = snap.git_dir.clone() else {
            return;
        };

        // A WORKTREE review's diff moves under it as the user edits or
        // stages. The probe reads the index and stats the tree, so the
        // service runs it and reports back; a change reloads silently —
        // content-addressed seen state carries over untouched. This is also
        // how "stage seen hunks" refreshes the pane.
        match &snap.workdir {
            Some(workdir) => review().send(ReviewCmd::WorktreeProbe {
                window: self.state.id,
                workdir: workdir.clone(),
                last: self.worktree_fp,
            }),
            None => self.worktree_fp = 0,
        }

        // Everything other writers commit — a `submit`ted guide, a CLI-added
        // comment — is the service's to probe: it owns the store, and both
        // probes are I/O. This tick only has to ask. An explicit `--guide`
        // wins over stored guides, so the guide half is skipped while one is
        // set; a worktree review keys its guides by the zero-oid convention.
        let key = if snap.workdir.is_some() {
            Some(store::guide_key(snap.merge_base_oid, snap.head_oid))
        } else {
            snap.merge_base_oid.zip(snap.head_oid)
        };
        let guide = match (&snap.guide_path, key) {
            (None, Some((merge_base, head))) => Some(service::GuideProbe {
                merge_base,
                head,
                applied_at: snap.applied_guide_at,
            }),
            _ => None,
        };
        review().send(ReviewCmd::Poll {
            git_dir,
            guide,
            window: self.state.id,
        });
    }

    /// The worktree moved under a WORKTREE review: reload at the same range.
    fn worktree_changed(&mut self, cx: &mut Cx, fp: u64) {
        let had = std::mem::replace(&mut self.worktree_fp, fp);
        let Some((target, guide)) = ({
            let d = self.state.snapshot();
            // The first probe only baselines; a load in flight owns the doc.
            (had != 0 && !d.loading).then(|| (d.target(), d.guide_path.clone()))
        }) else {
            return;
        };
        if let Some(mut pane) = self.pane(cx).borrow_mut::<ReviewPane>() {
            pane.start_load(cx, target, guide, None);
        }
    }

    /// A guide was submitted for the open range: reload, which is the one
    /// path that applies one (the loader re-resolves the newest itself, and
    /// `has_guide` flipping true is what makes the Guide tab appear).
    fn guide_ready(&mut self, cx: &mut Cx) {
        let Some(target) = ({
            let d = self.state.snapshot();
            // A load already in flight re-resolves the newest guide itself.
            (!d.loading).then(|| d.target())
        }) else {
            return;
        };
        if let Some(mut pane) = self.pane(cx).borrow_mut::<ReviewPane>() {
            pane.start_load(cx, target, None, Some("guide submitted — loading…"));
        }
    }

    /// The service published a new review state (a tick, a comment, someone
    /// else's write): splice the comments into this window's document,
    /// re-tally the progress bar, redraw. Windows on another repo no-op.
    fn review_state_changed(&mut self, cx: &mut Cx) {
        let mut has_comments = None;
        {
            let git_dir = self.state.read(|d| d.git_dir.clone());
            let state = review_state(git_dir.as_deref()).load();
            // A publish for a repo this window no longer shows (the user
            // switched mid-flight) is not ours to splice.
            if state.git_dir.is_some() && state.git_dir != git_dir {
                return;
            }
            if self.spliced_rev != state.comments_rev {
                self.spliced_rev = state.comments_rev;
                has_comments = Some(self.state.with(|doc| {
                    resplice_comments(doc, &state.comments);
                    doc.has_comments
                }));
            }
        }
        // Comments come and go without a load (the CLI adds one, a thread is
        // deleted): reconcile the Comments tab and its view button here, the
        // same way a landed load reconciles the other streams.
        if let Some(has) = has_comments {
            let pane = self.pane(cx);
            pane.button(cx, ids!(comments_button)).set_visible(cx, has);
            let closed = pane
                .borrow::<ReviewPane>()
                .map(|p| p.user_closed.clone())
                .unwrap_or_default();
            let dock = pane.dock(cx, ids!(dock));
            let tab_id = stream_tab_spec(Stream::Comments).id;
            let want = has && !closed.contains(&tab_id);
            sync_stream_tab(cx, &dock, Stream::Comments, want);
        }
        if let Some(mut p) = self.pane(cx).borrow_mut::<ReviewPane>() {
            p.refresh_progress(cx);
        }
        self.window.redraw(cx);
    }
}

/// Build `mod.app_theme` — the DSL palette the chrome reads — from the active
/// Rust theme (`theme.rs`). Colors splice in as `pod_vec4f`, valid as plain
/// props and shader uniforms alike. Runs before the app's own `script_mod!`
/// block AND terminal_view's (both read `mod.app_theme.*`), and re-runs on every
/// `request_live_edit`, so a theme switch re-bakes the whole chrome.
fn install_app_theme(vm: &mut ScriptVm) {
    let t = theme::active_theme();
    let a = |c: Vec4f, w: f32| Vec4f { w, ..c };
    script_eval!(vm, {
        mod.app_theme = {
            color_bg: #(paint(t.background))
            // The theme's shadow colour, darker than its page so a shadow
            // reads over both the page and a card.
            color_shadow: #(paint(t.shadow))
            color_card: #(paint(t.surface))
            color_border: #(paint(t.border))
            color_border_hover: #(paint(t.border_hover))
            color_border_focus: #(paint(t.border_focus))
            color_chrome: #(paint(t.chrome))
            color_text: #(paint(t.text))
            color_dim: #(paint(t.text_muted))
            color_faint: #(paint(t.text_faint))
            color_yellow: #(paint(t.modified))
            color_accent: #(paint(t.accent))
            color_on_accent: #(paint(t.on_accent))
            color_added: #(paint(t.added))
            color_deleted: #(paint(t.deleted))
            color_checkbox_bg: #(paint(t.checkbox_bg))
            color_checkbox_hover: #(paint(t.checkbox_hover))
            color_element_hover: #(paint(t.element_hover))
            color_drag: #(a(paint(t.accent), 0.25))
            color_sel_focus: #(a(paint(t.accent), 0.5))
            // Find hits, in the "modified" hue so they never read as a
            // selection — the two are on screen together while searching.
            color_find: #(a(paint(t.modified), 0.45))
            color_sel_unfocus: #(a(paint(t.accent), 0.25))
            color_cursor: #(paint(t.terminal_cursor))
            color_cell_bg: #(paint(t.terminal_cell_bg))
        }
    });
}

/// Build `mod.app_font` — the resources the DSL TextStyles hang their font
/// members off — from the active font setting (theme.rs). Re-runs on
/// `request_live_edit`, so a font change re-bakes app-wide. Built before the
/// app + terminal_view blocks.
///
/// Only the resources are built here. A `FontFamily` assembled in this scope
/// and referenced by name applies as an empty family — every glyph disappears —
/// so the family literal stays in each TextStyle and this fills its slots.
///
/// The embedded fonts always follow the user's, so no setting can leave the app
/// without box drawing, CJK or emoji. The user's own faces occupy a fixed four
/// slots because the script block's text is compiled, not generated; a shorter
/// list repeats, which costs the shaper a duplicate miss on a glyph nobody has
/// and keeps the block readable.
fn install_app_font(vm: &mut ScriptVm) {
    let f = theme::active_font();
    let size = f.size;
    if f.paths.is_empty() {
        script_eval!(vm, {
            mod.app_font = {
                size: #(size)
                first: mod.res.crate_resource("makepad_widgets:resources/jetbrains_mono_variable.ttf")
                second: mod.res.crate_resource("makepad_widgets:resources/jetbrains_mono_variable.ttf")
                third: mod.res.crate_resource("makepad_widgets:resources/jetbrains_mono_variable.ttf")
                fourth: mod.res.crate_resource("makepad_widgets:resources/jetbrains_mono_variable.ttf")
                mono: mod.res.crate_resource("makepad_widgets:resources/jetbrains_mono_variable.ttf")
                cjk: mod.res.crate_resource("makepad_widgets:resources/LXGWWenKaiRegular.ttf")
                emoji: mod.res.crate_resource("makepad_widgets:resources/NotoColorEmoji.ttf")
            }
        });
        return;
    }
    let mut slots = f.paths.iter().cycle();
    let (a, b, c, d) = (
        slots.next().cloned().unwrap_or_default(),
        slots.next().cloned().unwrap_or_default(),
        slots.next().cloned().unwrap_or_default(),
        slots.next().cloned().unwrap_or_default(),
    );
    script_eval!(vm, {
        mod.app_font = {
            size: #(size)
            first: mod.res.file_resource(#(a))
            second: mod.res.file_resource(#(b))
            third: mod.res.file_resource(#(c))
            fourth: mod.res.file_resource(#(d))
            mono: mod.res.crate_resource("makepad_widgets:resources/jetbrains_mono_variable.ttf")
            cjk: mod.res.crate_resource("makepad_widgets:resources/LXGWWenKaiRegular.ttf")
            emoji: mod.res.crate_resource("makepad_widgets:resources/NotoColorEmoji.ttf")
        }
    });
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        install_app_theme(vm);
        install_app_font(vm);
        terminal_view::script_mod(vm);
        widgets::script_mod(vm);
        self::script_mod(vm)
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        // The pane's header band doubles as the window caption (the native
        // caption bar is hidden; macOS traffic lights float over the header's
        // left edge): any press in that band drags the window — except on the
        // repo name, the diff-picker chip, and the Load/share buttons, which
        // need their clicks.
        if let Event::WindowDragQuery(dq) = event {
            let pane = self.ui.widget(cx, ids!(pane_a));
            let r = pane.view(cx, ids!(header)).area().rect(cx);
            let buttons = [
                id!(repo_button),
                id!(range_button),
                id!(load_button),
                id!(share_button),
            ];
            let on_button = buttons
                .iter()
                .any(|id| pane.widget(cx, &[*id]).area().rect(cx).contains(dq.abs));
            if r.size.y > 0.0 && dq.abs.y >= r.pos.y && dq.abs.y <= r.pos.y + r.size.y && !on_button
            {
                dq.response.set(WindowDragQueryResponse::Caption);
            }
        }
        if let Event::MacosMenuCommand(command) = event
            && *command == live_id!(new_window)
        {
            self.open_new_window(cx);
        }
        // After `Root` has dropped the widget, so the window is gone from both.
        if let Event::WindowClosed(e) = event {
            self.retire_window(e.window_id);
        }
        self.match_event(cx, event);
        // `Root` hands every window the same scope, so the per-window one is
        // built further down, by each window's own pane.
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
