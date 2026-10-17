//! Where the app and the CLI meet: the socket the app answers on for a
//! checkout, and what it says.
//!
//! A crate because two binaries meet here. The app binds [`SOCKET`] in the root
//! of every checkout a window shows; `concats`, run anywhere in that checkout,
//! asks it for the window's range. That is what lets a bare `concats manifest`
//! review the diff on screen — across range switches, from any terminal.
//!
//! The checkout is the identity. A linked worktree has a root, and so a socket,
//! of its own, so asking from one never answers for another. And because the
//! socket sits inside the checkout, an agent sandboxed to it can reach the app
//! with nothing but `curl --unix-socket`.

use serde::{Deserialize, Serialize};

/// Where the app answers, relative to a checkout's root.
///
/// NOTE: macOS caps a socket path at 104 bytes; a checkout nested deeper than
/// that gets no socket.
pub const SOCKET: &str = ".concats/app.sock";

/// `GET`: the window's [`Target`], as JSON.
pub const RANGE: &str = "/range";

/// `GET`: the threads the window shows, as an interchange document; `?all`
/// for every thread in the store. `POST {COMMENTS}/{id}/{REPLIES}` answers
/// one: the body is the reply, `?author=` who wrote it, and `?at=path:lines`
/// where the code lives now.
pub const COMMENTS: &str = "/comments";
pub const REPLIES: &str = "replies";

/// What a window reviews: a repo and the `base...head` range in it — the one
/// value the load pipeline threads from the CLI flags or the picker down to
/// the loader. `repo` is a path; `base`/`head` are whatever the loader
/// resolves: revs, or the `INDEX`/`WORKTREE` sentinels.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Target {
    pub repo: String,
    pub base: String,
    pub head: String,
}
