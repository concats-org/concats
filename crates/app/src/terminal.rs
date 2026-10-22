//! The in-app terminal: one shell session per dock tab.
//!
//! A session is an `alacritty_terminal` [`Term`] behind a [`FairMutex`], plus
//! the `EventLoop` thread that owns its PTY. That thread reads the shell,
//! parses into the term and wakes the UI; the UI thread never parses. It locks
//! the term in the draw path and reads the grid.
//!
//! Lock order runs one way: the session map is never held while a term is
//! locked. The parser calls back into [`Proxy`] *with* the term locked, so a UI
//! thread holding the map and waiting on a term would deadlock against it.
//! Every accessor here clones what it needs out of the map and drops the guard
//! before touching a term.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use alacritty_terminal::{
    event::{Event, EventListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg},
    grid::Dimensions,
    sync::FairMutex,
    term::{ClipboardType, Config, Term},
    tty,
    vte::ansi::Rgb,
};
use makepad_widgets::{LiveId, makepad_platform::thread::SignalToUI};

pub mod blink;
pub mod colors;
pub mod environment;
pub mod keys;
pub mod mouse;
pub mod queries;

/// The term a view renders, shared with the thread parsing into it.
pub type SharedTerm = Arc<FairMutex<Term<Proxy>>>;

/// Which shell a view is showing: the window it belongs to, and the dock tab
/// inside it. Two windows have the same tab ids — both call their first
/// terminal `terminal_tab` — so a tab alone does not name a session, and
/// keying by one would have the second window adopt the first one's shell.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Session {
    pub window: LiveId,
    pub tab: LiveId,
}

/// What the program running in a session said about itself, waiting for the UI
/// to pick it up. Written on the parser thread and drained on the UI thread,
/// so it hangs beside the session rather than inside it — see the lock order
/// above.
#[derive(Default)]
pub struct Report {
    /// OSC 0/2: what the program calls itself.
    ///
    /// NOTE: the matching reset is ignored. The tab's own name is the dock's,
    /// not ours to reconstruct, and keeping the last title is what other
    /// terminals do when a program exits without clearing it.
    pub title: Option<String>,
    /// OSC 52: text the program asked to put on the clipboard.
    pub clipboard: Option<String>,
    /// OSC 4/10/11/12 queries: which palette slot, and how the program wants
    /// the answer written. Answered on the UI thread, where the term can be
    /// locked and the theme read.
    pub color_queries: Vec<(usize, ColorReply)>,
    /// CSI 14t: the program asking how big the text area is, in pixels.
    pub size_queries: Vec<SizeReply>,
}

/// How a program wants the text area's size reported.
pub type SizeReply = Arc<dyn Fn(WindowSize) -> String + Sync + Send>;

/// How a program wants a colour query answered, from alacritty's parser.
pub type ColorReply = Arc<dyn Fn(Rgb) -> String + Sync + Send>;

/// The geometry both halves need: the term counts cells, the PTY also carries
/// the pixel size, which full-screen apps ask for to lay themselves out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Size {
    pub columns: u16,
    pub screen_lines: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        usize::from(self.screen_lines)
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.screen_lines)
    }

    fn columns(&self) -> usize {
        usize::from(self.columns)
    }
}

impl From<Size> for WindowSize {
    fn from(size: Size) -> Self {
        Self {
            num_lines: size.screen_lines,
            num_cols: size.columns,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        }
    }
}

/// What the parser thread reports back. Every call arrives on the `EventLoop`
/// thread with the term locked, so nothing here may reach for the session map.
#[derive(Clone)]
pub struct Proxy {
    session: Session,
    /// Filled right after the loop is built — `EventLoop::new` wants the proxy
    /// before there is a channel to hand it.
    sender: Arc<OnceLock<EventLoopSender>>,
    exited: Arc<AtomicBool>,
    report: Arc<Mutex<Report>>,
}

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        match event {
            // The term answered a query (device attributes, cursor report, a
            // colour) and wants the bytes on the PTY. Straight down the loop's
            // own channel — see the lock order note above.
            Event::PtyWrite(text) => {
                if let Some(sender) = self.sender.get() {
                    let _ = sender.send(Msg::Input(text.into_bytes().into()));
                }
                return;
            }
            Event::ChildExit(_) => self.exited.store(true, Ordering::Relaxed),
            Event::Title(title) => self.report.lock().unwrap().title = Some(title),
            Event::ClipboardStore(ClipboardType::Clipboard, text) => {
                self.report.lock().unwrap().clipboard = Some(text);
            }
            Event::ColorRequest(index, reply) => {
                self.report
                    .lock()
                    .unwrap()
                    .color_queries
                    .push((index, reply));
            }
            Event::TextAreaSizeRequest(reply) => {
                self.report.lock().unwrap().size_queries.push(reply);
            }
            // TODO: a bell should raise a desktop notification. It is how an
            // agent says it has finished while you are looking at another
            // window, and the panel currently swallows it. Left out here
            // because the sequences that carry a message with it (OSC 9, OSC
            // 777) are not ones the library parses, and hand-rolling that is
            // more than this change should carry.
            // The view reads whether the cursor blinks off the term itself;
            // a change only has to redraw it.
            Event::Wakeup | Event::Bell | Event::CursorBlinkingChange => {}
            _ => return,
        }
        // Every event that gets here changed what the view shows.
        dirty().lock().unwrap().insert(self.session);
        SignalToUI::set_ui_signal();
    }
}

struct Shell {
    term: SharedTerm,
    sender: EventLoopSender,
    size: Size,
    exited: Arc<AtomicBool>,
    report: Arc<Mutex<Report>>,
}

fn sessions() -> &'static Mutex<HashMap<Session, Shell>> {
    static S: OnceLock<Mutex<HashMap<Session, Shell>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The size this tab's shell was last given, or None while it has no live
/// session.
pub fn size(session: Session) -> Option<Size> {
    shell(session, |s| s.size)
}

/// What `read` takes out of this session's shell. The map is unlocked again by
/// the time the caller holds it, so whatever it locks next is not under it.
fn shell<R>(session: Session, read: impl FnOnce(&Shell) -> R) -> Option<R> {
    sessions().lock().unwrap().get(&session).map(read)
}

/// Sessions whose term changed since the UI last looked.
fn dirty() -> &'static Mutex<HashSet<Session>> {
    static D: OnceLock<Mutex<HashSet<Session>>> = OnceLock::new();
    D.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Whether this dock tab has a live session — also how the view recognizes
/// which terminal tab it is nested under.
// NOTE: the liveness check every PTY test starts from; its fan-in is the tests.
// coland-ignore: @coland/high_call_degree
pub fn is_open(session: Session) -> bool {
    sessions().lock().unwrap().contains_key(&session)
}

/// This window's live session count (its toggle spawns the first shell only
/// when 0 — another window's terminals are not this window's business).
pub fn count(window: LiveId) -> usize {
    sessions()
        .lock()
        .unwrap()
        .keys()
        .filter(|s| s.window == window)
        .count()
}

/// The term this tab renders, or None while it has no live session. The `Arc`
/// leaves the map behind: lock it only after this returns.
pub fn term(session: Session) -> Option<SharedTerm> {
    shell(session, |s| s.term.clone())
}

/// What this session's program reported since the last look, cleared as it is
/// taken.
pub fn take_report(session: Session) -> Report {
    shell(session, |s| s.report.clone())
        .map(|report| std::mem::take(&mut *report.lock().unwrap()))
        .unwrap_or_default()
}

/// Spawn the user's login shell in `cwd` for this tab, unless one is already
/// running. Called from the terminal toggle, a tab press or `+`, never from the
/// draw path, so a failing shell can't respawn in a loop.
// NOTE: the one way a shell starts, PTY to running session in order (the tests are most of
// its fan-in); its clones are shared handles.
// coland-ignore: @coland/clone_heavy_function, @coland/high_fan_in_complex_function
// coland-ignore: @coland/cross_file_call_coupling, @coland/high_call_degree
// coland-ignore: @coland/function_too_large
pub fn open(session: Session, cwd: &Path) {
    let mut guard = sessions().lock().unwrap();
    if guard.contains_key(&session) {
        return;
    }

    let fixture = crate::dev_hooks::var("CONCATS_APP_TERM_SCRIPT");
    // NOTE: PTY and visual tests must not run personal shell startup files.
    let hermetic = cfg!(test) || fixture.is_some();
    let script = hermetic.then(|| {
        fixture.map_or_else(
            || vec!["-i".to_string()],
            |script| vec!["-c".to_string(), script],
        )
    });
    // NOTE: alacritty's `tty::setup_env` sets these process-wide, which a GUI
    // app with other children must not do. Per child instead.
    let (mut environment, shell) = environment::shell_from_env(script);
    if hermetic {
        environment.extend([
            ("ENV".to_string(), "/dev/null".to_string()),
            ("PS1".to_string(), String::new()),
        ]);
    }

    // A seed: the view resizes to its real geometry on the first draw.
    let size = Size {
        columns: 80,
        screen_lines: 24,
        cell_width: 8,
        cell_height: 16,
    };
    let options = tty::Options {
        shell,
        working_directory: Some(cwd.to_path_buf()),
        drain_on_exit: false,
        env: environment,
    };
    let pty = match tty::new(&options, size.into(), session.window.0) {
        Ok(pty) => pty,
        Err(err) => {
            eprintln!(
                "terminal: failed to spawn shell in {}: {err}",
                cwd.display()
            );
            return;
        }
    };

    let config = Config {
        // Agents print a lot. Ten thousand lines is a short memory for one
        // build log, let alone a session.
        scrolling_history: 100_000,
        // What shift+enter and every other modified key ride on. The encoder
        // in `keys` speaks it, so the terminal may say so.
        kitty_keyboard: true,
        ..Config::default()
    };
    let exited = Arc::new(AtomicBool::new(false));
    let report = Arc::new(Mutex::new(Report::default()));
    let proxy = Proxy {
        session,
        sender: Arc::new(OnceLock::new()),
        exited: exited.clone(),
        report: report.clone(),
    };
    let term = Arc::new(FairMutex::new(Term::new(config, &size, proxy.clone())));
    let event_loop = match EventLoop::new(term.clone(), proxy.clone(), pty, false, false) {
        Ok(event_loop) => event_loop,
        Err(err) => {
            eprintln!("terminal: failed to start the pty loop: {err}");
            return;
        }
    };
    let sender = event_loop.channel();
    let _ = proxy.sender.set(sender.clone());
    event_loop.spawn();

    guard.insert(
        session,
        Shell {
            term,
            sender,
            size,
            exited,
            report,
        },
    );
}

/// End a session: the loop thread shuts down its PTY and the child shell gets
/// hung up on.
// NOTE: the teardown every PTY test ends with; its fan-in is the tests.
// coland-ignore: @coland/high_call_degree
pub fn close(session: Session) {
    let Some(shell) = sessions().lock().unwrap().remove(&session) else {
        return;
    };
    let _ = shell.sender.send(Msg::Shutdown);
}

/// Keystrokes/paste from the view, already encoded to terminal bytes.
// NOTE: every byte the view sends a shell goes through it.
// coland-ignore: @coland/high_call_degree
pub fn input(session: Session, data: Vec<u8>) {
    if data.is_empty() {
        return;
    }
    let Some(sender) = shell(session, |s| s.sender.clone()) else {
        return;
    };
    let _ = sender.send(Msg::Input(data.into()));
}

/// The view's geometry, from the draw path on every frame. A no-op unless it
/// changed, so the shell only hears about real resizes.
pub fn resize(session: Session, size: Size) {
    let mut guard = sessions().lock().unwrap();
    let Some(shell) = guard.get_mut(&session) else {
        return;
    };
    if shell.size == size {
        return;
    }
    shell.size = size;
    let (term, sender) = (shell.term.clone(), shell.sender.clone());
    drop(guard);

    term.lock().resize(size);
    let _ = sender.send(Msg::Resize(size.into()));
}

/// The sessions whose terms changed since the last call — redraw those tabs.
/// Sessions whose shell exited are dropped here and reported one last time, so
/// the tab redraws empty; pressing it spawns a new shell.
pub fn take_dirty() -> Vec<Session> {
    let dirty: Vec<Session> = dirty().lock().unwrap().drain().collect();
    let mut guard = sessions().lock().unwrap();
    for session in &dirty {
        if guard
            .get(session)
            .is_some_and(|shell| shell.exited.load(Ordering::Relaxed))
        {
            guard.remove(session);
        }
    }
    dirty
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use alacritty_terminal::{term::TermMode, vte::ansi::NamedColor};
    use makepad_widgets::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    /// Poll the way the UI looks between wakeups. Ten seconds is far longer
    /// than any of these needs, and short enough that a hung shell fails its
    /// own test rather than the suite.
    fn until(mut ready: impl FnMut() -> bool) -> bool {
        for _ in 0..40 {
            if ready() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        false
    }

    /// Everything on screen, as one string.
    fn screen(session: Session) -> String {
        term(session)
            .map(|shared| {
                shared
                    .lock()
                    .grid()
                    .display_iter()
                    .map(|cell| cell.c)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A tab of its own per test, so they can run at once.
    fn session(tab: u64) -> Session {
        Session {
            window: LiveId(0),
            tab: LiveId(tab),
        }
    }

    /// Everything under the renderer, end to end: spawn a shell, run a
    /// command, and find its output in the grid.
    #[test]
    fn shell_output_reaches_the_grid() {
        let session = session(0xC0FF_EE01);
        open(session, Path::new("."));
        assert!(is_open(session), "shell failed to spawn");
        input(session, b"printf 'concats_%s\\n' ok\r".to_vec());

        assert!(until(|| screen(session).contains("concats_ok")));
        close(session);
    }

    /// OSC 11 asks for the background; the query reaches the report with the
    /// slot it names and the shape its answer takes.
    #[test]
    fn a_colour_query_reaches_the_report() {
        let session = session(0xC0FF_EE0B);
        open(session, Path::new("."));
        assert!(is_open(session), "shell failed to spawn");
        input(session, b"printf '\\033]11;?\\033\\\\'\r".to_vec());
        let mut queries = Vec::new();
        assert!(until(|| {
            queries.extend(take_report(session).color_queries);
            !queries.is_empty()
        }));
        let (index, reply) = &queries[0];
        assert_eq!(*index, NamedColor::Background as usize);
        assert!(
            reply(Rgb {
                r: 0x1e,
                g: 0x1f,
                b: 0x22
            })
            .contains("rgb:1e1e/1f1f/2222")
        );
        close(session);
    }

    /// CSI 14t asks for the text area in pixels; the answer is the session's
    /// own size.
    #[test]
    fn a_size_query_reaches_the_report() {
        let session = session(0xC0FF_EE0C);
        open(session, Path::new("."));
        assert!(is_open(session), "shell failed to spawn");
        input(session, b"printf '\\033[14t'\r".to_vec());
        let mut queries = Vec::new();
        assert!(until(|| {
            queries.extend(take_report(session).size_queries);
            !queries.is_empty()
        }));
        let size = WindowSize {
            num_lines: 24,
            num_cols: 80,
            cell_width: 8,
            cell_height: 16,
        };
        assert_eq!(queries[0](size), "\x1b[4;384;640t");
        close(session);
    }

    #[test]
    fn the_shell_knows_it_runs_in_concats() {
        let session = session(0xC0FF_EE0A);
        open(session, Path::new("."));
        assert!(is_open(session), "shell failed to spawn");
        input(
            session,
            b"printf 'program=%s\\n' \"$TERM_PROGRAM\"\r".to_vec(),
        );
        assert!(
            until(|| screen(session).contains("program=concats")),
            "{}",
            screen(session)
        );
        close(session);
    }

    /// NOTE: `stty` reads the PTY size directly; `tput` can instead report
    /// the shell’s cached LINES/COLUMNS values during startup.
    #[test]
    fn a_resize_reaches_the_term_and_the_shell() {
        let session = session(0xC0FF_EE03);
        open(session, Path::new("."));
        assert!(is_open(session), "shell failed to spawn");
        resize(
            session,
            Size {
                columns: 97,
                screen_lines: 31,
                cell_width: 8,
                cell_height: 16,
            },
        );

        let shared = term(session).expect("session has a term");
        {
            let term = shared.lock();
            assert_eq!(term.columns(), 97);
            assert_eq!(term.screen_lines(), 31);
        }
        input(session, b"printf 'size='; stty size\r".to_vec());
        assert!(
            until(|| screen(session).contains("size=31 97")),
            "PTY size did not reach the shell: {}",
            screen(session)
        );
        close(session);
    }

    #[test]
    fn full_screen_program_returns_when_sent_the_view_keyboard_bytes() {
        let session = session(0xC0FF_EE09);
        open(session, Path::new("."));
        assert!(is_open(session), "shell failed to spawn");
        input(
            session,
            b"printf '\\033[?1049h'; IFS= read -r line; printf '\\033[?1049lreturned=%s\\n' \"$line\"\r"
                .to_vec(),
        );
        assert!(until(|| {
            term(session).is_some_and(|term| term.lock().mode().contains(TermMode::ALT_SCREEN))
        }));

        let mode = *term(session).expect("session has a term").lock().mode();
        // `:` and `q` may arrive as text, enter has to be encoded.
        for (key_code, shift, text) in [
            (KeyCode::Semicolon, true, Some(b":".as_slice())),
            (KeyCode::KeyQ, false, Some(b"q".as_slice())),
            (KeyCode::ReturnKey, false, None),
        ] {
            let key = KeyEvent {
                key_code,
                modifiers: KeyModifiers {
                    shift,
                    ..Default::default()
                },
                ..Default::default()
            };
            let bytes = crate::terminal::keys::encode(&key, true, mode)
                .or_else(|| text.map(<[u8]>::to_vec))
                .expect("enter is encoded");
            input(session, bytes);
        }

        assert!(until(|| {
            term(session).is_some_and(|term| !term.lock().mode().contains(TermMode::ALT_SCREEN))
        }));
        assert!(screen(session).contains("returned=:q"));
        close(session);
    }

    /// A shell that exits is reported once and dropped, so pressing its tab
    /// spawns a new one instead of talking to a corpse.
    #[test]
    fn an_exited_shell_is_reaped() {
        let session = session(0xC0FF_EE04);
        open(session, Path::new("."));
        assert!(is_open(session), "shell failed to spawn");
        input(session, b"exit\r".to_vec());

        // Reaping happens where the UI picks up its redraws.
        assert!(until(|| {
            take_dirty();
            !is_open(session)
        }));
    }

    /// OSC 52: what a program asks to put on the clipboard reaches the UI.
    /// Nothing in a shell's own startup emits this, so the report is ours.
    #[test]
    fn a_copy_request_reaches_the_report() {
        let session = session(0xC0FF_EE05);
        open(session, Path::new("."));
        assert!(is_open(session), "shell failed to spawn");
        // base64 of "concats_copied", which the terminal decodes.
        input(
            session,
            b"printf '\\033]52;c;Y29uY2F0c19jb3BpZWQ=\\007'\r".to_vec(),
        );

        assert!(until(
            || take_report(session).clipboard.as_deref() == Some("concats_copied")
        ));
        close(session);
    }

    /// A flood neither loses its tail nor grows without end: the scrollback
    /// keeps what [`open`] configured and no more.
    #[test]
    fn a_flood_keeps_its_tail_and_caps_the_scrollback() {
        let session = session(0xC0FF_EE06);
        open(session, Path::new("."));
        assert!(is_open(session), "shell failed to spawn");
        // The echoed command line already holds the count, so the needle has
        // to be something only running it can print.
        input(session, b"seq 1 200000; echo flood=$((21*2))\r".to_vec());

        assert!(until(|| screen(session).contains("flood=42")));
        let shared = term(session).expect("session has a term");
        let history = shared.lock().history_size();
        assert!(
            history > 10_000,
            "the scrollback is still the library default: {history}"
        );
        assert!(
            history <= 100_000,
            "the scrollback outgrew what was asked for: {history}"
        );
        close(session);
    }
}
