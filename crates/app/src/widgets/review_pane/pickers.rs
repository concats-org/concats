//! The header's pickers, a breadcrumb `repo → base / head`. The repo name
//! drops down this repository's checkouts, the recent repos and a folder
//! dialog; a checkout picked there opens on its branch so far. The base and the
//! head each drop down a filter over what that end can be, so either changes
//! alone; the head's presets, or a typed `base...head`, change both.

use concats_diff::load::{self, INDEX_REV, WORKTREE_REV};
use concats_state::Target;

use super::{ReviewPane, ReviewPaneAction};
#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::{
    keymap::Command,
    makepad_widgets::*,
    palette,
    review_doc::{ReviewDoc, Stream},
    service::{ReviewCmd, review},
};

/// Which end of the range a range picker edits.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum RangeEnd {
    Base,
    #[default]
    Head,
}

/// One row of a range picker: the text it shows and the range it loads.
pub(super) type RangeRow = (String, Target);

/// The fixed row slots of the range-picker dropdown. A handful is plenty:
/// the filter narrows the list as you type.
macro_rules! combo_rows {
    () => {
        [
            ids!(combo_row0),
            ids!(combo_row1),
            ids!(combo_row2),
            ids!(combo_row3),
            ids!(combo_row4),
            ids!(combo_row5),
            ids!(combo_row6),
            ids!(combo_row7),
        ]
    };
}

/// The worktree row slots of the repo picker.
macro_rules! worktree_rows {
    () => {
        [
            ids!(worktree_row0),
            ids!(worktree_row1),
            ids!(worktree_row2),
            ids!(worktree_row3),
            ids!(worktree_row4),
            ids!(worktree_row5),
            ids!(worktree_row6),
            ids!(worktree_row7),
        ]
    };
}

/// The recent-repo row slots of the repo picker (capped to match `recents::MAX`).
macro_rules! repo_rows {
    () => {
        [
            ids!(repo_row0),
            ids!(repo_row1),
            ids!(repo_row2),
            ids!(repo_row3),
            ids!(repo_row4),
        ]
    };
}

impl ReviewPane {
    /// Open the range picker for one end, under that end's chip: empty
    /// filter, everything that end can be listed, keyboard in the input.
    pub(super) fn combo_open(&mut self, cx: &mut Cx, end: RangeEnd) {
        self.repo_close(cx);
        let (target, refs) = self.state().read(|d| (d.target(), d.refs.clone()));
        let branch_point = load::branch_point(std::path::Path::new(&target.repo));
        self.range_end = end;
        self.range_rows = range_rows(end, &target, &refs, &branch_point);
        let chip = match end {
            RangeEnd::Base => ids!(base_button),
            RangeEnd::Head => ids!(head_button),
        };
        let left =
            self.view.widget(cx, chip).area().rect(cx).pos.x - self.view.area().rect(cx).pos.x;
        let panel = self.view.view(cx, ids!(combo_panel));
        if let Some(mut panel) = panel.borrow_mut() {
            panel.walk.margin.left = left;
        }
        self.palette = None;
        self.show_combo(cx, "filter, a rev, or base...head");
    }

    /// ⇧⌘P: the command palette, in the range picker's dropdown over the
    /// middle of the window, listing what this window can run.
    pub(super) fn palette_open(&mut self, cx: &mut Cx) {
        self.repo_close(cx);
        let available = self.state().read(|d| {
            crate::review_doc::STREAMS
                .into_iter()
                .filter(|stream| *stream == Stream::Files || !d.stream(*stream).is_empty())
                .collect::<Vec<_>>()
        });
        self.palette = Some(palette::rows(
            &crate::theme::active_keymap(),
            |command| match command {
                Command::OpenStream(stream) => available.contains(&stream),
                _ => true,
            },
        ));
        self.palette_return = cx.key_focus();
        let width = self.view.area().rect(cx).size.x;
        if let Some(mut panel) = self.view.view(cx, ids!(combo_panel)).borrow_mut() {
            panel.walk.margin.left = (width - 340.0).max(0.0) / 2.0;
        }
        self.show_combo(cx, "Run a command");
    }

    /// Show the dropdown empty, keyboard in its filter, every row listed.
    fn show_combo(&mut self, cx: &mut Cx, placeholder: &str) {
        self.view.view(cx, ids!(combo_panel)).set_visible(cx, true);
        let input = self.view.text_input(cx, ids!(combo_input));
        input.set_text(cx, "");
        input.set_empty_text(cx, placeholder.to_string());
        input.set_key_focus(cx);
        self.focus_combo = true;
        self.combo_filter(cx, "");
        self.view.redraw(cx);
    }

    /// Close the dropdown; the palette gives the keyboard back to where it
    /// was.
    fn combo_close(&mut self, cx: &mut Cx) {
        self.view.view(cx, ids!(combo_panel)).set_visible(cx, false);
        if self.palette.take().is_some() {
            cx.set_key_focus(self.palette_return);
        }
        self.view.redraw(cx);
    }

    /// Run the command a palette row names, in this window, and remember it
    /// for the next time the palette opens.
    fn palette_run(&mut self, cx: &mut Cx, command: Command) {
        self.combo_close(cx);
        self.recent_commands.retain(|recent| *recent != command);
        self.recent_commands.insert(0, command);
        self.recent_commands.truncate(8);
        cx.widget_action(self.widget_uid(), ReviewPaneAction::Run(command));
    }

    /// Open the repo picker under the repo name: this repository's checkouts
    /// when it has more than one, the recent repos (most recent first, shown by
    /// basename), and the always-present "Open dir…".
    pub fn repo_open(&mut self, cx: &mut Cx) {
        self.combo_close(cx);
        self.picker_cursor = None;
        review().send(ReviewCmd::LoadRecents);
        self.show_worktrees(cx);
        self.show_recents(cx);
        self.view.view(cx, ids!(repo_panel)).set_visible(cx, true);
        self.view.redraw(cx);
    }

    pub fn set_recents(&mut self, cx: &mut Cx, recents: Vec<String>) {
        self.recents = recents;
        self.show_recents(cx);
        self.view.redraw(cx);
    }

    /// The checkout rows, one per worktree, shown when there is more than one.
    fn show_worktrees(&self, cx: &mut Cx) {
        let (worktrees, repo) = self.state().read(|d| (d.worktrees.clone(), d.repo.clone()));
        let shown = load::discover(std::path::Path::new(&repo));
        let labels = worktree_labels(&worktrees, shown.as_deref());
        for (i, ids) in worktree_rows!().into_iter().enumerate() {
            let row = self.view.button(cx, ids);
            match labels.get(i) {
                Some(label) => {
                    row.set_text(cx, label);
                    row.set_visible(cx, true);
                }
                None => row.set_visible(cx, false),
            }
        }
        self.view
            .view(cx, ids!(worktree_rule))
            .set_visible(cx, !labels.is_empty());
    }

    fn show_recents(&self, cx: &mut Cx) {
        for (i, ids) in repo_rows!().into_iter().enumerate() {
            let row = self.view.button(cx, ids);
            match self.recents.get(i) {
                Some(path) => {
                    let name = std::path::Path::new(path)
                        .file_name()
                        .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
                    row.set_text(cx, &name);
                    row.set_visible(cx, true);
                }
                None => row.set_visible(cx, false),
            }
        }
    }

    fn repo_close(&mut self, cx: &mut Cx) {
        self.view.view(cx, ids!(repo_panel)).set_visible(cx, false);
        self.view.redraw(cx);
    }

    /// Fill the row slots with the rows matching the query.
    fn combo_filter(&mut self, cx: &mut Cx, query: &str) {
        self.picker_cursor = None;
        if let Some(rows) = &self.palette {
            let ranked = palette::rank(query, rows, &self.recent_commands);
            let labels: Vec<String> = ranked
                .iter()
                .filter_map(|command| rows.iter().find(|(c, _)| c == command))
                .map(|(_, label)| label.clone())
                .collect();
            for (i, ids) in combo_rows!().into_iter().enumerate() {
                let row = self.view.button(cx, ids);
                match labels.get(i) {
                    Some(label) => {
                        row.set_text(cx, label);
                        row.set_visible(cx, true);
                    }
                    None => row.set_visible(cx, false),
                }
            }
            self.palette_shown = ranked;
            return self.view.redraw(cx);
        }
        let query = query.trim().to_lowercase();
        let slots = combo_rows!();
        let matches: Vec<&RangeRow> = self
            .range_rows
            .iter()
            .filter(|(label, _)| query.is_empty() || label.to_lowercase().contains(&query))
            .take(slots.len())
            .collect();
        for (i, ids) in slots.into_iter().enumerate() {
            let row = self.view.button(cx, ids);
            match matches.get(i) {
                Some((label, _)) => {
                    row.set_text(cx, label);
                    row.set_visible(cx, true);
                }
                None => row.set_visible(cx, false),
            }
        }
        self.range_shown = matches
            .into_iter()
            .map(|(_, target)| target.clone())
            .collect();
        self.view.redraw(cx);
    }

    /// Load `base…head` of `repo`, and close the range picker.
    fn combo_load(&mut self, cx: &mut Cx, target: Target) {
        self.combo_close(cx);
        self.start_load(cx, target, None, Some("loading…"));
    }

    /// Open a native folder dialog under the repo name and load whatever repo
    /// the user picks, keeping the open range. Blocks on the modal panel —
    /// fine, we are on the main thread and there is nothing else to draw.
    fn pick_repo(&mut self, cx: &mut Cx) {
        let mut dialog = rfd::FileDialog::new().set_title("Choose a repository");
        // Start the browser next to the current repo, not the cwd.
        let current = self.state().read(|d| d.repo.clone());
        if let Some(parent) = std::path::Path::new(&current).parent()
            && !parent.as_os_str().is_empty()
        {
            dialog = dialog.set_directory(parent);
        }
        let picked = dialog.pick_folder();
        // The modal swallowed the row's mouse-up/hover-out; snap it back so the
        // "Open dir…" row doesn't reopen stuck in its pressed style.
        self.view.button(cx, ids!(open_dir_row)).reset_hover(cx);
        let Some(path) = picked else {
            return; // cancelled
        };
        self.combo_load(cx, branch_so_far(path.to_string_lossy().into_owned()));
    }

    /// Load the open repo again at its current range; one nothing has loaded
    /// yet loads on its branch so far.
    pub(super) fn reload(&mut self, cx: &mut Cx) {
        let target = self.state().read(ReviewDoc::target);
        let target = if target.base.is_empty() {
            branch_so_far(target.repo)
        } else {
            target
        };
        self.combo_load(cx, target);
    }

    /// The rows of whichever picker is open, as shown, top to bottom.
    fn picker_rows(&self, cx: &mut Cx) -> Vec<&'static [LiveId]> {
        let repo = self.view.view(cx, ids!(repo_panel)).visible();
        let combo = self.view.view(cx, ids!(combo_panel)).visible();
        let rows: Vec<&'static [LiveId]> = if repo {
            worktree_rows!()
                .into_iter()
                .chain(repo_rows!())
                .chain([ids!(open_dir_row)])
                .map(|ids| ids as &[LiveId])
                .collect()
        } else if combo {
            combo_rows!().map(|ids| ids as &[LiveId]).to_vec()
        } else {
            Vec::new()
        };
        rows.into_iter()
            .filter(|ids| self.view.button(cx, ids).visible())
            .collect()
    }

    /// Arrow keys walk the open picker's rows, highlighting one, and Enter
    /// picks the highlighted row; with none, the range picker's input takes
    /// Enter for what was typed. Escape closes the repo picker (the range
    /// picker's input closes its own). Whether a picker took the key.
    pub(super) fn picker_key(&mut self, cx: &mut Cx, ke: &KeyEvent) -> bool {
        let rows = self.picker_rows(cx);
        if rows.is_empty() {
            return false;
        }
        let repo = self.view.view(cx, ids!(repo_panel)).visible();
        match ke.key_code {
            KeyCode::ArrowDown | KeyCode::ArrowUp => {
                let at = walk(self.picker_cursor, ke.key_code, rows.len());
                self.picker_cursor = Some(at);
                for (i, ids) in rows.iter().enumerate() {
                    let row = self.view.button(cx, ids);
                    if i != at {
                        row.reset_hover(cx);
                    } else if let Some(mut row) = row.borrow_mut() {
                        row.animator_cut(cx, ids!(hover.on));
                    }
                }
                self.view.redraw(cx);
                true
            }
            KeyCode::ReturnKey if repo || self.picker_cursor.is_some() => {
                self.pick_highlighted(cx, &rows, repo);
                true
            }
            KeyCode::Escape if repo => {
                self.repo_close(cx);
                true
            }
            _ => false,
        }
    }

    /// Pick the row the arrows highlighted, in the repo picker or a range
    /// picker.
    fn pick_highlighted(&mut self, cx: &mut Cx, rows: &[&[LiveId]], repo: bool) {
        let Some(at) = self.picker_cursor else {
            return;
        };
        if repo {
            if let Some(row) = rows.get(at) {
                self.pick_repo_row(cx, row);
            }
        } else {
            self.pick_combo_row(cx, at);
        }
    }

    /// Pick the dropdown's row at `at`: run its command in the palette, load
    /// its range in a range picker.
    fn pick_combo_row(&mut self, cx: &mut Cx, at: usize) {
        if self.palette.is_some() {
            if let Some(command) = self.palette_shown.get(at).copied() {
                self.palette_run(cx, command);
            }
        } else if let Some(target) = self.range_shown.get(at).cloned() {
            self.combo_load(cx, target);
        }
    }

    /// Open what a repo picker row names: a checkout or a recent repo on its
    /// branch so far, or the folder dialog.
    fn pick_repo_row(&mut self, cx: &mut Cx, row: &[LiveId]) {
        self.repo_close(cx);
        if row == *ids!(open_dir_row) {
            return self.pick_repo(cx);
        }
        let at = |rows: &[&[LiveId; 1]]| rows.iter().position(|ids| ids.as_slice() == row);
        let checkout = at(&worktree_rows!()).and_then(|i| {
            self.state().read(|d| {
                d.worktrees
                    .get(i)
                    .map(|(path, _)| path.to_string_lossy().into_owned())
            })
        });
        let picked =
            checkout.or_else(|| at(&repo_rows!()).and_then(|i| self.recents.get(i).cloned()));
        if let Some(repo) = picked {
            self.combo_load(cx, branch_so_far(repo));
        }
    }

    /// The header Load button and the repo picker: the name toggles it, a
    /// checkout or recent row loads that one on its branch so far, and "Open
    /// dir…" browses for another.
    pub(super) fn handle_repo_picker(&mut self, cx: &mut Cx, actions: &Actions) {
        // The header Load button reloads the open repo at its current range.
        if self.view.button(cx, ids!(load_button)).clicked(actions) {
            self.reload(cx);
        }

        if self.view.button(cx, ids!(repo_button)).clicked(actions) {
            if self.view.view(cx, ids!(repo_panel)).visible() {
                self.repo_close(cx);
            } else {
                self.repo_open(cx);
            }
        }
        let clicked = worktree_rows!()
            .into_iter()
            .chain(repo_rows!())
            .chain([ids!(open_dir_row)])
            .find(|ids| self.view.button(cx, *ids).clicked(actions));
        if let Some(row) = clicked {
            self.pick_repo_row(cx, row);
        }
    }

    /// The range pickers: each end's chip toggles its picker, typing filters,
    /// Enter takes a typed rev for that end (or a typed `base...head` for both),
    /// and a row loads the range it shows.
    pub(super) fn handle_diff_picker(&mut self, cx: &mut Cx, actions: &Actions) {
        for (chip, end) in [
            (ids!(base_button), RangeEnd::Base),
            (ids!(head_button), RangeEnd::Head),
        ] {
            if self.view.button(cx, chip).clicked(actions) {
                let open = self.view.view(cx, ids!(combo_panel)).visible();
                if open && self.range_end == end {
                    self.combo_close(cx);
                } else {
                    self.combo_open(cx, end);
                }
            }
        }
        if let Some(query) = self.view.text_input(cx, ids!(combo_input)).changed(actions) {
            self.combo_filter(cx, &query);
        }
        if let Some((text, _)) = self
            .view
            .text_input(cx, ids!(combo_input))
            .returned(actions)
        {
            let text = text.trim().to_string();
            if self.palette.is_some() {
                // Enter runs the best match.
                self.pick_combo_row(cx, 0);
            } else if text.is_empty() {
                self.combo_close(cx);
            } else {
                let target = typed(self.state().read(ReviewDoc::target), self.range_end, &text);
                self.combo_load(cx, target);
            }
        }
        if self.view.text_input(cx, ids!(combo_input)).escaped(actions) {
            self.combo_close(cx);
        }
        if let Some(at) = combo_rows!()
            .into_iter()
            .position(|ids| self.view.button(cx, ids).clicked(actions))
        {
            self.pick_combo_row(cx, at);
        }
    }
}

/// Where an arrow key moves a picker's highlight among `len` rows: the first
/// or last row from none, then one row, stopping at the ends.
fn walk(cursor: Option<usize>, key: KeyCode, len: usize) -> usize {
    let last = len.saturating_sub(1);
    match (cursor, key) {
        (None, KeyCode::ArrowDown) => 0,
        (None, _) => last,
        (Some(at), KeyCode::ArrowDown) => (at + 1).min(last),
        (Some(at), _) => at.saturating_sub(1),
    }
}

/// What a range picker offers for one end, each row with the range it loads.
/// The head's picker leads with the presets that set both ends. Nothing the
/// loader refuses is offered: INDEX is a base only for a WORKTREE head, and a
/// commit is a head only after a commit base.
fn range_rows(
    end: RangeEnd,
    target: &Target,
    refs: &[String],
    branch_point: &str,
) -> Vec<RangeRow> {
    let range = |base: &str, head: &str| Target {
        repo: target.repo.clone(),
        base: base.to_string(),
        head: head.to_string(),
    };
    match end {
        RangeEnd::Base => {
            let index = (target.head == WORKTREE_REV).then_some(INDEX_REV);
            let recent = ["HEAD~1", "HEAD~5", "HEAD~10", "HEAD~20"];
            index
                .into_iter()
                .chain(refs.iter().map(String::as_str))
                .chain(recent)
                .map(|base| (base.to_string(), range(base, &target.head)))
                .collect()
        }
        RangeEnd::Head => {
            let presets = [
                ("Branch so far", branch_point, WORKTREE_REV),
                ("Uncommitted", "HEAD", WORKTREE_REV),
                ("Unstaged", INDEX_REV, WORKTREE_REV),
                ("Last commit", "HEAD~1", "HEAD"),
            ]
            .map(|(name, base, head)| (format!("{name} · {base}...{head}"), range(base, head)));
            let commits = (target.base != INDEX_REV)
                .then(|| std::iter::once("HEAD").chain(refs.iter().map(String::as_str)))
                .into_iter()
                .flatten();
            let heads = std::iter::once(WORKTREE_REV)
                .chain(commits)
                .map(|head| (head.to_string(), range(&target.base, head)));
            presets.into_iter().chain(heads).collect()
        }
    }
}

/// A typed range: `base...head` (or `base..head`) sets both ends, anything else
/// is a rev for the end being edited. Git ref names cannot contain `..`, so a
/// ref is never split.
fn typed(target: Target, end: RangeEnd, text: &str) -> Target {
    match (
        text.split_once("...").or_else(|| text.split_once("..")),
        end,
    ) {
        (Some((base, head)), _) => Target {
            base: base.trim().to_string(),
            head: head.trim().to_string(),
            ..target
        },
        (None, RangeEnd::Base) => Target {
            base: text.to_string(),
            ..target
        },
        (None, RangeEnd::Head) => Target {
            head: text.to_string(),
            ..target
        },
    }
}

/// `repo` at the branch so far: what a checkout opens on.
fn branch_so_far(repo: String) -> Target {
    Target {
        base: load::branch_point(std::path::Path::new(&repo)),
        head: load::WORKTREE_REV.into(),
        repo,
    }
}

/// What each checkout has checked out and where, the one this window shows
/// marked. None for a repository with a single checkout: there is nowhere
/// else to go.
fn worktree_labels(
    worktrees: &[(std::path::PathBuf, String)],
    shown: Option<&std::path::Path>,
) -> Vec<String> {
    if worktrees.len() < 2 {
        return Vec::new();
    }
    worktrees
        .iter()
        .map(|(path, checked_out)| {
            // The last two components tell `…/276b/concats` from `…/2ae6/concats`.
            let skip = path.iter().count().saturating_sub(2);
            let tail: std::path::PathBuf = path.iter().skip(skip).collect();
            let open = if shown == Some(path.as_path()) {
                "  (open)"
            } else {
                ""
            };
            format!("{checked_out} · {}{open}", tail.display())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn range(base: &str, head: &str) -> Target {
        Target {
            repo: "/work/concats".into(),
            base: base.into(),
            head: head.into(),
        }
    }

    #[test]
    fn an_end_picks_alone_and_a_preset_or_a_typed_range_sets_both() {
        let refs = ["main".to_string(), "oh/app".to_string()];
        let from = range("main", WORKTREE_REV);
        let pick = |end, label: &str| {
            range_rows(end, &from, &refs, "main")
                .into_iter()
                .find(|(shown, _)| shown == label)
                .map(|(_, target)| target)
        };
        assert_eq!(pick(RangeEnd::Head, "HEAD"), Some(range("main", "HEAD")));
        assert_eq!(
            pick(RangeEnd::Base, "oh/app"),
            Some(range("oh/app", WORKTREE_REV))
        );

        let presets: Vec<Target> = range_rows(RangeEnd::Head, &from, &refs, "main")
            .into_iter()
            .take(4)
            .map(|(_, target)| target)
            .collect();
        assert_eq!(
            presets,
            [
                range("main", WORKTREE_REV),
                range("HEAD", WORKTREE_REV),
                range(INDEX_REV, WORKTREE_REV),
                range("HEAD~1", "HEAD"),
            ]
        );

        assert_eq!(
            typed(from.clone(), RangeEnd::Head, "v1...v2"),
            range("v1", "v2")
        );
        assert_eq!(
            typed(from.clone(), RangeEnd::Head, "v1..v2"),
            range("v1", "v2")
        );
        assert_eq!(typed(from, RangeEnd::Base, "v1"), range("v1", WORKTREE_REV));
    }

    /// The loader only diffs INDEX (or WORKTREE) against the working tree; no
    /// row a picker offers may ask it for anything else.
    #[test]
    fn no_offered_row_pairs_the_index_with_a_commit_head() {
        let refs = ["main".to_string()];
        let opened = [
            (INDEX_REV, WORKTREE_REV),
            ("main", WORKTREE_REV),
            ("main", "HEAD"),
            ("HEAD~1", "HEAD"),
        ];
        for (base, head) in opened {
            for end in [RangeEnd::Base, RangeEnd::Head] {
                for (label, to) in range_rows(end, &range(base, head), &refs, "main") {
                    let worktree_only = to.base == INDEX_REV || to.base == WORKTREE_REV;
                    assert!(
                        !worktree_only || to.head == WORKTREE_REV,
                        "{base}...{head} offers {label}: {}...{}",
                        to.base,
                        to.head
                    );
                }
            }
        }
    }

    #[test]
    fn checkouts_read_by_branch_and_the_tail_of_their_path() {
        let worktrees = [
            (PathBuf::from("/work/concats"), "main".to_string()),
            (PathBuf::from("/codex/276b/concats"), "8a8ed6e".to_string()),
        ];
        assert_eq!(
            worktree_labels(&worktrees, Some(Path::new("/codex/276b/concats"))),
            ["main · work/concats", "8a8ed6e · 276b/concats  (open)"]
        );
        assert_eq!(worktree_labels(&worktrees[..1], None), Vec::<String>::new());
    }

    #[test]
    fn arrows_walk_a_picker_and_stop_at_its_ends() {
        let (down, up) = (KeyCode::ArrowDown, KeyCode::ArrowUp);
        assert_eq!(walk(None, down, 3), 0);
        assert_eq!(walk(None, up, 3), 2);
        assert_eq!(walk(Some(1), down, 3), 2);
        assert_eq!(walk(Some(2), down, 3), 2);
        assert_eq!(walk(Some(0), up, 3), 0);
    }
}
