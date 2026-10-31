//! The command palette's rows: every command a window can run, with the key
//! it has, ranked against what was typed.

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

use crate::keymap::{COMMANDS, Command, Keymap, label};

/// The palette's rows: each command `available` lets this window run, and
/// its title with the keys bound to it, `Close All Tabs  ⌘K ⌘W`.
pub(crate) fn rows(keymap: &Keymap, available: impl Fn(Command) -> bool) -> Vec<(Command, String)> {
    COMMANDS
        .iter()
        .filter(|(command, ..)| available(*command))
        .map(|(command, _, title)| {
            let row = match keymap.binding(*command) {
                Some(keys) => format!("{title}  {}", label(keys)),
                None => (*title).to_string(),
            };
            (*command, row)
        })
        .collect()
}

/// `rows` ranked for `query`, best first, the way VS Code's and Zed's
/// palettes match: every word of the query a fuzzy subsequence of the title.
/// With nothing typed, the commands run lately come first, most recent
/// first, then the rest in table order.
pub(crate) fn rank(query: &str, rows: &[(Command, String)], recent: &[Command]) -> Vec<Command> {
    if query.trim().is_empty() {
        let lately = recent
            .iter()
            .filter(|command| rows.iter().any(|(row, _)| row == *command));
        let rest = rows
            .iter()
            .map(|(command, _)| command)
            .filter(|command| !recent.contains(command));
        return lately.chain(rest).copied().collect();
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, Command)> = rows
        .iter()
        .filter_map(|(command, _)| {
            let title = Utf32Str::new(command.title(), &mut buf);
            Some((pattern.score(title, &mut matcher)?, *command))
        })
        .collect();
    // Stable, so equal scores keep the table's order.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(_, command)| command).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::review_doc::Stream;

    #[test]
    fn a_query_ranks_the_command_it_spells_first() {
        let rows = rows(&Keymap::default(), |_| true);
        assert_eq!(rank("clo al", &rows, &[])[0], Command::CloseAllTabs);
        assert_eq!(rank("new term", &rows, &[])[0], Command::NewTerminal);
        assert_eq!(rank("zzzz", &rows, &[]), []);
    }

    #[test]
    fn with_nothing_typed_recent_commands_come_first() {
        let rows = rows(&Keymap::default(), |_| true);
        let recent = [Command::Reload, Command::NewFile];
        let ranked = rank("", &rows, &recent);
        assert_eq!(ranked[..2], recent);
        assert_eq!(ranked.len(), rows.len());
    }

    #[test]
    fn rows_show_each_commands_current_key_and_only_what_can_run() {
        let rows = rows(&Keymap::default(), |_| true);
        assert_eq!(rows.len(), COMMANDS.len());
        let row = |rows: &[(Command, String)], command| {
            rows.iter()
                .find(|(c, _)| *c == command)
                .map(|(_, row)| row.clone())
        };
        assert_eq!(
            row(&rows, Command::CloseAllTabs).as_deref(),
            Some("Close All Tabs  ⌘K ⌘W")
        );
        let rebound = Keymap::new(&BTreeMap::from([(
            "cmd-e".to_string(),
            "close_all_tabs".to_string(),
        )]))
        .expect("valid");
        let rebound_rows = super::rows(&rebound, |_| true);
        assert!(
            row(&rebound_rows, Command::CloseAllTabs).is_some_and(|row| row.contains("⌘E")),
            "{rebound_rows:?}"
        );
        let no_guide = super::rows(&Keymap::default(), |c| {
            c != Command::OpenStream(Stream::Guide)
        });
        assert_eq!(row(&no_guide, Command::OpenStream(Stream::Guide)), None);
    }
}
