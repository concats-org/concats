//! The recent repos behind the header's picker: rows in the app database,
//! newest first, capped. One read, one write, no state kept in between.
//!
//! App state, not repo state: the list spans repos, so `app.db` lives next to
//! `config.toml` rather than under any `.git`.

use std::time::{SystemTime, UNIX_EPOCH};

/// How many recent repos to keep. A short list is all a picker wants.
const MAX: usize = 5;

/// The app database, WAL'd and schema'd. `None` (with a warning) when the
/// config home is unusable: callers lose the affordance, nothing else.
fn open_app_db() -> Option<rusqlite::Connection> {
    let Some(dir) = concats_config::config_dir() else {
        eprintln!("concats-app: no platform configuration directory");
        return None;
    };
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
            "CREATE TABLE IF NOT EXISTS recents(
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

/// The persisted recent repo paths, most-recent first.
pub fn recents() -> Vec<String> {
    let Some(conn) = open_app_db() else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare("SELECT path FROM recents ORDER BY last_opened DESC LIMIT ?1")
    else {
        return Vec::new();
    };
    stmt.query_map([MAX], |row| row.get::<_, String>(0))
        .into_iter()
        .flatten()
        .flatten()
        .collect()
}

/// Move `repo` to the front of the recents (dedup by primary key, capped)
/// and persist.
pub fn record_recent(repo: &str) {
    let Some(conn) = open_app_db() else { return };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let _ = conn.execute(
        "INSERT OR REPLACE INTO recents(path, last_opened) VALUES (?1, ?2)",
        (repo, now),
    );
    let _ = conn.execute(
        "DELETE FROM recents WHERE path NOT IN
         (SELECT path FROM recents ORDER BY last_opened DESC LIMIT ?1)",
        [MAX],
    );
}
