//! The desktop notifications a program asks its terminal for: OSC 9 (iTerm2's)
//! and OSC 777 (rxvt's), which agents send when they finish or need you.
//! alacritty's parser drops both, so a second parser reads the same bytes for
//! them — vte's own, at the level below alacritty's.

use alacritty_terminal::vte::{Parser, Perform};

/// What a program asked to show: its text, under a title when it gave one.
#[derive(Debug, PartialEq, Eq)]
pub struct Notification {
    pub title: Option<String>,
    pub body: String,
}

/// The notifications in one session's output. Bytes arrive in whatever chunks
/// the PTY hands over, so one may end inside a sequence; the parser keeps it
/// until the rest arrives.
#[derive(Default)]
pub struct Reader {
    parser: Parser,
    found: Found,
}

impl Reader {
    /// The notifications `bytes` completed.
    pub fn notifications(&mut self, bytes: &[u8]) -> Vec<Notification> {
        self.parser.advance(&mut self.found, bytes);
        std::mem::take(&mut self.found.0)
    }
}

#[derive(Default)]
struct Found(Vec<Notification>);

impl Perform for Found {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // The parser splits on every `;`, the text's own included.
        let text = |fields: &[&[u8]]| String::from_utf8_lossy(&fields.join(&b';')).into_owned();
        let notification = match params {
            [b"9", rest @ ..] if !rest.is_empty() && !conemu(&text(rest)) => Notification {
                title: None,
                body: text(rest),
            },
            [b"777", b"notify", title, rest @ ..] if !rest.is_empty() => Notification {
                title: Some(String::from_utf8_lossy(title).into_owned()),
                body: text(rest),
            },
            _ => return,
        };
        self.0.push(notification);
    }
}

/// Whether OSC 9's `data` is one of the `ConEmu` commands rather than a
/// message — a progress report (`4;…`) above all, which would otherwise notify
/// on every update. Ghostty's rules: a command needs its exact form, anything
/// else is text.
// NOTE: ConEmu's command forms, a table of their own; inline, they would bury
// the OSC match's one arm per sequence.
// coland-ignore: @coland/single_caller_private_function
fn conemu(data: &str) -> bool {
    let (command, rest) = data
        .split_once(';')
        .map_or((data, None), |(command, rest)| (command, Some(rest)));
    match command {
        "1" | "2" | "3" | "6" | "7" | "8" | "9" | "11" => rest.is_some(),
        "4" => rest.is_some_and(|rest| rest.starts_with(['0', '1', '2', '3', '4'])),
        "10" => rest.is_none_or(|rest| rest.starts_with(['0', '1', '2', '3'])),
        "5" | "12" => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: [Notification; 0] = [];

    fn read(bytes: &[u8]) -> Vec<Notification> {
        Reader::default().notifications(bytes)
    }

    fn titled(title: &str, body: &str) -> Notification {
        Notification {
            title: Some(title.to_owned()),
            body: body.to_owned(),
        }
    }

    fn untitled(body: &str) -> Notification {
        Notification {
            title: None,
            body: body.to_owned(),
        }
    }

    #[test]
    fn rxvt_notify_carries_a_title_and_its_text_whole() {
        assert_eq!(
            read(b"\x1b]777;notify;Claude Code;done\x07"),
            [titled("Claude Code", "done")]
        );
        assert_eq!(
            read(b"\x1b]777;notify;Codex;a; b\x1b\\"),
            [titled("Codex", "a; b")],
            "ST-terminated, a `;` in the text"
        );
        assert_eq!(read(b"\x1b]777;notify;no body\x07"), NONE);
        assert_eq!(read(b"\x1b]777;preexec;x;y\x07"), NONE);
    }

    #[test]
    fn iterm_notify_is_text_but_conemu_commands_are_not() {
        assert_eq!(
            read(b"\x1b]9;Agent turn complete\x07"),
            [untitled("Agent turn complete")]
        );
        for command in ["4;1;50", "4;0", "1;100", "3;tab", "9;/tmp", "10", "12", "5"] {
            assert_eq!(
                read(format!("\x1b]9;{command}\x07").as_bytes()),
                NONE,
                "{command}"
            );
        }
        // Not quite a command: Ghostty shows these as text, so do we.
        for text in ["4", "4;5", "3", "10;4", "11", "1a"] {
            assert_eq!(
                read(format!("\x1b]9;{text}\x07").as_bytes()),
                [untitled(text)]
            );
        }
    }

    #[test]
    fn a_notification_split_across_reads_arrives_once_whole() {
        let mut reader = Reader::default();
        assert_eq!(reader.notifications(b"output\x1b]777;notify;Cla"), NONE);
        assert_eq!(
            reader.notifications(b"ude Code;needs you\x07more"),
            [titled("Claude Code", "needs you")]
        );
    }

    #[test]
    fn other_sequences_are_not_notifications() {
        assert_eq!(
            read(b"\x1b]0;title\x07\x1b]52;c;Y29w\x07\x07\x1b[31mred"),
            NONE
        );
    }
}
