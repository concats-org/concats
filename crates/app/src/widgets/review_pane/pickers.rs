//! The header's two pickers: the repo name drops down the recent repos and a
//! folder dialog, and the range chip drops down a filter over the refs. What
//! either one picks, the pane loads.

use concats_diff::load;

use super::ReviewPane;
#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::{
    makepad_widgets::*,
    review_doc::ReviewDoc,
    service::{ReviewCmd, review},
};

/// The fixed row slots of the diff-picker dropdown. A handful is plenty:
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
    /// Open the diff picker under the header chip: empty filter, refs listed,
    /// keyboard in the input.
    pub fn combo_open(&mut self, cx: &mut Cx) {
        self.repo_close(cx);
        self.view.view(cx, ids!(combo_panel)).set_visible(cx, true);
        let input = self.view.text_input(cx, ids!(combo_input));
        input.set_text(cx, "");
        input.set_key_focus(cx);
        self.combo_filter(cx, "");
        self.view.redraw(cx);
    }

    fn combo_close(&mut self, cx: &mut Cx) {
        self.view.view(cx, ids!(combo_panel)).set_visible(cx, false);
        self.view.redraw(cx);
    }

    /// Open the repo picker under the repo name: the recent repos as rows (most
    /// recent first, shown by basename), plus the always-present "Open dir…".
    pub fn repo_open(&mut self, cx: &mut Cx) {
        self.combo_close(cx);
        review().send(ReviewCmd::LoadRecents);
        self.show_recents(cx);
        self.view.view(cx, ids!(repo_panel)).set_visible(cx, true);
        self.view.redraw(cx);
    }

    pub fn set_recents(&mut self, cx: &mut Cx, recents: Vec<String>) {
        self.recents = recents;
        self.show_recents(cx);
        self.view.redraw(cx);
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

    /// Fill the row slots with the refs matching the query.
    fn combo_filter(&mut self, cx: &mut Cx, query: &str) {
        let query = query.trim().to_lowercase();
        let rows = combo_rows!();
        let matches: Vec<String> = self.state().read(|d| {
            d.refs
                .iter()
                .filter(|r| query.is_empty() || r.to_lowercase().contains(&query))
                .take(rows.len())
                .cloned()
                .collect()
        });
        for (i, ids) in rows.into_iter().enumerate() {
            let row = self.view.button(cx, ids);
            match matches.get(i) {
                Some(name) => {
                    row.set_text(cx, name);
                    row.set_visible(cx, true);
                }
                None => row.set_visible(cx, false),
            }
        }
        self.view.redraw(cx);
    }

    /// Load `base…head` of `repo`, and close the diff picker.
    fn combo_load(&mut self, cx: &mut Cx, target: concats_state::Target) {
        self.combo_close(cx);
        self.start_load(cx, target, None, Some("loading…"));
    }

    /// What to (re)load: the open repo at the open range, else the branch so
    /// far for a repo the app hasn't diffed yet.
    fn current_target(&self) -> concats_state::Target {
        let target = self.state().read(ReviewDoc::target);
        if target.base.is_empty() {
            concats_state::Target {
                base: load::branch_point(std::path::Path::new(&target.repo)),
                head: load::WORKTREE_REV.into(),
                ..target
            }
        } else {
            target
        }
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
        let repo = path.to_string_lossy().into_owned();
        self.combo_load(
            cx,
            concats_state::Target {
                repo,
                ..self.current_target()
            },
        );
    }

    /// The header Load button and the repo picker: the name toggles it, a
    /// recent row loads that repo, and "Open dir…" browses for another. Loads
    /// keep the current range.
    pub(super) fn handle_repo_picker(&mut self, cx: &mut Cx, actions: &Actions) {
        // The header Load button reloads the open repo at its current range.
        if self.view.button(cx, ids!(load_button)).clicked(actions) {
            let target = self.current_target();
            self.combo_load(cx, target);
        }

        if self.view.button(cx, ids!(repo_button)).clicked(actions) {
            if self.view.view(cx, ids!(repo_panel)).visible() {
                self.repo_close(cx);
            } else {
                self.repo_open(cx);
            }
        }
        for (i, ids) in repo_rows!().into_iter().enumerate() {
            if self.view.button(cx, ids).clicked(actions)
                && let Some(repo) = self.recents.get(i).cloned()
            {
                let target = concats_state::Target {
                    repo,
                    ..self.current_target()
                };
                self.repo_close(cx);
                self.combo_load(cx, target);
            }
        }
        if self.view.button(cx, ids!(open_dir_row)).clicked(actions) {
            self.repo_close(cx);
            self.pick_repo(cx);
        }
    }

    /// The diff picker: chip toggles, typing filters, Enter accepts a typed
    /// ref (or an explicit base...head), a row click picks that ref as the
    /// base — both against HEAD.
    pub(super) fn handle_diff_picker(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.view.button(cx, ids!(range_button)).clicked(actions) {
            if self.view.view(cx, ids!(combo_panel)).visible() {
                self.combo_close(cx);
            } else {
                self.combo_open(cx);
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
            if text.is_empty() {
                self.combo_close(cx);
            } else {
                let target = target_of(self.state().read(|d| d.repo.clone()), &text);
                self.combo_load(cx, target);
            }
        }
        if self.view.text_input(cx, ids!(combo_input)).escaped(actions) {
            self.combo_close(cx);
        }
        for ids in combo_rows!() {
            if self.view.button(cx, ids).clicked(actions) {
                let text = self.view.button(cx, ids).text();
                if !text.is_empty() {
                    // A row is usually a base ref (diffed against HEAD), but
                    // the worktree presets are full `base...head` ranges.
                    let target = target_of(self.state().read(|d| d.repo.clone()), &text);
                    self.combo_load(cx, target);
                }
            }
        }
    }
}

/// `repo` at a picked or typed range: `base...head` (or `base..head`), else a
/// base ref diffed against HEAD. Git ref names cannot contain `..`, so a ref is
/// never split.
// NOTE: handle_diff_picker reads both a typed and a picked range through it.
// coland-ignore: @coland/single_caller_private_function
fn target_of(repo: String, text: &str) -> concats_state::Target {
    let (base, head) = match text.split_once("...").or_else(|| text.split_once("..")) {
        Some((base, head)) => (base.trim().to_string(), head.trim().to_string()),
        None => (text.to_string(), "HEAD".to_string()),
    };
    concats_state::Target { repo, base, head }
}
