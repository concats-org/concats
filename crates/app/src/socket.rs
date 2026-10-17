//! The socket a window answers on in the checkout it shows, so `concats` and
//! any agent working there can ask what is on screen — the range, and the
//! threads it shows — and answer them. Plain HTTP, so `curl --unix-socket` is
//! a client; the routes are `concats_state`'s.
//!
//! Every request reads the document as it stands when it arrives: an answer
//! follows the window across range switches with nothing to republish.
//!
//! The threads a window shows are the ones its range places — what the review
//! is about. The store holds every thread of every worktree and every range
//! ever reviewed in the repo; an agent asking for "the comments" means these.

use std::{
    io::Cursor,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::Arc,
};

use concats_review::{
    comments, interchange,
    store::{self, Comment, Store},
};
use concats_state::{COMMENTS, RANGE, REPLIES};
use tiny_http::{Header, Method, Request, Response};

use crate::{review_doc::ReviewDoc, window::WindowState};

/// A bound socket. Dropping it stops the server, which removes the socket file.
pub(crate) struct Socket {
    root: PathBuf,
    server: Arc<tiny_http::Server>,
}

impl Drop for Socket {
    fn drop(&mut self) {
        // NOTE: The serving thread holds the other handle; whichever goes last
        // removes the file.
        self.server.unblock();
    }
}

/// Answer for the checkout `repo` is in, letting go of whatever this window
/// answered for before. A no-op while it already answers there.
pub(crate) fn serve(state: &Arc<WindowState>, repo: &str) {
    let Some(root) = concats_diff::load::discover(Path::new(repo)) else {
        return;
    };
    let mut socket = state.socket.lock().unwrap();
    if socket.as_ref().is_some_and(|s| s.root == root) {
        return;
    }
    *socket = None;
    let Some(server) = bind(&root) else {
        return;
    };
    let window = Arc::downgrade(state);
    let serving = server.clone();
    std::thread::spawn(move || {
        for request in serving.incoming_requests() {
            let Some(state) = window.upgrade() else {
                break;
            };
            answer(&state, request);
        }
    });
    *socket = Some(Socket { root, server });
}

/// Bind `root`'s socket, unless another window — in this app or another one —
/// already answers there: the first to open a checkout keeps it.
fn bind(root: &Path) -> Option<Arc<tiny_http::Server>> {
    let path = root.join(concats_state::SOCKET);
    let dir = path.parent()?;
    if let Err(error) = std::fs::create_dir_all(dir) {
        eprintln!("concats-app: cannot create {}: {error}", dir.display());
        return None;
    }
    if UnixStream::connect(&path).is_ok() {
        return None;
    }
    // NOTE: Nothing answers, so the file is absent or left by a crash; bind
    // will not take the path while it exists.
    let _ = std::fs::remove_file(&path);
    tiny_http::Server::http_unix(&path)
        .map(Arc::new)
        .inspect_err(|error| {
            eprintln!("concats-app: cannot answer on {}: {error}", path.display());
        })
        .ok()
}

fn answer(state: &WindowState, mut request: Request) {
    let response = respond(&state.snapshot(), &mut request);
    if let Err(error) = request.respond(response) {
        // NOTE: The client hung up; there is no one left to tell.
        eprintln!("concats-app: socket response not sent: {error}");
    }
}

type Answer = Response<Cursor<Vec<u8>>>;

fn respond(doc: &ReviewDoc, request: &mut Request) -> Answer {
    let Ok(url) = url::Url::parse("http://concats").and_then(|base| base.join(request.url()))
    else {
        return text(400, "malformed request target");
    };
    match (request.method(), url.path(), reply_to(url.path())) {
        (Method::Get, RANGE, _) => match serde_json::to_string(&doc.target()) {
            Ok(json) => typed(200, "application/json", json),
            Err(error) => text(500, &error.to_string()),
        },
        (Method::Get, COMMENTS, _) => shown_comments(doc, query(&url, "all").is_some()),
        (Method::Post, _, Some(to)) => {
            let mut body = String::new();
            if request.as_reader().read_to_string(&mut body).is_err() {
                return text(400, "the reply body must be UTF-8 text");
            }
            reply(doc, to, body.trim(), &url)
        }
        _ => text(
            404,
            &format!(
                "no such route — GET {RANGE}, GET {COMMENTS}[?all], POST {COMMENTS}/{{id}}/{REPLIES}"
            ),
        ),
    }
}

fn query(url: &url::Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// `{COMMENTS}/{id}/{REPLIES}` → the id.
fn reply_to(path: &str) -> Option<u64> {
    path.strip_prefix(COMMENTS)?
        .strip_prefix('/')?
        .strip_suffix(REPLIES)?
        .strip_suffix('/')?
        .parse()
        .ok()
}

/// The threads `doc` places, whole, as the document `concats comments export`
/// writes; with `all`, every thread in the store.
fn shown_comments(doc: &ReviewDoc, all: bool) -> Answer {
    let (loaded, root) = match load(doc) {
        Ok(loaded) => loaded,
        Err(answer) => return answer,
    };
    let store = Store::open(&loaded.git_dir);
    let shown: Vec<Comment> = store
        .comments
        .iter()
        .filter(|c| all || doc.placed_threads.contains(&store::thread_key(c)))
        .cloned()
        .collect();
    let (entries, _) = comments::entries(&loaded, &shown);
    typed(
        200,
        "text/markdown; charset=utf-8",
        interchange::render(&comments::meta(&loaded, &root), &entries),
    )
}

/// Answer the thread `to` is in, the way `concats comments reply` does: by
/// `?author=`, and at `?at=` — where its code lives now — when given.
fn reply(doc: &ReviewDoc, to: u64, body: &str, url: &url::Url) -> Answer {
    let author = query(url, "author");
    if body.is_empty() {
        return text(400, "empty reply body");
    }
    let Some(git_dir) = &doc.git_dir else {
        return text(503, "this window has no repository open");
    };
    let mut store = Store::open(git_dir);
    let Some(thread) = store.root_of(to).map(|c| c.id) else {
        return text(404, &format!("no comment with id {to}"));
    };
    let Some(spec) = query(url, "at") else {
        let id = store.reply_comment(to, body.to_string(), author, store::now(), None);
        return match id {
            Some(id) => text(201, &format!("added comment #{id} replying to #{thread}")),
            None => text(404, &format!("no comment with id {to}")),
        };
    };
    let (loaded, root) = match load(doc) {
        Ok(loaded) => loaded,
        Err(answer) => return answer,
    };
    let (path, anchor) = match comments::resolve_anchor(&spec, &loaded, &root) {
        Ok(resolved) => resolved,
        Err(error) => return text(422, &error.to_string()),
    };
    let id = store.add_comment(Comment {
        id: 0,
        path: path.clone(),
        anchor,
        body: body.to_string(),
        author,
        created_at: store::now(),
        parent: Some(thread),
        external: None,
        cursors: comments::document_cursors(&loaded, &path, anchor),
        // NOTE: a reply names lines; a quote is the app's selection.
        quote: None,
    });
    text(
        201,
        &format!(
            "added comment #{id} replying to #{thread}, on {path}:{}–{}",
            anchor.start + 1,
            anchor.end + 1
        ),
    )
}

/// The diff `doc` shows, loaded fresh the way the CLI loads it, with the root
/// its links are relative to.
fn load(doc: &ReviewDoc) -> Result<(concats_diff::load::Loaded, String), Answer> {
    let target = doc.target();
    let root = concats_diff::load::discover(Path::new(&target.repo))
        .ok_or_else(|| text(503, &format!("no git repository at {}", target.repo)))?;
    let loaded = concats_diff::load::load(Path::new(&target.repo), &target.base, &target.head)
        .map_err(|error| text(503, &error.to_string()))?;
    Ok((loaded, root.display().to_string()))
}

fn text(status: u16, body: &str) -> Answer {
    typed(status, "text/plain; charset=utf-8", format!("{body}\n"))
}

fn typed(status: u16, content_type: &str, body: String) -> Answer {
    let header = Header::from_bytes("Content-Type", content_type).expect("a valid header");
    Response::from_string(body)
        .with_status_code(status)
        .with_header(header)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        io::{Read, Write},
        process::Command,
    };

    use concats_review::store::Anchor;

    use super::*;
    use crate::makepad_widgets::LiveId;

    /// One request on a fresh connection, as `curl --unix-socket` sends it:
    /// the status and the body.
    fn ask(socket: &Path, method: &str, target: &str, body: &str) -> (u16, String) {
        let mut stream = UnixStream::connect(socket).unwrap();
        write!(
            stream,
            "{method} {target} HTTP/1.1\r\nHost: concats\r\nConnection: close\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, body.to_string())
    }

    fn comment(path: &str, text: &str, body: &str) -> Comment {
        Comment {
            id: 0,
            path: path.into(),
            anchor: Anchor {
                blob: concats_sync::hash_object(text.as_bytes()),
                start: 0,
                end: 0,
            },
            body: body.into(),
            author: Some("reviewer".into()),
            created_at: 1,
            parent: None,
            external: None,
            cursors: None,
            quote: None,
        }
    }

    #[test]
    #[expect(
        clippy::cognitive_complexity,
        reason = "One window's socket, asked every route in turn against one store."
    )]
    fn a_window_answers_for_its_checkout_with_the_threads_it_shows() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .current_dir(&root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("file.rs"), "old\n").unwrap();
        git(&["add", "file.rs"]);
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ]);
        std::fs::write(root.join("file.rs"), "new\n").unwrap();

        let git_dir = root.join(".git");
        let mut store = Store::open(&git_dir);
        let shown = store.add_comment(comment("file.rs", "new\n", "on screen"));
        let elsewhere = store.add_comment(comment("file.rs", "gone\n", "another range"));
        drop(store);

        let state = WindowState::new(LiveId(1), None);
        state.with(|d| {
            d.repo = root.display().to_string();
            d.base = "HEAD".into();
            d.head = "WORKTREE".into();
            d.git_dir = Some(git_dir.clone());
            d.placed_threads = HashSet::from([shown]);
        });
        serve(&state, &root.display().to_string());
        let socket = root.join(concats_state::SOCKET);

        let (status, range) = ask(&socket, "GET", RANGE, "");
        assert_eq!(status, 200);
        let range: concats_state::Target = serde_json::from_str(&range).unwrap();
        assert_eq!(
            (range.base.as_str(), range.head.as_str()),
            ("HEAD", "WORKTREE")
        );

        let (status, threads) = ask(&socket, "GET", COMMENTS, "");
        assert_eq!(status, 200);
        assert!(threads.contains("on screen"), "{threads}");
        assert!(!threads.contains("another range"), "{threads}");

        let (_, every) = ask(&socket, "GET", &format!("{COMMENTS}?all"), "");
        assert!(every.contains("on screen") && every.contains("another range"));

        let (status, said) = ask(
            &socket,
            "POST",
            &format!("{COMMENTS}/{shown}/{REPLIES}?author=claude"),
            "fixed\n",
        );
        assert_eq!(status, 201, "{said}");
        let (status, said) = ask(
            &socket,
            "POST",
            &format!("{COMMENTS}/{elsewhere}/{REPLIES}?at=file.rs:7"),
            "moved",
        );
        assert_eq!(status, 422);
        assert!(said.contains("not part of this diff"), "{said}");
        let (status, _) = ask(&socket, "POST", &format!("{COMMENTS}/999/{REPLIES}"), "x");
        assert_eq!(status, 404);

        let replies: Vec<Comment> = Store::open(&git_dir)
            .comments
            .into_iter()
            .filter(|c| c.parent.is_some())
            .collect();
        assert_eq!(replies.len(), 1);
        assert_eq!(
            (
                replies[0].parent,
                replies[0].body.as_str(),
                replies[0].author.as_deref()
            ),
            (Some(shown), "fixed", Some("claude"))
        );

        state.socket.lock().unwrap().take();
        let gone = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            !socket.exists()
        });
        assert!(gone, "closing the window leaves no socket behind");
    }

    #[test]
    fn only_a_reply_route_names_a_comment() {
        assert_eq!(reply_to("/comments/12/replies"), Some(12));
        assert_eq!(reply_to("/comments/12"), None);
        assert_eq!(reply_to("/comments/x/replies"), None);
        assert_eq!(reply_to("/range"), None);
    }
}
