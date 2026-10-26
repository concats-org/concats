//! The pipe a window reads in the checkout it shows, so `concats` and any
//! agent working there can ask what is on screen — the range, and the threads
//! it shows — and answer them. How a request travels, and the routes, are
//! `concats_state`'s.
//!
//! Every request reads the document as it stands when it arrives: an answer
//! follows the window across range switches with nothing to republish.
//!
//! The threads a window shows are the ones its range places — what the review
//! is about. The store holds every thread of every worktree and every range
//! ever reviewed in the repo; an agent asking for "the comments" means these.

use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};

use concats_review::{
    Error,
    comments::{self, Scope},
    interchange,
    store::Store,
};
use concats_state::{Answer, COMMENTS, Method, RANGE, REPLIES, Request};

use crate::{review_doc::ReviewDoc, window::WindowState};

/// A pipe a window reads. Dropping it removes the pipe and ends the thread
/// reading it.
pub(crate) struct Pipe {
    root: PathBuf,
    /// The reading thread's pipe, to stop it with an empty line — never a
    /// request id.
    wake: File,
}

impl Drop for Pipe {
    fn drop(&mut self) {
        let path = self.root.join(concats_state::PIPE);
        if let Err(error) = std::fs::remove_file(&path) {
            eprintln!("concats-app: cannot remove {}: {error}", path.display());
        }
        if let Err(error) = self.wake.write_all(b"\n") {
            eprintln!(
                "concats-app: cannot stop reading {}: {error}",
                path.display()
            );
        }
    }
}

/// Answer for the checkout `repo` is in, letting go of whatever this window
/// answered for before. A no-op while it already answers there.
pub(crate) fn serve(state: &Arc<WindowState>, repo: &str) {
    let Some(root) = concats_diff::load::root(Path::new(repo)) else {
        return;
    };
    let mut pipe = state.pipe.lock().unwrap();
    if pipe.as_ref().is_some_and(|p| p.root == root) {
        return;
    }
    *pipe = None;
    let reading = match concats_state::listen(&root) {
        Ok(Some(reading)) => reading,
        // NOTE: Another window reads it: the first to open a checkout keeps it.
        Ok(None) => return,
        Err(error) => {
            let path = root.join(concats_state::PIPE);
            eprintln!("concats-app: cannot answer on {}: {error}", path.display());
            return;
        }
    };
    let wake = match reading.try_clone() {
        Ok(wake) => wake,
        Err(error) => {
            eprintln!("concats-app: cannot answer on {}: {error}", root.display());
            return;
        }
    };
    let (window, answering) = (Arc::downgrade(state), root.clone());
    std::thread::spawn(move || read(reading, &answering, &window));
    *pipe = Some(Pipe { root, wake });
}

/// Answer each request id clients write to the pipe, until the window lets go
/// of it or goes away.
// NOTE: the reading thread's body, apart from the setup `serve` does on the
// caller's thread.
// coland-ignore: @coland/single_caller_private_function
fn read(pipe: File, root: &Path, window: &Weak<WindowState>) {
    for line in BufReader::new(pipe).split(b'\n') {
        // NOTE: A read that fails will fail again; this window stops answering.
        let Ok(line) = line else {
            break;
        };
        let Some(state) = window.upgrade().filter(|_| !line.is_empty()) else {
            break;
        };
        if let Ok(id) = std::str::from_utf8(&line) {
            respond_to(&state.snapshot(), root, id);
        }
    }
}

/// Answer the request left under `id`; when it cannot be read, the client
/// hears why.
// NOTE: the reading loop's one step, apart so the loop only says when to stop.
// coland-ignore: @coland/single_caller_private_function
fn respond_to(doc: &ReviewDoc, root: &Path, id: &str) {
    let answer = match concats_state::read_request(root, id) {
        Ok(request) => respond(doc, &request),
        Err(error) => text(400, &format!("cannot read the request: {error}")),
    };
    if let Err(error) = concats_state::answer(root, id, &answer) {
        // NOTE: The client hung up; there is no one left to tell.
        eprintln!("concats-app: answer to {id} not sent: {error}");
    }
}

fn respond(doc: &ReviewDoc, request: &Request) -> Answer {
    let Ok(url) = url::Url::parse("http://concats").and_then(|base| base.join(&request.target))
    else {
        return text(400, "malformed request target");
    };
    match (request.method, url.path(), reply_to(url.path())) {
        (Method::Get, RANGE, _) => match serde_json::to_string(&doc.target()) {
            Ok(json) => Answer {
                status: 200,
                body: json,
            },
            Err(error) => text(500, &error.to_string()),
        },
        (Method::Get, COMMENTS, _) => shown_comments(doc, query(&url, "all").is_some()),
        (Method::Post, _, Some(to)) => reply(doc, to, request.body.trim(), &url),
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
    let loaded = match load(doc) {
        Ok(loaded) => loaded,
        Err(answer) => return answer,
    };
    let scope = if all {
        Scope::All
    } else {
        Scope::Shown(&doc.placed_threads)
    };
    let (entries, _) = comments::entries(&loaded, &Store::open(&loaded.git_dir).comments, scope);
    Answer {
        status: 200,
        body: interchange::render(&comments::meta(&loaded), &entries),
    }
}

/// Answer the thread `to` is in, the way `concats comments reply` does: by
/// `?author=`, and at `?at=` — where its code lives now — when given.
fn reply(doc: &ReviewDoc, to: u64, body: &str, url: &url::Url) -> Answer {
    let Some(git_dir) = &doc.git_dir else {
        return text(503, "this window has no repository open");
    };
    let at = query(url, "at");
    let loaded = match at.as_ref().map(|_| load(doc)).transpose() {
        Ok(loaded) => loaded,
        Err(answer) => return answer,
    };
    let moved = loaded.as_ref().zip(at.as_deref());
    let mut store = Store::open(git_dir);
    match comments::reply(&mut store, to, body, query(url, "author"), moved) {
        Ok(c) => text(201, &comments::added(&c, loaded.as_ref())),
        Err(error) => {
            let status = match error {
                Error::EmptyBody => 400,
                Error::NoSuchComment(_) => 404,
                Error::Store { .. } => 500,
                Error::AnchorSyntax
                | Error::AnchorLines(_)
                | Error::AnchorZero
                | Error::AnchorOldSide { .. }
                | Error::NotInDiff(_)
                | Error::LinesNotInDiff { .. }
                | Error::UnsupportedSchema { .. }
                | Error::Json(_) => 422,
            };
            text(status, &error.to_string())
        }
    }
}

/// The diff `doc` shows, loaded fresh the way the CLI loads it.
fn load(doc: &ReviewDoc) -> Result<concats_diff::load::Loaded, Answer> {
    let target = doc.target();
    concats_diff::load::load(Path::new(&target.repo), &target.base, &target.head)
        .map_err(|error| text(503, &error.to_string()))
}

fn text(status: u16, body: &str) -> Answer {
    Answer {
        status,
        body: format!("{body}\n"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use concats_fixture::git;
    use concats_review::store::{Anchor, Comment};

    use super::*;
    use crate::makepad_widgets::LiveId;

    /// One request, as `concats` asks it: the status and the body.
    fn ask(root: &Path, method: Method, target: &str, body: &str) -> (u16, String) {
        let request = Request {
            method,
            target: target.into(),
            body: body.into(),
        };
        let answer = concats_state::ask(root, &request).unwrap().unwrap();
        (answer.status, answer.body)
    }

    /// A reply to comment `to`, posted with `query`.
    fn post_reply(root: &Path, to: u64, query: &str, body: &str) -> (u16, String) {
        ask(
            root,
            Method::Post,
            &format!("{COMMENTS}/{to}/{REPLIES}{query}"),
            body,
        )
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
        reason = "One window's pipe, asked every route in turn against one store."
    )]
    fn a_window_answers_for_its_checkout_with_the_threads_it_shows() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git(&root, &["init", "-q"]);
        std::fs::write(root.join("file.rs"), "old\n").unwrap();
        git(&root, &["add", "file.rs"]);
        git(&root, &["commit", "-qm", "fixture"]);
        std::fs::write(root.join("file.rs"), "new\n").unwrap();

        let git_dir = root.join(".git");
        let mut store = Store::open(&git_dir);
        let shown = store
            .add_comment(comment("file.rs", "new\n", "on screen"))
            .unwrap()
            .id;
        let elsewhere = store
            .add_comment(comment("file.rs", "gone\n", "another range"))
            .unwrap()
            .id;
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

        let (status, range) = ask(&root, Method::Get, RANGE, "");
        assert_eq!(status, 200);
        let range: concats_state::Target = serde_json::from_str(&range).unwrap();
        assert_eq!(
            (range.base.as_str(), range.head.as_str()),
            ("HEAD", "WORKTREE")
        );

        let (status, threads) = ask(&root, Method::Get, COMMENTS, "");
        assert_eq!(status, 200);
        assert!(threads.contains("on screen"), "{threads}");
        assert!(!threads.contains("another range"), "{threads}");

        let (_, every) = ask(&root, Method::Get, &format!("{COMMENTS}?all"), "");
        assert!(every.contains("on screen") && every.contains("another range"));

        let (status, said) = post_reply(&root, shown, "?author=claude", "fixed\n");
        assert_eq!(status, 201, "{said}");
        let (status, said) = post_reply(&root, elsewhere, "?at=file.rs:7", "moved");
        assert_eq!(status, 422);
        assert!(said.contains("not part of this diff"), "{said}");
        let (status, said) = post_reply(&root, shown, "?at=file.rs@INDEX:1", "moved");
        assert_eq!(status, 422);
        assert!(said.contains("not this diff's old side"), "{said}");
        let (status, _) = post_reply(&root, 999, "", "x");
        assert_eq!(status, 404);
        let (status, _) = post_reply(&root, 999, "?at=file.rs:1", "x");
        assert_eq!(status, 404);
        let (status, said) = post_reply(&root, shown, "", " \n");
        assert_eq!((status, said.as_str()), (400, "empty comment body\n"));

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

        state.pipe.lock().unwrap().take();
        assert!(
            !root.join(concats_state::PIPE).exists(),
            "closing the window leaves no pipe behind"
        );
        let request = Request {
            method: Method::Get,
            target: RANGE.into(),
            body: String::new(),
        };
        assert!(concats_state::ask(&root, &request).unwrap().is_none());
    }

    #[test]
    fn only_a_reply_route_names_a_comment() {
        assert_eq!(reply_to("/comments/12/replies"), Some(12));
        assert_eq!(reply_to("/comments/12"), None);
        assert_eq!(reply_to("/comments/x/replies"), None);
        assert_eq!(reply_to("/range"), None);
    }
}
