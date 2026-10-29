//! A session's PTY thread: it reads the shell into the term and writes the
//! view's input back, as alacritty's `EventLoop` did. That loop reads the PTY
//! itself and shows the bytes to no one else; this one also hands every chunk
//! to an [`osc::Reader`], which reads what alacritty's parser drops.

use std::{
    borrow::Cow,
    collections::VecDeque,
    io::{self, ErrorKind, Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    sync::{Arc, atomic::Ordering, mpsc},
    time::{Duration, Instant},
};

use alacritty_terminal::{
    event::{Event, EventListener, OnResize},
    event_loop::Msg,
    tty::{self, EventedPty},
    vte::ansi::Processor,
};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
};

use super::{Proxy, SharedTerm, osc};

/// Most the thread reads ahead while the view holds the term — alacritty's
/// figure.
const READ_BUFFER_SIZE: usize = 0x10_0000;

/// Most it parses in one hold of the term, so the view gets its turn.
const MAX_LOCKED_READ: usize = u16::MAX as usize;

/// How long the thread waits without news before it looks at the shell. A
/// shell that exits while a job it left behind holds the PTY open sends no
/// hangup, only SIGCHLD — which alacritty's PTY keeps to itself.
const CHILD_CHECK: Duration = Duration::from_secs(1);

/// Where the view sends a session's input, resizes and shutdown.
#[derive(Clone)]
pub struct Sender {
    queue: mpsc::Sender<Msg>,
    /// The thread waits in `poll`, not on the queue: a byte here wakes it.
    wake: Arc<UnixStream>,
}

impl Sender {
    pub fn send(&self, msg: Msg) {
        // NOTE: a closed queue means the thread ended with its shell.
        if self.queue.send(msg).is_err() {
            return;
        }
        // NOTE: a full socket already holds a wake-up the thread has to read.
        if let Err(error) = (&*self.wake).write(&[0])
            && error.kind() != ErrorKind::WouldBlock
        {
            eprintln!("terminal: cannot wake the pty thread: {error}");
        }
    }
}

/// The thread's end of a [`Sender`].
pub struct Inbox {
    queue: mpsc::Receiver<Msg>,
    wake: UnixStream,
}

impl Inbox {
    /// What the view sent since the last look, if anything. The wake-ups go
    /// first: one that arrives after this still finds its message.
    pub fn messages(&self) -> Vec<Msg> {
        let mut wakes = [0; 64];
        while matches!((&self.wake).read(&mut wakes), Ok(1..)) {}
        self.queue.try_iter().collect()
    }
}

/// A line to a PTY thread that does not exist yet: the term answers queries
/// through it, so it comes first.
pub fn channel() -> io::Result<(Sender, Inbox)> {
    let (wake, woken) = UnixStream::pair()?;
    wake.set_nonblocking(true)?;
    woken.set_nonblocking(true)?;
    let (queue, inbox) = mpsc::channel();
    let sender = Sender {
        queue,
        wake: Arc::new(wake),
    };
    Ok((
        sender,
        Inbox {
            queue: inbox,
            wake: woken,
        },
    ))
}

/// Run `pty` for `term` on a thread of its own until the shell exits or the
/// view sends [`Msg::Shutdown`].
pub fn spawn(pty: tty::Pty, term: SharedTerm, proxy: Proxy, inbox: Inbox) {
    std::thread::spawn(move || {
        Loop {
            pty,
            term,
            proxy,
            inbox,
            parser: Processor::new(),
            osc: osc::Reader::default(),
            pending: VecDeque::new(),
            written: 0,
        }
        .run();
    });
}

/// The thread's state: the PTY, both parsers reading it, and the input it has
/// yet to take.
struct Loop {
    pty: tty::Pty,
    term: SharedTerm,
    proxy: Proxy,
    inbox: Inbox,
    parser: Processor,
    osc: osc::Reader,
    /// Input the PTY has not taken yet, `written` bytes into the first.
    pending: VecDeque<Cow<'static, [u8]>>,
    written: usize,
}

impl Loop {
    fn run(mut self) {
        let mut buf = vec![0; READ_BUFFER_SIZE];
        loop {
            let deadline = self.parser.sync_timeout().sync_timeout();
            if let Err(error) = self.wait(deadline) {
                eprintln!("terminal: cannot wait on the pty: {error}");
                break;
            }
            for msg in self.inbox.messages() {
                match msg {
                    Msg::Input(bytes) => self.pending.push_back(bytes),
                    Msg::Resize(size) => self.pty.on_resize(size),
                    // NOTE: from `close`: the session is gone, no one to tell.
                    Msg::Shutdown => return,
                }
            }
            if let Err(error) = self.parse_output(&mut buf) {
                eprintln!("terminal: cannot read the pty: {error}");
                break;
            }
            // A program that began a synchronized update and never ended it.
            if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                self.parser.stop_sync(&mut *self.term.lock());
                self.proxy.send_event(Event::Wakeup);
            }
            if let Err(error) = self.write() {
                eprintln!("terminal: cannot write the pty: {error}");
                break;
            }
            if self.pty.next_child_event().is_some() {
                break;
            }
        }
        self.proxy.exited.store(true, Ordering::Relaxed);
        self.proxy.send_event(Event::Wakeup);
    }

    /// Wait for the shell's output, room for input, a message from the view
    /// or `deadline` — whichever comes first. The rest of the turn looks at
    /// each without blocking.
    fn wait(&self, deadline: Option<Instant>) -> nix::Result<()> {
        let timeout = deadline.map_or(CHILD_CHECK, |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .min(CHILD_CHECK)
        });
        let interest = if self.pending.is_empty() {
            PollFlags::POLLIN
        } else {
            PollFlags::POLLIN | PollFlags::POLLOUT
        };
        let mut fds = [
            PollFd::new(self.pty.file().as_fd(), interest),
            PollFd::new(self.inbox.wake.as_fd(), PollFlags::POLLIN),
        ];
        match poll(
            &mut fds,
            PollTimeout::try_from(timeout).unwrap_or(PollTimeout::MAX),
        ) {
            Ok(_) | Err(Errno::EINTR) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Read what the shell wrote, parse it into the term and wake the view.
    /// Nothing written yet returns at once: the PTY does not block.
    #[expect(
        clippy::cognitive_complexity,
        reason = "One read burst: reading ahead while the view holds the term, then parsing under one hold."
    )]
    fn parse_output(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut unprocessed = 0;
        let mut processed = 0;
        // The next hold of the term is this read's: the view waits it out.
        let _lease = self.term.lease();
        let mut term = None;
        loop {
            match self.pty.file().read(&mut buf[unprocessed..]) {
                // What macOS reads once there is nothing more.
                Ok(0) if unprocessed == 0 => break,
                Ok(read) => unprocessed += read,
                Err(error)
                    if !matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) =>
                {
                    return Err(error);
                }
                // Nothing more for now: parse what there is, if anything.
                Err(_) if unprocessed == 0 => break,
                Err(_) => {}
            }
            // Parse once the view lets go of the term, or once the buffer is full.
            let term = match &mut term {
                Some(term) => term,
                None => term.insert(match self.term.try_lock_unfair() {
                    None if unprocessed >= READ_BUFFER_SIZE => self.term.lock_unfair(),
                    None => continue,
                    Some(term) => term,
                }),
            };
            let chunk = &buf[..unprocessed];
            for notification in self.osc.notifications(chunk) {
                self.proxy.bell.lock().unwrap().notify(
                    Instant::now(),
                    notification.title.as_deref(),
                    &notification.body,
                );
            }
            self.parser.advance(&mut **term, chunk);
            processed += unprocessed;
            unprocessed = 0;
            if processed >= MAX_LOCKED_READ {
                break;
            }
        }
        // A redraw, unless all of it is held for a synchronized update.
        if self.parser.sync_bytes_count() < processed {
            self.proxy.send_event(Event::Wakeup);
        }
        Ok(())
    }

    /// Hand the PTY what it takes of the queued input; the rest waits for room.
    fn write(&mut self) -> io::Result<()> {
        while let Some(input) = self.pending.front() {
            match self.pty.file().write(&input[self.written..]) {
                Ok(0) => break,
                Ok(written) => {
                    self.written += written;
                    if self.written == input.len() {
                        self.pending.pop_front();
                        self.written = 0;
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) =>
                {
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}
