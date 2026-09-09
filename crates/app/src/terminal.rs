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
};
use makepad_widgets::{LiveId, makepad_platform::thread::SignalToUI};

pub mod colors;
pub mod keys;
pub mod mouse;

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
}

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
            }
            Event::ChildExit(_) => {
                self.exited.store(true, Ordering::Relaxed);
                self.wake();
            }
            Event::Title(title) => {
                self.report.lock().unwrap().title = Some(title);
                self.wake();
            }
            Event::ClipboardStore(ClipboardType::Clipboard, text) => {
                self.report.lock().unwrap().clipboard = Some(text);
                self.wake();
            }
            // TODO: a bell should raise a desktop notification. It is how an
            // agent says it has finished while you are looking at another
            // window, and the panel currently swallows it. Left out here
            // because the sequences that carry a message with it (OSC 9, OSC
            // 777) are not ones the library parses, and hand-rolling that is
            // more than this change should carry.
            Event::Wakeup | Event::Bell => self.wake(),
            _ => {}
        }
    }
}

impl Proxy {
    fn wake(&self) {
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

/// Sessions whose term changed since the UI last looked.
fn dirty() -> &'static Mutex<HashSet<Session>> {
    static D: OnceLock<Mutex<HashSet<Session>>> = OnceLock::new();
    D.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Whether this dock tab has a live session — also how the view recognizes
/// which terminal tab it is nested under.
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
    sessions()
        .lock()
        .unwrap()
        .get(&session)
        .map(|s| s.term.clone())
}

/// What this session's program reported since the last look, cleared as it is
/// taken.
pub fn take_report(session: Session) -> Report {
    let report = sessions()
        .lock()
        .unwrap()
        .get(&session)
        .map(|shell| shell.report.clone());
    report
        .map(|report| std::mem::take(&mut *report.lock().unwrap()))
        .unwrap_or_default()
}

/// Spawn the user's login shell in `cwd` for this tab, unless one is already
/// running. Called from the terminal toggle, a tab press or `+`, never from the
/// draw path, so a failing shell can't respawn in a loop. `env` goes into the
/// shell: the app passes the window's identity and open range, so a CLI (or an
/// agent) started here defaults to the diff on screen.
pub fn open(session: Session, cwd: &Path, env: &[(&str, &str)]) {
    let mut guard = sessions().lock().unwrap();
    if guard.contains_key(&session) {
        return;
    }

    let mut environment: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    // NOTE: alacritty's `tty::setup_env` sets these process-wide, which a GUI
    // app with other children must not do. Per child instead — and claim the
    // terminfo entry every system has, since the `alacritty` one ships with
    // alacritty, not with us.
    environment.insert("TERM".to_string(), "xterm-256color".to_string());
    environment.insert("COLORTERM".to_string(), "truecolor".to_string());
    let fixture = crate::dev_hooks::var("CONCATS_APP_TERM_SCRIPT").ok();
    // NOTE: PTY and visual tests must not run personal shell startup files.
    if cfg!(test) || fixture.is_some() {
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
        shell: if cfg!(test) || fixture.is_some() {
            Some(tty::Shell::new(
                "/bin/sh".to_string(),
                vec![fixture.unwrap_or_else(|| "-i".to_string())],
            ))
        } else {
            None
        },
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
pub fn close(session: Session) {
    let Some(shell) = sessions().lock().unwrap().remove(&session) else {
        return;
    };
    let _ = shell.sender.send(Msg::Shutdown);
}

/// Keystrokes/paste from the view, already encoded to terminal bytes.
pub fn input(session: Session, data: Vec<u8>) {
    if data.is_empty() {
        return;
    }
    let Some(sender) = sessions()
        .lock()
        .unwrap()
        .get(&session)
        .map(|s| s.sender.clone())
    else {
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
        open(session, Path::new("."), &[]);
        assert!(is_open(session), "shell failed to spawn");
        input(session, b"printf 'concats_%s\\n' ok\r".to_vec());

        assert!(until(|| screen(session).contains("concats_ok")));
        close(session);
    }

    /// The app exports the window's range into the shell, and now shares that
    /// map with `TERM`/`COLORTERM`. A command must still see it.
    #[test]
    fn env_reaches_the_shell() {
        let session = session(0xC0FF_EE02);
        open(
            session,
            Path::new("."),
            &[("CONCATS_APP_BASE", concats_diff::load::INDEX_REV)],
        );
        assert!(is_open(session), "shell failed to spawn");
        // `base=$VAR`: the echoed command line shows the unexpanded form, so
        // only the shell's own expansion can produce the needle.
        input(session, b"echo base=$CONCATS_APP_BASE\r".to_vec());

        assert!(until(|| screen(session).contains("base=INDEX")));
        close(session);
    }

    /// NOTE: `stty` reads the PTY size directly; `tput` can instead report
    /// the shell’s cached LINES/COLUMNS values during startup.
    #[test]
    fn a_resize_reaches_the_term_and_the_shell() {
        let session = session(0xC0FF_EE03);
        open(session, Path::new("."), &[]);
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

    /// A shell that exits is reported once and dropped, so pressing its tab
    /// spawns a new one instead of talking to a corpse.
    #[test]
    fn an_exited_shell_is_reaped() {
        let session = session(0xC0FF_EE04);
        open(session, Path::new("."), &[]);
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
        open(session, Path::new("."), &[]);
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
        open(session, Path::new("."), &[]);
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
