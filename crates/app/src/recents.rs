//! The recent repos behind the header's picker: rows in the app database,
//! newest first, capped. One read, one write, no state kept in between.

use std::time::{SystemTime, UNIX_EPOCH};

use concats_state::open_app_db;

/// How many recent repos to keep. A short list is all a picker wants.
const MAX: usize = 5;

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
        .map(|d| d.as_secs())
        .unwrap_or(0);
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
