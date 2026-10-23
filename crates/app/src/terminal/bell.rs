//! A terminal's bell as a desktop notification: how an agent says it has
//! finished while you are looking at another window.

use std::{
    path::Path,
    time::{Duration, Instant},
};

/// How long one terminal's bells merge into one notification, so a program
/// that rings in a loop raises one rather than one per bell.
const MERGE: Duration = Duration::from_secs(5);

/// The identity a notification is posted under: the app's own when it runs
/// bundled, the installed app's for a development build, which has none.
// NOTE: the identifier in the packager table of crates/app/Cargo.toml.
#[cfg(target_os = "macos")]
const BUNDLE: &str = "dev.concats.app";

/// One terminal's bell: what its notifications are named after, and when it
/// last raised one.
pub struct Bell {
    /// The directory the shell started in, by name: the window's repo.
    place: String,
    /// What the program last called itself (OSC 0/2).
    title: Option<String>,
    rung: Option<Instant>,
}

impl Bell {
    pub fn new(cwd: &Path) -> Self {
        Self {
            place: cwd.file_name().map_or_else(
                || cwd.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            title: None,
            rung: None,
        }
    }

    /// What the program now calls itself. An empty title is none.
    pub fn titled(&mut self, title: &str) {
        self.title = (!title.is_empty()).then(|| title.to_owned());
    }

    /// The program rang at `now`: a notification, unless this terminal
    /// raised one moments ago.
    pub fn ring(&mut self, now: Instant) {
        if !due(self.rung, now) {
            return;
        }
        self.rung = Some(now);
        let title = self.title.as_deref().unwrap_or("Terminal");
        notify(format!("{} — {title}", self.place));
    }
}

/// Whether a bell at `now` raises a notification, given when this terminal's
/// last one did.
fn due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.duration_since(last) >= MERGE)
}

/// Raise a notification titled `title` for a bell, off the caller's thread:
/// delivery can block.
fn notify(title: String) {
    #[cfg(target_os = "macos")]
    std::thread::spawn(move || {
        static IDENTITY: std::sync::Once = std::sync::Once::new();
        IDENTITY.call_once(|| {
            if let Err(error) = mac_notification_sys::set_application(BUNDLE) {
                eprintln!("concats-app: cannot post as {BUNDLE}: {error}");
            }
        });
        if let Err(error) = mac_notification_sys::Notification::new()
            .title(&title)
            .message("Bell")
            .send()
        {
            eprintln!("concats-app: cannot raise a notification: {error}");
        }
    });
    #[cfg(not(target_os = "macos"))]
    drop(title);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_of_bells_raises_one_notification() {
        let start = Instant::now();
        let mut last = None;
        let mut raised = 0;
        for i in 0..20 {
            let now = start + Duration::from_millis(10 * i);
            if due(last, now) {
                last = Some(now);
                raised += 1;
            }
        }
        assert_eq!(raised, 1);
        let later = start + MERGE + Duration::from_millis(500);
        assert!(due(last, later), "a bell after the burst rings again");
    }
}
