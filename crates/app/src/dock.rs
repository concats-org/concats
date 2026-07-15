//! The dock: the tab↔stream mapping, tab (re)creation, terminal-tab detection,
//! and the per-repo layout persistence under `.git/`. Free functions over
//! makepad's `Dock`, called by the `App`/`ReviewPane` widgets in `main.rs`.

use std::{collections::HashMap, path::Path};

use crate::{
    makepad_widgets::{makepad_micro_serde::*, *},
    review_doc::Tab,
};

/// The document stream a dock tab renders. `Tab` names the stream, the dock's
/// `LiveId` names the tab. `open` lists the document's open file tabs: a File
/// tab is its dock tab, so the caller has to say which ids are live, or a
/// terminal's tab would answer to one.
pub(crate) fn model_tab_of(tab_id: LiveId, open: &[u64]) -> Option<Tab> {
    [
        Tab::Guide,
        Tab::Sessions,
        Tab::Commits,
        Tab::Files,
        Tab::Comments,
    ]
    .into_iter()
    .chain(open.iter().map(|id| Tab::File(*id)))
    .find(|t| stream_tab_spec(*t).0 == tab_id)
}

/// One stream tab's dock wiring: (tab id, content kind, tab template, name).
pub(crate) fn stream_tab_spec(tab: Tab) -> (LiveId, LiveId, LiveId, &'static str) {
    match tab {
        Tab::Guide => (id!(guide_tab), id!(GuidePane), id!(GuideTab), "Guide"),
        Tab::Sessions => (
            id!(sessions_tab),
            id!(SessionsPane),
            id!(SessionsTab),
            "Sessions",
        ),
        Tab::Commits => (
            id!(commits_tab),
            id!(CommitsPane),
            id!(CommitsTab),
            "Commits",
        ),
        Tab::Files => (id!(files_tab), id!(FilesPane), id!(FilesTab), "File Diff"),
        Tab::Comments => (
            id!(comments_tab),
            id!(CommentsPane),
            id!(CommentsTab),
            "Comments",
        ),
        // The tab id comes from the variant, not from a table: there is one
        // per open file. The name is a placeholder — `open_file_tab` titles
        // each tab with the file it holds.
        Tab::File(tab) => (LiveId(tab), id!(FilePane), id!(FileTab), "File"),
    }
}

/// The Settings tab. It is a File tab like any other: the settings are a file,
/// and a stream of their own would answer every question the file path already
/// answers — how it lowers, how you type into it, how it saves — a second time.
/// Only the content and what saving means differ.
pub(crate) fn settings_tab_id() -> LiveId {
    id!(settings_tab)
}

/// The dock tab that shows `path`, so opening a file twice selects the tab it
/// is already in rather than stacking a second copy of it.
pub(crate) fn file_tab_id(path: &str) -> LiveId {
    LiveId::from_str(&format!("file:{path}"))
}

/// (Re)create a stream tab: anchored on whichever stream tabs are still open
/// (before File Diff when it is, after another otherwise), so re-opened tabs
/// land in the bar the user keeps their views in.
pub(crate) fn create_stream_tab(cx: &mut Cx, dock: &DockRef, tab: Tab) {
    let (tab_id, kind, template, name) = stream_tab_spec(tab);
    if dock.find_tab_bar_of_tab(tab_id).is_some() {
        return;
    }
    let (bar, insert_after) = if let Some((bar, pos)) = dock.find_tab_bar_of_tab(id!(files_tab)) {
        (bar, pos.checked_sub(1))
    } else if let Some((bar, pos)) = [id!(guide_tab), id!(sessions_tab), id!(commits_tab)]
        .into_iter()
        .find_map(|t| dock.find_tab_bar_of_tab(t))
    {
        (bar, Some(pos))
    } else {
        (id!(main_tabs), None)
    };
    dock.create_tab(cx, bar, tab_id, kind, name.into(), template, insert_after);
}

/// Is this dock tab a terminal pane? Judged by its kind, so it holds for the
/// permanent tab and every `+`-created session alike.
pub(crate) fn is_terminal_dock_tab(dock: &DockRef, tab_id: LiveId) -> bool {
    dock.clone_state().is_some_and(|items| {
        matches!(items.get(&tab_id), Some(DockItem::Tab { kind, .. }) if *kind == id!(TerminalPane))
    })
}

/// The dragged dock tab, recovered from the drag payload: the dock marks its
/// own tabs with `internal_id`; external drags (files from Finder) have none.
pub(crate) fn drag_source_tab_id(items: &[DragItem]) -> Option<LiveId> {
    match items {
        [DragItem::FilePath { internal_id, .. }] => *internal_id,
        _ => None,
    }
}

/// The dock layout, persisted per repo next to the review store in `.git/`: tab
/// arrangement, splits, terminal-panel height.
///
/// The `-2` marks the shape of the dock tree. The file browser added an outer
/// horizontal splitter, and a layout saved before that restores a `root` with
/// no room for the browser. A layout from another version has a name we don't
/// read, so it is ignored; one reset beats a dock missing a panel.
const LAYOUT_FILE: &str = "concats-app-layout-2.ron";

// NOTE: bare `pub`, not `pub(crate)`: makepad's hand-rolled SerRon/DeRon derive
// parser does `eat_ident("pub")` then expects `struct`, so it accepts private
// or bare-`pub` structs but chokes on a `pub(crate)` visibility group.
#[derive(SerRon, DeRon)]
pub struct DockLayoutRon {
    pub dock_items: HashMap<LiveId, DockItem>,
    pub bottom_restore: f64,
    pub sidebar_restore: f64,
}

/// Load the layout persisted for this repo. A file whose tabs reference a kind
/// this version doesn't declare (older/newer app) is rejected whole — a dead
/// pane is worse than the default layout.
pub(crate) fn load_layout(git_dir: &Path) -> Option<DockLayoutRon> {
    let text = std::fs::read_to_string(git_dir.join(LAYOUT_FILE)).ok()?;
    let mut state = DockLayoutRon::deserialize_ron(&text).ok()?;
    let known = [
        id!(GuidePane),
        id!(SessionsPane),
        id!(CommitsPane),
        id!(FilesPane),
        id!(FilePane),
        id!(TerminalPane),
        id!(SettingsPane),
        id!(SidebarPane),
    ];
    for item in state.dock_items.values() {
        if let DockItem::Tab { kind, .. } = item {
            if !known.contains(kind) {
                return None;
            }
        }
    }
    state.dock_items.get(&id!(root))?;

    // File tabs are session state, not layout. A tab's id is a hash of its path
    // and nothing stores the path, so a restored tab has no file behind it: it
    // draws an empty pane until you open that file again. Restored ones used to
    // pile up across runs with no way to get rid of them. The Settings tab is
    // the same; it comes back from its button.
    let mut dropped: Vec<LiveId> = Vec::new();
    state.dock_items.retain(|id, item| match item {
        DockItem::Tab { kind, .. } if *kind == id!(FilePane) || *kind == id!(SettingsPane) => {
            dropped.push(*id);
            false
        }
        _ => true,
    });
    for item in state.dock_items.values_mut() {
        if let DockItem::Tabs { tabs, selected, .. } = item {
            tabs.retain(|t| !dropped.contains(t));
            *selected = (*selected).min(tabs.len().saturating_sub(1));
        }
    }
    Some(state)
}

pub(crate) fn save_layout(
    git_dir: &Path,
    dock_items: HashMap<LiveId, DockItem>,
    restores: (f64, f64),
) {
    let state = DockLayoutRon {
        dock_items,
        bottom_restore: restores.0,
        sidebar_restore: restores.1,
    };
    let _ = std::fs::write(git_dir.join(LAYOUT_FILE), state.serialize_ron());
}
