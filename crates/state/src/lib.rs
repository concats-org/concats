//! The app's own state: `app.db` in the config home, and the one row of it a
//! CLI reads.
//!
//! A crate because two binaries meet here. The app publishes what each window
//! has open; `concats`, run from that window's terminal, reads it back. That is
//! what lets a bare `concats comments add …` review the diff on screen — across
//! range switches, and without two windows crossing wires.
//!
//! The env carries the window's IDENTITY (`CONCATS_APP_WINDOW`), never its
//! state. An id cannot go stale when the reviewer switches ranges; a range
//! baked into the env at spawn time would.
//!
//! App state, not repo state: a window can switch repos, so this lives next to
//! `config.toml` rather than under any `.git`. The recents list sits in the
//! same file.

/// What a window reviews: a repo and the `base...head` range in it — the one
/// value the load pipeline threads from the CLI flags or the picker down to
/// the loader. `repo` is a path; `base`/`head` are whatever the loader
/// resolves: revs, or the `INDEX`/`WORKTREE` sentinels.
#[derive(Clone, Default)]
pub struct Target {
    pub repo: String,
    pub base: String,
    pub head: String,
}

/// This window's identity, exported into its terminals as `CONCATS_APP_WINDOW`.
/// One window per process today, so one id per process; a process that opens
/// several mints one per window. Two windows on one repo get two ids, where a
/// repo-keyed range would collide.
#[must_use]
pub fn window_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{}-{nanos}", std::process::id())
    })
}

/// The app database, WAL'd and schema'd. `None` (with a warning) when the
/// config home is unusable: callers lose the affordance, nothing else.
#[must_use]
pub fn open_app_db() -> Option<rusqlite::Connection> {
    let dir = concats_config::config_dir();
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("concats-app: cannot create {}: {error}", dir.display());
        return None;
    }
    let open = || -> rusqlite::Result<rusqlite::Connection> {
        let conn = rusqlite::Connection::open(dir.join("app.db"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS windows(
               id TEXT PRIMARY KEY,
               repo TEXT NOT NULL,
               base TEXT NOT NULL,
               head TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS recents(
               path TEXT PRIMARY KEY,
               last_opened INTEGER NOT NULL
             );",
        )?;
        Ok(conn)
    };
    match open() {
        Ok(conn) => Some(conn),
        Err(error) => {
            eprintln!("concats-app: cannot open app state db: {error}");
            None
        }
    }
}

/// Publish `target` as what window `window` has open: one row per window,
/// rewritten on every load. Fire-and-forget — the row is an affordance for
/// bare CLI commands, not a correctness requirement.
///
/// NOTE: a window that crashes leaves its row behind. Nothing can name it (its
/// terminals died with it), so the row is inert; at one per launch it is not
/// worth a liveness probe.
pub fn publish_window_range(conn: &rusqlite::Connection, window: &str, target: &Target) {
    let write = conn.execute(
        "INSERT OR REPLACE INTO windows(id, repo, base, head) VALUES (?1,?2,?3,?4)",
        (window, &target.repo, &target.base, &target.head),
    );
    if let Err(error) = write {
        eprintln!("concats-app: cannot publish open range: {error}");
    }
}

/// The range window `window` has open.
#[must_use]
pub fn window_range(conn: &rusqlite::Connection, window: &str) -> Option<Target> {
    conn.query_row(
        "SELECT repo, base, head FROM windows WHERE id = ?1",
        [window],
        |row| {
            Ok(Target {
                repo: row.get(0)?,
                base: row.get(1)?,
                head: row.get(2)?,
            })
        },
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_reads_back_what_it_published_and_nothing_else() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE windows(id TEXT PRIMARY KEY, repo TEXT NOT NULL,
             base TEXT NOT NULL, head TEXT NOT NULL);",
        )
        .unwrap();
        let target = Target {
            repo: "/some/repo".into(),
            base: "HEAD~1".into(),
            head: "HEAD".into(),
        };

        assert!(window_range(&conn, "window-a").is_none());
        publish_window_range(&conn, "window-a", &target);
        let read = window_range(&conn, "window-a").unwrap();
        assert_eq!(
            (read.repo.as_str(), read.base.as_str(), read.head.as_str()),
            ("/some/repo", "HEAD~1", "HEAD")
        );
        assert!(window_range(&conn, "window-b").is_none());

        // A reload republishes: the row is rewritten, never duplicated.
        publish_window_range(
            &conn,
            "window-a",
            &Target {
                base: "HEAD~2".into(),
                ..target
            },
        );
        assert_eq!(window_range(&conn, "window-a").unwrap().base, "HEAD~2");
    }
}
