//! The in-app terminal: shell sessions, single-process.
//!
//! makepad-studio splits this across a hub process (PTY thread, `Terminal`
//! emulation model and framebuffer building, in
//! `studio/hub/src/terminal_manager.rs` and `dispatch.rs`) and a dumb renderer
//! widget. Here the hub side lives in the app. Per session, a PTY I/O thread
//! pumps bytes over an mpsc channel and wakes the UI with `SignalToUI`; the UI
//! thread owns the `Terminal` models, feeds them in [`drain`], and rebuilds the
//! [`TerminalFramebuffer`] snapshot the copied `DesktopTerminalView` renders.
//! Sessions are keyed by window and dock tab, where studio keys by a
//! String path; the copied view still speaks String paths, and
//! [`tab_path`]/[`tab_from_path`] bridge the two.

use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{
        mpsc::{self, Receiver, Sender, TryRecvError},
        Mutex, OnceLock,
    },
    time::Duration,
};

use makepad_terminal_core::{Pty, StyleFlags, Terminal};
use makepad_widgets::{makepad_platform::thread::SignalToUI, LiveId};

/// One rendered viewport snapshot. Copied from makepad-studio's wire protocol
/// (`platform/studio/src/hub_protocol.rs`) rather than pulling in the whole
/// protocol crate; the serde derives are gone because this never leaves the
/// process. Kept field for field, even the ones the view never reads, so the
/// copied view diffs cleanly against upstream.
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct TerminalFramebuffer {
    pub frame_id: u64,
    pub cols: u16,
    pub rows: u16,
    pub top_row: usize,
    pub total_lines: usize,
    pub cursor_col: u16,
    pub cursor_row: i32,
    pub cursor_visible: bool,
    pub default_fg_rgb: u32,
    pub default_bg_rgb: u32,
    pub bracketed_paste: bool,
    pub cursor_keys_application_mode: bool,
    pub is_tui: bool,
    // Tight binary payload, row-major:
    // [codepoint_u32_le, fg_r, fg_g, fg_b, bg_r, bg_g, bg_b] per cell.
    pub cells: Vec<u8>,
}

enum Control {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

enum Output {
    Data(Vec<u8>),
    /// The PTY was actually resized — only now may the emulation model follow,
    /// so it never runs ahead of what the shell was told.
    Resized {
        cols: u16,
        rows: u16,
    },
    Exited,
}

pub struct Shell {
    terminal: Terminal,
    control_tx: Sender<Control>,
    output_rx: Receiver<Output>,
    /// The snapshot the view renders; rebuilt on output and viewport changes.
    frame: Option<TerminalFramebuffer>,
    // Viewport requested by the view (cols × view_rows rendered, pty_rows is
    // the shell's window height) …
    cols: u16,
    view_rows: u16,
    pty_rows: u16,
    // … and the size the emulation model currently has.
    applied_cols: u16,
    applied_rows: u16,
    top_row: usize,
    /// Follow output: new lines keep the viewport glued to the bottom.
    anchor_bottom: bool,
    frame_id: u64,
}

/// Which shell a view is showing: the window it belongs to, and the dock tab
/// inside it. Two windows have the same tab ids — both call their first
/// terminal `terminal_tab` — so a tab alone does not name a session, and
/// keying by one would have the second window adopt the first one's shell.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Session {
    pub window: LiveId,
    pub tab: LiveId,
}

fn sessions() -> &'static Mutex<HashMap<Session, Shell>> {
    static S: OnceLock<Mutex<HashMap<Session, Shell>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
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

/// The frame a tab's view renders, or None while it has no live session.
pub fn frame_of(session: Session) -> Option<TerminalFramebuffer> {
    sessions()
        .lock()
        .unwrap()
        .get(&session)
        .and_then(|s| s.frame.clone())
}

/// The copied view still speaks studio's String "path"; ours encodes the
/// window and the dock tab in it.
pub fn tab_path(session: Session) -> String {
    format!("{}.{}", session.window.0, session.tab.0)
}

pub fn tab_from_path(path: &str) -> Option<Session> {
    let (window, tab) = path.split_once('.')?;
    Some(Session {
        window: LiveId(window.parse().ok()?),
        tab: LiveId(tab.parse().ok()?),
    })
}

/// Paint the emulator from a theme: the default fg/bg (so the panel fuses with
/// the chrome) and the 16 ANSI colours. Reused on a live theme switch. `Rgb`
/// isn't exported from terminal-core, so we set the byte fields directly.
fn apply_theme(terminal: &mut Terminal, theme: &crate::theme::Theme) {
    let to_u8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    terminal.default_bg.r = to_u8(theme.terminal_bg.r);
    terminal.default_bg.g = to_u8(theme.terminal_bg.g);
    terminal.default_bg.b = to_u8(theme.terminal_bg.b);
    terminal.default_fg.r = to_u8(theme.terminal_fg.r);
    terminal.default_fg.g = to_u8(theme.terminal_fg.g);
    terminal.default_fg.b = to_u8(theme.terminal_fg.b);
    for (i, c) in theme.ansi.iter().enumerate() {
        terminal.palette.colors[i].r = to_u8(c.r);
        terminal.palette.colors[i].g = to_u8(c.g);
        terminal.palette.colors[i].b = to_u8(c.b);
    }
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
    let (cols, rows) = (120u16, 32u16);
    let pty = match Pty::spawn(cols, rows, None, env, Some(cwd)) {
        Ok(pty) => pty,
        Err(err) => {
            eprintln!(
                "terminal: failed to spawn shell in {}: {err}",
                cwd.display()
            );
            return;
        }
    };
    let (control_tx, control_rx) = mpsc::channel();
    let (output_tx, output_rx) = mpsc::channel();
    std::thread::spawn(move || run_terminal_loop(pty, control_rx, output_tx));

    let mut terminal = Terminal::new(cols as usize, rows as usize);
    apply_theme(&mut terminal, &crate::theme::active_theme());

    guard.insert(
        session,
        Shell {
            terminal,
            control_tx,
            output_rx,
            frame: None,
            cols,
            view_rows: rows,
            pty_rows: rows,
            applied_cols: cols,
            applied_rows: rows,
            top_row: usize::MAX,
            anchor_bottom: true,
            frame_id: 0,
        },
    );
}

/// End a session: dropping it closes the control channel, its PTY thread
/// exits, and the child shell gets hung up on.
pub fn close(session: Session) {
    sessions().lock().unwrap().remove(&session);
}

/// Pump everything the PTY threads queued into the emulation models. Called
/// on `Event::Signal`; returns the tabs whose frames changed (redraw those).
/// A dead shell tears its session down — pressing its tab respawns it.
pub fn drain() -> Vec<Session> {
    let mut guard = sessions().lock().unwrap();
    let mut dirty = Vec::new();
    let mut dead = Vec::new();
    for (tab, s) in guard.iter_mut() {
        let mut changed = false;
        let mut grew = false;
        let mut exited = false;
        loop {
            match s.output_rx.try_recv() {
                Ok(Output::Data(data)) => {
                    let old_total = {
                        let sc = s.terminal.screen();
                        sc.scrollback_len() + sc.used_rows()
                    };
                    s.terminal.process_bytes(&data);
                    let _ = s.terminal.take_bell();
                    let outbound = s.terminal.take_outbound();
                    if !outbound.is_empty() {
                        let _ = s.control_tx.send(Control::Input(outbound));
                    }
                    let new_total = {
                        let sc = s.terminal.screen();
                        sc.scrollback_len() + sc.used_rows()
                    };
                    // Stick to the bottom only when output extends history. TUI
                    // redraws rewrite in place and must not yank the viewport
                    // around; studio's dispatch.rs does the same.
                    grew |= new_total > old_total;
                    changed = true;
                }
                Ok(Output::Resized { cols, rows }) => {
                    if s.applied_cols != cols || s.applied_rows != rows {
                        s.applied_cols = cols;
                        s.applied_rows = rows;
                        s.terminal.resize(cols as usize, rows as usize);
                    }
                    changed = true;
                }
                Ok(Output::Exited) | Err(TryRecvError::Disconnected) => {
                    exited = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if exited {
            dead.push(*tab);
        } else if changed {
            rebuild_frame(s, grew);
        }
        if changed || exited {
            dirty.push(*tab);
        }
    }
    for tab in &dead {
        guard.remove(tab);
    }
    dirty
}

/// Keystrokes/paste from the view, already encoded to terminal bytes.
pub fn input(session: Session, data: Vec<u8>) {
    if data.is_empty() {
        return;
    }
    let guard = sessions().lock().unwrap();
    if let Some(s) = guard.get(&session) {
        let _ = s.control_tx.send(Control::Input(data));
    }
}

/// The view's viewport: rendered size, shell window height, and scroll
/// position (`usize::MAX` = follow the bottom). Port of studio's
/// single-subscriber `on_terminal_viewport_request`.
pub fn request_viewport(
    session: Session,
    cols: u16,
    rows: u16,
    pty_rows: u16,
    top_row: usize,
) -> bool {
    let mut guard = sessions().lock().unwrap();
    let Some(s) = guard.get_mut(&session) else {
        return false;
    };
    let cols = cols.max(1);
    let rows = rows.max(1);
    let pty_rows = pty_rows.max(1);
    if cols != s.cols || pty_rows != s.pty_rows {
        let _ = s.control_tx.send(Control::Resize {
            cols,
            rows: pty_rows,
        });
    }
    s.cols = cols;
    s.view_rows = rows;
    s.pty_rows = pty_rows;

    let max_top = max_top_row(&s.terminal, rows);
    if top_row == usize::MAX {
        s.top_row = max_top;
        s.anchor_bottom = true;
    } else {
        let clamped = top_row.min(max_top);
        s.anchor_bottom = clamped >= max_top.saturating_sub(1);
        s.top_row = clamped;
    }
    rebuild_frame(s, false);
    true
}

/// Re-apply the active theme to every open session and rebuild its frame, so a
/// live theme switch recolors terminals at once — framebuffers bake resolved
/// RGB, so a palette change needs the rebuild.
pub fn retheme_all() {
    let theme = crate::theme::active_theme();
    let mut guard = sessions().lock().unwrap();
    for s in guard.values_mut() {
        apply_theme(&mut s.terminal, &theme);
        rebuild_frame(s, false);
    }
}

fn rebuild_frame(s: &mut Shell, force_bottom: bool) {
    let max_top = max_top_row(&s.terminal, s.view_rows);
    if s.anchor_bottom && force_bottom {
        s.top_row = max_top;
    }
    s.top_row = s.top_row.min(max_top);
    s.frame_id = s.frame_id.wrapping_add(1);
    s.frame = Some(framebuffer_from_terminal(
        &s.terminal,
        s.cols,
        s.view_rows,
        s.top_row,
        s.frame_id,
    ));
}

// ---------------------------------------------------------------------------
// Ports of the hub's frame building (studio/hub/src/dispatch.rs).
// ---------------------------------------------------------------------------

fn max_top_row(terminal: &Terminal, rows: u16) -> usize {
    let screen = terminal.screen();
    let is_tui = screen.scroll_top != 0
        || screen.scroll_bottom != screen.rows()
        || terminal.modes.alt_screen;
    let total_lines = if is_tui {
        screen.scrollback_len() + screen.rows()
    } else {
        screen.scrollback_len() + screen.used_rows()
    };
    total_lines.saturating_sub(rows.max(1) as usize)
}

fn rgb_to_u32(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

fn framebuffer_from_terminal(
    terminal: &Terminal,
    cols: u16,
    rows: u16,
    requested_top_row: usize,
    frame_id: u64,
) -> TerminalFramebuffer {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let cols_usize = cols as usize;
    let rows_usize = rows as usize;
    let screen = terminal.screen();
    let is_tui = screen.scroll_top != 0
        || screen.scroll_bottom != screen.rows()
        || terminal.modes.alt_screen;

    let total_lines = if is_tui {
        screen.scrollback_len() + screen.rows()
    } else {
        screen.scrollback_len() + screen.used_rows()
    };
    let max_top = total_lines.saturating_sub(rows_usize);
    let top_row = requested_top_row.min(max_top);
    let mut cells = Vec::with_capacity(cols_usize * rows_usize * 10);
    let palette = &terminal.palette.colors;
    let default_fg = terminal.default_fg;
    let default_bg = terminal.default_bg;
    for row in 0..rows_usize {
        let virtual_row = top_row + row;
        let row_slice = screen.row_slice_virtual(virtual_row);
        for col in 0..cols_usize {
            let (codepoint, fg, bg) = if let Some(cell) = row_slice.and_then(|slice| slice.get(col))
            {
                let mut fg_src = cell.style.fg;
                let mut bg_src = cell.style.bg;
                if cell.style.flags.has(StyleFlags::INVERSE) {
                    std::mem::swap(&mut fg_src, &mut bg_src);
                }
                let fg = fg_src.resolve(palette, default_fg);
                let bg = bg_src.resolve(palette, default_bg);
                // Preserve raw codepoints so the view can distinguish
                // placeholder/continuation cells (e.g. '\0') during copy.
                (cell.codepoint as u32, fg, bg)
            } else {
                (' ' as u32, default_fg, default_bg)
            };
            cells.extend_from_slice(&codepoint.to_le_bytes());
            cells.push(fg.r);
            cells.push(fg.g);
            cells.push(fg.b);
            cells.push(bg.r);
            cells.push(bg.g);
            cells.push(bg.b);
        }
    }

    let cursor_virtual_row = screen.scrollback_len().saturating_add(terminal.cursor().y);
    let cursor_row = cursor_virtual_row as isize - top_row as isize;
    let cursor_visible =
        terminal.modes.cursor_visible && cursor_row >= 0 && cursor_row < rows_usize as isize;

    TerminalFramebuffer {
        frame_id,
        cols,
        rows,
        top_row,
        total_lines,
        cursor_col: terminal.cursor().x as u16,
        cursor_row: if cursor_visible {
            cursor_row as i32
        } else {
            -1
        },
        cursor_visible,
        default_fg_rgb: rgb_to_u32(default_fg.r, default_fg.g, default_fg.b),
        default_bg_rgb: rgb_to_u32(default_bg.r, default_bg.g, default_bg.b),
        bracketed_paste: terminal.modes.bracketed_paste,
        cursor_keys_application_mode: terminal.modes.cursor_keys,
        is_tui,
        cells,
    }
}

// ---------------------------------------------------------------------------
// The PTY I/O thread — a trimmed port of the hub's run_terminal_loop
// (studio/hub/src/terminal_manager.rs): control drain → backpressured writes
// → batched reads → one coalesced+throttled resize, then sleep.
// ---------------------------------------------------------------------------

struct PendingInput {
    data: Vec<u8>,
    offset: usize,
}

fn run_terminal_loop(pty: Pty, control_rx: Receiver<Control>, output_tx: Sender<Output>) {
    const MAX_READ_BYTES_PER_TICK: usize = 1 << 20;

    let mut should_close = false;
    let mut pending_input = VecDeque::<PendingInput>::new();
    let mut pending_resize: Option<(u16, u16)> = None;
    let mut last_resize_time = std::time::Instant::now() - Duration::from_secs(1);
    // Throttle to 20fps so TUI apps don't get a SIGWINCH storm and leave
    // half-drawn intermediate states behind.
    let resize_throttle = Duration::from_millis(50);

    loop {
        loop {
            match control_rx.try_recv() {
                Ok(Control::Input(data)) => {
                    if !data.is_empty() {
                        pending_input.push_back(PendingInput { data, offset: 0 });
                    }
                }
                Ok(Control::Resize { cols, rows }) => {
                    // Coalesce resize bursts; apply the latest once per loop.
                    pending_resize = Some((cols.max(1), rows.max(1)));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    should_close = true;
                    break;
                }
            }
        }
        if should_close {
            break;
        }

        while let Some(front) = pending_input.front_mut() {
            let remaining = &front.data[front.offset..];
            match pty.try_write(remaining) {
                Ok(0) => {
                    should_close = true;
                    break;
                }
                Ok(n) => {
                    front.offset += n;
                    if front.offset >= front.data.len() {
                        pending_input.pop_front();
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    should_close = true;
                    break;
                }
            }
        }
        if should_close {
            break;
        }

        let mut output = Vec::new();
        loop {
            let Some(data) = pty.try_read() else {
                break;
            };
            if data.is_empty() {
                continue;
            }
            output.extend_from_slice(&data);
            if output.len() >= MAX_READ_BYTES_PER_TICK {
                break;
            }
        }
        let had_output = !output.is_empty();
        if had_output {
            let _ = output_tx.send(Output::Data(output));
            SignalToUI::set_ui_signal();
        }

        // Apply one coalesced resize after I/O so buffered output is consumed
        // before the shell learns the new geometry.
        if let Some((cols, rows)) = pending_resize {
            let now = std::time::Instant::now();
            if now.duration_since(last_resize_time) >= resize_throttle {
                pending_resize = None;
                last_resize_time = now;
                if pty.resize(cols, rows).is_ok() {
                    let _ = output_tx.send(Output::Resized { cols, rows });
                    SignalToUI::set_ui_signal();
                }
            }
        }

        if pending_input.is_empty() && pending_resize.is_none() && !had_output {
            std::thread::sleep(Duration::from_millis(16));
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    let _ = output_tx.send(Output::Exited);
    SignalToUI::set_ui_signal();
}

/// The frame's cells as one flat string — shared by the debug dump and the
/// PTY round-trip tests.
fn frame_text(f: &TerminalFramebuffer) -> String {
    f.cells
        .chunks(10)
        .map(|c| {
            char::from_u32(u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .filter(|ch| *ch != '\0')
                .unwrap_or(' ')
        })
        .collect()
}

/// Dev aid (CONCATS_APP_TERM_DEBUG=1): dump every session's frame text to
/// stderr so "frame exists but renders empty" can be told apart from "no
/// output arrived".
pub fn debug_dump() {
    let guard = sessions().lock().unwrap();
    if guard.is_empty() {
        eprintln!("term-debug: no sessions");
        return;
    }
    for (tab, s) in guard.iter() {
        let Some(f) = &s.frame else {
            eprintln!("term-debug: [{}] no frame", tab.tab.0);
            continue;
        };
        eprintln!(
            "term-debug: [{}] cursor=({},{}) top={} total={} {}x{} text={:?}",
            tab.tab.0,
            f.cursor_col,
            f.cursor_row,
            f.top_row,
            f.total_lines,
            f.cols,
            f.rows,
            frame_text(f)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end through the real PTY: spawn a shell, run a command, and
    /// assert its output lands in the framebuffer.
    #[test]
    fn shell_roundtrip() {
        let tab = Session {
            window: LiveId(0),
            tab: LiveId(0xC0FFEE01),
        };
        open(tab, Path::new("."), &[]);
        assert!(is_open(tab), "shell failed to spawn");
        input(tab, b"echo concats_ok\r".to_vec());
        let mut seen = String::new();
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(250));
            drain();
            assert!(is_open(tab), "shell exited early");
            if let Some(f) = frame_of(tab) {
                seen = frame_text(&f);
                if seen.contains("concats_ok") {
                    close(tab);
                    return;
                }
            }
        }
        panic!("shell output never arrived; screen: {:?}", seen.trim());
    }

    /// The app exports the window's range into the shell; a command must see
    /// it.
    #[test]
    fn env_reaches_the_shell() {
        let tab = Session {
            window: LiveId(0),
            tab: LiveId(0xC0FFEE03),
        };
        open(
            tab,
            Path::new("."),
            &[("CONCATS_APP_BASE", concats_diff::load::INDEX_REV)],
        );
        assert!(is_open(tab), "shell failed to spawn");
        // `base=$VAR`: the echoed command line shows the unexpanded form, so
        // only the shell's own expansion can produce the needle.
        input(tab, b"echo base=$CONCATS_APP_BASE\r".to_vec());
        let mut seen = String::new();
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(250));
            drain();
            assert!(is_open(tab), "shell exited early");
            if let Some(f) = frame_of(tab) {
                seen = frame_text(&f);
                if seen.contains("base=INDEX") {
                    close(tab);
                    return;
                }
            }
        }
        panic!("env never echoed; screen: {:?}", seen.trim());
    }

    /// Mimic the GUI's flow: viewport requests interleaved with drains, the
    /// way redraws fire them. Diagnoses the app-side wiring.
    #[test]
    fn viewport_interplay() {
        let tab = Session {
            window: LiveId(0),
            tab: LiveId(0xC0FFEE02),
        };
        open(tab, Path::new("."), &[]);
        assert!(is_open(tab), "shell failed to spawn");
        input(tab, b"echo concats_ok\r".to_vec());
        for tick in 0..30 {
            std::thread::sleep(Duration::from_millis(200));
            let changed = drain();
            request_viewport(tab, 166, 15, 14, usize::MAX);
            assert!(is_open(tab), "shell exited early");
            if let Some(f) = frame_of(tab) {
                let text = frame_text(&f);
                eprintln!(
                    "tick {tick} changed={changed:?} cursor=({},{}) top={} total={} rows={} cols={} text={:?}",
                    f.cursor_col,
                    f.cursor_row,
                    f.top_row,
                    f.total_lines,
                    f.rows,
                    f.cols,
                    text.split_whitespace().collect::<Vec<_>>().join(" ")
                );
                if text.contains("concats_ok") {
                    close(tab);
                    return;
                }
            }
        }
        panic!("prompt/echo never rendered under viewport interplay");
    }
}
