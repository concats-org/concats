//! Where the app and the CLI meet: the pipe the app reads in a checkout, how a
//! request and its answer travel, and what they say.
//!
//! A crate because two binaries meet here. The app makes [`PIPE`] in the root
//! of every checkout a window shows; `concats`, run anywhere in that checkout,
//! [`ask`]s it for the window's range, its threads, or to store a reply. That
//! is what lets a bare `concats manifest` review the diff on screen — across
//! range switches, from any terminal.
//!
//! The checkout is the identity. A linked worktree has a root, and so a pipe,
//! of its own, so asking from one never answers for another.
//!
//! Files rather than a socket, because asking then takes nothing but writes in
//! the checkout — which a sandboxed agent may make. Codex's sandbox counts
//! connecting to a Unix socket as network access and refuses it.
//!
//! A request is two files named by an id the client picks: `<id>.request`,
//! its method and target on the first line and the body after, and
//! `<id>.reply`, a pipe of the client's own. The client writes the id to
//! [`PIPE`]; the app answers on `<id>.reply` with `<status> <length>`, a
//! newline, and the body. Those two names are the only files the app opens
//! for a client, so no request can point it anywhere else.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::fs::{FileTypeExt, OpenOptionsExt},
    path::{Path, PathBuf},
    str::FromStr,
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nix::{
    errno::Errno,
    fcntl::{FcntlArg, OFlag, fcntl},
    sys::stat::Mode,
    unistd::mkfifo,
};
use serde::{Deserialize, Serialize};

/// What concats keeps in a checkout's root: the pipe, and in a directory
/// without Git the review state as well.
pub const DIR: &str = ".concats";

/// Where the app reads requests, relative to a checkout's root: a named pipe
/// it holds open for as long as a window shows the checkout.
pub const PIPE: &str = ".concats/app.pipe";

/// `GET`: the window's [`Target`], as JSON.
pub const RANGE: &str = "/range";

/// `GET`: the threads the window shows, as an interchange document; `?all`
/// for every thread in the store. `POST {COMMENTS}/{id}/{REPLIES}` answers
/// one: the body is the reply, `?author=` who wrote it, and `?at=path:lines`
/// where the code lives now — [`reply_target`] writes it.
pub const COMMENTS: &str = "/comments";
pub const REPLIES: &str = "replies";
pub const ALL: &str = "all";
pub const AUTHOR: &str = "author";
pub const AT: &str = "at";

/// The two files of a request, named `<id>` and these suffixes beside the
/// pipe.
const REQUEST: &str = ".request";
const REPLY: &str = ".reply";

/// How long a client waits for an answer. An app that quit after taking a
/// request never answers it.
const PATIENCE: Duration = Duration::from_secs(30);

/// What a window reviews: a repo and the `base...head` range in it — the one
/// value the load pipeline threads from the CLI flags or the picker down to
/// the loader. `repo` is a path; `base`/`head` are whatever the loader
/// resolves: revs, or the `INDEX`/`WORKTREE` sentinels.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub repo: String,
    pub base: String,
    pub head: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    const fn name(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// One request, as the client writes it and the app reads it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    /// A route and its query, e.g. `/comments?all`.
    pub target: String,
    pub body: String,
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}\n{}", self.method.name(), self.target, self.body)
    }
}

impl FromStr for Request {
    type Err = io::Error;

    fn from_str(text: &str) -> io::Result<Self> {
        let (line, body) = text.split_once('\n').unwrap_or((text, ""));
        let (method, target) = line
            .split_once(' ')
            .ok_or_else(|| malformed("a request starts with its method and target"))?;
        let method = [Method::Get, Method::Post]
            .into_iter()
            .find(|m| m.name() == method)
            .ok_or_else(|| malformed("the method is GET or POST"))?;
        Ok(Self {
            method,
            target: target.to_string(),
            body: body.to_string(),
        })
    }
}

/// The app's answer: an HTTP status and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub body: String,
}

/// The target that answers comment `to`, by `author` and at `at` when given,
/// escaped so a name with spaces or an anchor with `@` arrives as written.
#[must_use]
pub fn reply_target(to: u64, author: Option<&str>, at: Option<&str>) -> String {
    let mut query = form_urlencoded::Serializer::new(String::new());
    for (key, value) in [(AUTHOR, author), (AT, at)] {
        if let Some(value) = value {
            query.append_pair(key, value);
        }
    }
    format!("{COMMENTS}/{to}/{REPLIES}?{}", query.finish())
}

/// Ask the app reading `root`'s pipe, and wait for its answer. `None` when no
/// app reads it.
///
/// # Errors
///
/// When the request cannot be left beside the pipe — a sandbox that does not
/// allow it — or the app does not answer within thirty seconds.
pub fn ask(root: &Path, request: &Request) -> io::Result<Option<Answer>> {
    let Some(pipe) = writer(root)? else {
        return Ok(None);
    };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = format!("{}-{nanos}", std::process::id());
    let answer = exchange(pipe, root, &id, request);
    for suffix in [REQUEST, REPLY] {
        let path = file(root, &id, suffix)?;
        // NOTE: Absent when the exchange failed before making it.
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!("warning: cannot remove {}: {error}", path.display());
        }
    }
    answer.map(Some)
}

/// Leave `request` under `id`, hand the id to the app, and read its answer.
fn exchange(mut pipe: File, root: &Path, id: &str, request: &Request) -> io::Result<Answer> {
    fs::write(file(root, id, REQUEST)?, request.to_string())?;
    let reply = file(root, id, REPLY)?;
    mkfifo(&reply, Mode::S_IRUSR | Mode::S_IWUSR)?;
    // Both ends: the open need not wait for the app, and the app closing its
    // end is no EOF the read could take for a short answer.
    let reply = OpenOptions::new().read(true).write(true).open(&reply)?;
    // One write under PIPE_BUF, so it lands whole beside other clients' ids.
    pipe.write_all(format!("{id}\n").as_bytes())?;
    let (sender, waiting) = mpsc::channel();
    std::thread::spawn(move || sender.send(read_answer(reply)));
    waiting.recv_timeout(PATIENCE).unwrap_or_else(|_| {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "concats-app took the request but did not answer",
        ))
    })
}

// NOTE: the reader of `answer`'s framing, run on the thread `exchange` waits on.
// coland-ignore: @coland/single_caller_private_function
fn read_answer(reply: impl Read) -> io::Result<Answer> {
    let mut reply = BufReader::new(reply);
    let mut head = String::new();
    reply.read_line(&mut head)?;
    let (status, length) = head
        .trim_end()
        .split_once(' ')
        .and_then(|(status, length)| Some((status.parse().ok()?, length.parse().ok()?)))
        .ok_or_else(|| malformed("an answer starts with its status and length"))?;
    let mut body = vec![0; length];
    reply.read_exact(&mut body)?;
    let body = String::from_utf8(body).map_err(|_| malformed("an answer is UTF-8 text"))?;
    Ok(Answer { status, body })
}

/// Make `root`'s pipe and open it to read — `None` while a window, in this app
/// or another, already reads it: the first to open a checkout keeps it.
///
/// # Errors
///
/// When the directory, its `.gitignore` or the pipe cannot be made.
// NOTE: the app's one way to make the pipe, and every test's that stands in
// for it; its branches are its steps.
// coland-ignore: @coland/high_fan_in_complex_function
pub fn listen(root: &Path) -> io::Result<Option<File>> {
    let dir = root.join(DIR);
    fs::create_dir_all(&dir)?;
    // A checkout can commit symlinks; none may send the app's writes out of it.
    if fs::symlink_metadata(&dir)?.is_symlink() {
        return Err(malformed("the .concats directory is a symlink"));
    }
    if writer(root)?.is_some() {
        return Ok(None);
    }
    // Requests leave files here for a moment; git should never list them.
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(".gitignore"))
    {
        Ok(mut ignore) => ignore.write_all(b"*\n")?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    // NOTE: Nobody reads it, so the path is free or left by an app that quit.
    let path = root.join(PIPE);
    if let Err(error) = fs::remove_file(&path)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error);
    }
    mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR)?;
    // Holding the write end as well: a read then waits for the next client
    // instead of ending when the last one hangs up.
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map(Some)
}

/// The request a client left under `id`.
///
/// # Errors
///
/// When `id` names no plain request file beside the pipe, or it does not
/// parse.
pub fn read_request(root: &Path, id: &str) -> io::Result<Request> {
    // Non-blocking: a pipe in its place must not stall the app. No symlinks: a
    // client cannot have the app read a file it could not.
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW).bits())
        .open(file(root, id, REQUEST)?)?;
    if !file.metadata()?.is_file() {
        return Err(malformed("a request is a plain file"));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    text.parse()
}

/// Answer the client waiting under `id`.
///
/// # Errors
///
/// When `id` names no pipe beside [`PIPE`] — the client gave up, or never made
/// one — or the client stops reading.
pub fn answer(root: &Path, id: &str, answer: &Answer) -> io::Result<()> {
    // Non-blocking, so a client that went away fails the open rather than
    // stalling the app; no symlinks, and only a pipe, so the app writes nothing
    // a client names.
    let mut reply = OpenOptions::new()
        .write(true)
        .custom_flags((OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW).bits())
        .open(file(root, id, REPLY)?)?;
    if !reply.metadata()?.file_type().is_fifo() {
        return Err(malformed("a reply goes to a pipe"));
    }
    // Blocking from here on: an answer can outgrow the pipe's buffer.
    fcntl(&reply, FcntlArg::F_SETFL(OFlag::empty()))?;
    write!(
        reply,
        "{} {}\n{}",
        answer.status,
        answer.body.len(),
        answer.body
    )
}

/// `root`'s pipe opened to write, or `None` when no app reads it: absent, or
/// left by one that quit.
fn writer(root: &Path) -> io::Result<Option<File>> {
    // Non-blocking: without a reader the open fails at once (ENXIO) rather
    // than waiting for one. No symlinks: the id is written to the pipe alone.
    let opened = OpenOptions::new()
        .write(true)
        .custom_flags((OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW).bits())
        .open(root.join(PIPE));
    match opened {
        Ok(pipe) if pipe.metadata()?.file_type().is_fifo() => Ok(Some(pipe)),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) if error.raw_os_error() == Some(Errno::ENXIO as i32) => Ok(None),
        Err(error) => Err(error),
    }
}

/// `<id><suffix>` beside the pipe. An id is letters, digits and dashes, so it
/// names a file there and nowhere else.
fn file(root: &Path, id: &str, suffix: &str) -> io::Result<PathBuf> {
    let plain = !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !plain {
        return Err(malformed("a request id is letters, digits and dashes"));
    }
    Ok(root.join(DIR).join(format!("{id}{suffix}")))
}

fn malformed(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(target: &str) -> Request {
        Request {
            method: Method::Get,
            target: target.into(),
            body: String::new(),
        }
    }

    /// An app on `root` that answers one request with `body`, handing back
    /// the request it read.
    fn answer_once(root: &Path, body: String) -> std::thread::JoinHandle<Request> {
        let pipe = listen(root).unwrap().expect("nobody reads the pipe yet");
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let mut id = String::new();
            BufReader::new(pipe).read_line(&mut id).unwrap();
            let id = id.trim_end();
            let request = read_request(&root, id).unwrap();
            answer(&root, id, &Answer { status: 200, body }).unwrap();
            request
        })
    }

    #[test]
    fn the_pipe_lives_in_the_concats_directory() {
        assert_eq!(Path::new(PIPE).parent(), Some(Path::new(DIR)));
    }

    #[test]
    fn a_request_reads_back_as_written() {
        let request = Request {
            method: Method::Post,
            target: "/comments/7/replies?author=claude".into(),
            body: "fixed\n\nsee the guard".into(),
        };
        assert_eq!(request.to_string().parse::<Request>().unwrap(), request);
        assert!("PUT /range\n".parse::<Request>().is_err());
        assert!("GET\n".parse::<Request>().is_err());
    }

    #[test]
    fn an_answer_larger_than_the_pipe_arrives_whole_and_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let body = "threads ".repeat(40_000);
        let app = answer_once(tmp.path(), body.clone());
        let answer = ask(tmp.path(), &get("/comments?all")).unwrap().unwrap();
        assert_eq!((answer.status, answer.body.len()), (200, body.len()));
        assert_eq!(app.join().unwrap(), get("/comments?all"));
        let mut left: Vec<_> = fs::read_dir(tmp.path().join(DIR))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, [".gitignore", "app.pipe"]);
    }

    #[test]
    fn without_a_reader_there_is_no_app_and_the_pipe_is_free() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(ask(tmp.path(), &get(RANGE)).unwrap().is_none());

        let reading = listen(tmp.path()).unwrap().unwrap();
        assert!(
            listen(tmp.path()).unwrap().is_none(),
            "a second window leaves the pipe to the first"
        );
        drop(reading);
        assert!(ask(tmp.path(), &get(RANGE)).unwrap().is_none());
        assert!(listen(tmp.path()).unwrap().is_some());
    }

    #[test]
    fn a_client_cannot_point_the_app_at_another_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let _reading = listen(root).unwrap().unwrap();
        let ok = Answer {
            status: 200,
            body: "x".into(),
        };

        assert!(read_request(root, "../../etc/passwd").is_err());
        assert!(answer(root, "a/b", &ok).is_err());

        let outside = root.join("outside");
        fs::write(&outside, "GET /range\n").unwrap();
        std::os::unix::fs::symlink(&outside, root.join(DIR).join("s.request")).unwrap();
        assert!(read_request(root, "s").is_err(), "a symlinked request");

        fs::write(root.join(DIR).join("f.reply"), "").unwrap();
        assert!(answer(root, "f", &ok).is_err(), "a plain file for a reply");
        assert_eq!(
            fs::read_to_string(root.join(DIR).join("f.reply")).unwrap(),
            ""
        );
    }

    #[test]
    fn a_checkout_cannot_send_the_pipe_through_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();

        let linked_dir = tmp.path().join("linked-dir");
        fs::create_dir(&linked_dir).unwrap();
        std::os::unix::fs::symlink(&elsewhere, linked_dir.join(DIR)).unwrap();
        assert!(listen(&linked_dir).is_err());
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);

        let linked_pipe = tmp.path().join("linked-pipe");
        fs::create_dir_all(linked_pipe.join(DIR)).unwrap();
        let target = elsewhere.join("target");
        fs::write(&target, "kept").unwrap();
        std::os::unix::fs::symlink(&target, linked_pipe.join(PIPE)).unwrap();
        assert!(ask(&linked_pipe, &get(RANGE)).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "kept");
    }
}
