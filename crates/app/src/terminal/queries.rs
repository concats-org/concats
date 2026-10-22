//! What a program asks the terminal about itself, answered: its colours and
//! the size of its text area. The parser thread queues the questions in the
//! session's report while it holds the term; the UI thread answers them,
//! where the term can be locked again and the theme read.

use alacritty_terminal::vte::ansi::Rgb;

use super::{Report, Session, colors, input, size, term};
use crate::theme::Theme;

/// Answer what a program asked about the terminal: its colours (what it set
/// itself, else the theme; Codex blends its shimmer from the foreground and
/// background it asks for) and the size of its text area.
pub fn answer(session: Session, report: Report, theme: &Theme) {
    if report.color_queries.is_empty() && report.size_queries.is_empty() {
        return;
    }
    let (Some(shared), Some(size)) = (term(session), size(session)) else {
        return;
    };
    let colors: Vec<u8> = {
        let term = shared.lock();
        report
            .color_queries
            .into_iter()
            .flat_map(|(index, reply)| {
                let [r, g, b] = colors::slot(index, term.colors(), theme).bytes();
                reply(Rgb { r, g, b }).into_bytes()
            })
            .collect()
    };
    let sizes = report
        .size_queries
        .into_iter()
        .flat_map(|reply| reply(size.into()).into_bytes());
    input(session, colors.into_iter().chain(sizes).collect());
}
