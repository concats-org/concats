//! The notifications programs send (OSC 9/777) as desktop notifications:
//! how an agent says it needs you while you are looking at another window. A
//! plain bell raises none; it only marks the terminal's tab.

use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

use makepad_widgets::makepad_platform::thread::SignalToUI;

use super::Session;

/// How long the same notification stays merged into the one it repeats, so a
/// program that rings in a loop raises one rather than one per bell.
const MERGE: Duration = Duration::from_secs(5);

/// How long any notification holds off the next, so a program that sends them
/// in a loop does not bury the desktop.
const FLOOD: Duration = Duration::from_secs(1);

/// The identity a notification is posted under: the app's own when it runs
/// bundled, the installed app's for a development build, which has none.
// NOTE: the identifier in the packager table of crates/app/Cargo.toml.
#[cfg(target_os = "macos")]
const BUNDLE: &str = "dev.concats.app";

/// The terminals whose notification was clicked, waiting for the UI to bring
/// them to the front.
static CLICKED: Mutex<Vec<Session>> = Mutex::new(Vec::new());

/// The terminals whose notification was clicked since the UI last looked.
pub fn take_clicked() -> Vec<Session> {
    std::mem::take(&mut CLICKED.lock().unwrap())
}

/// A click on `session`'s notification: from the poster thread, or from the
/// hooks, since a test can't click a real one.
pub fn clicked(session: Session) {
    CLICKED.lock().unwrap().push(session);
    SignalToUI::set_ui_signal();
}

/// One terminal's notifications: what they are named after, and the last one
/// raised.
pub struct Bell {
    /// The terminal a click on one of them brings to the front.
    session: Session,
    /// The directory the shell started in, by name: the window's repo.
    place: String,
    /// What the program last called itself (OSC 0/2).
    title: Option<String>,
    /// When the last notification went up, and what it said.
    raised: Option<(Instant, String)>,
}

impl Bell {
    pub fn new(session: Session, cwd: &Path) -> Self {
        Self {
            session,
            place: cwd.file_name().map_or_else(
                || cwd.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            title: None,
            raised: None,
        }
    }

    /// What the program now calls itself. An empty title is none.
    pub fn titled(&mut self, title: &str) {
        self.title = (!title.is_empty()).then(|| title.to_owned());
    }

    /// The program asked at `now` to show `body`, under `title` when it gave
    /// one (OSC 9/777) — unless that would repeat or flood.
    pub fn notify(&mut self, now: Instant, title: Option<&str>, body: &str) {
        let said = format!("{}\n{body}", title.unwrap_or_default());
        if !due(self.raised.as_ref(), now, &said) {
            return;
        }
        self.raised = Some((now, said));
        let terminal = self.title.as_deref().unwrap_or("Terminal");
        post(
            self.session,
            format!("{} — {terminal}", self.place),
            title.map(str::to_owned),
            body.to_owned(),
        );
    }
}

/// Whether a notification saying `said` at `now` goes up, given the last one
/// raised: not the same one within [`MERGE`], nor any within [`FLOOD`].
fn due(raised: Option<&(Instant, String)>, now: Instant, said: &str) -> bool {
    raised.is_none_or(|(at, last)| {
        let since = now.duration_since(*at);
        since >= MERGE || (since >= FLOOD && last != said)
    })
}

/// Post a notification off the caller's thread, which then waits for it to
/// be clicked or cleared: a click queues `session` for the UI to bring to the
/// front. The program's own title, when it gave one, goes under the
/// terminal's.
// NOTE: each notification still in Notification Center holds its thread; the
// merge and flood limits above keep those few.
fn post(session: Session, title: String, subtitle: Option<String>, message: String) {
    #[cfg(target_os = "macos")]
    std::thread::spawn(move || {
        static IDENTITY: std::sync::Once = std::sync::Once::new();
        IDENTITY.call_once(|| {
            if let Err(error) = mac_notification_sys::set_application(BUNDLE) {
                eprintln!("concats-app: cannot post as {BUNDLE}: {error}");
            }
        });
        let mut notification = mac_notification_sys::Notification::new();
        notification
            .title(&title)
            .message(&message)
            .default_sound()
            .wait_for_click(true);
        if let Some(subtitle) = &subtitle {
            notification.subtitle(subtitle);
        }
        match notification.send() {
            Ok(mac_notification_sys::NotificationResponse::Click) => clicked(session),
            Ok(_) => {}
            Err(error) => eprintln!("concats-app: cannot raise a notification: {error}"),
        }
    });
    #[cfg(not(target_os = "macos"))]
    drop((session, title, subtitle, message));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `said` at each offset (ms from now) and count what goes up.
    fn raised(at: &[(u64, &str)]) -> usize {
        let start = Instant::now();
        let mut last = None;
        at.iter()
            .filter(|(ms, said)| {
                let now = start + Duration::from_millis(*ms);
                let up = due(last.as_ref(), now, said);
                if up {
                    last = Some((now, (*said).to_owned()));
                }
                up
            })
            .count()
    }

    #[test]
    fn a_burst_of_bells_raises_one_notification() {
        let burst: Vec<_> = (0..20).map(|i| (10 * i, "Bell")).collect();
        assert_eq!(raised(&burst), 1);
        assert_eq!(
            raised(&[(0, "Bell"), (5_500, "Bell")]),
            2,
            "after the burst"
        );
    }

    #[test]
    fn different_messages_each_go_up_once_the_flood_passes() {
        assert_eq!(raised(&[(0, "done"), (1_500, "needs you")]), 2);
        assert_eq!(raised(&[(0, "done"), (500, "needs you")]), 1, "a flood");
        assert_eq!(raised(&[(0, "done"), (1_500, "done")]), 1, "a repeat");
    }
}
