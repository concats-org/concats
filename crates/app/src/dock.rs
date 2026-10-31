//! The dock: the tab↔stream mapping, tab (re)creation, terminal-tab detection,
//! and the per-repo layout persistence under `.git/`. Free functions over
//! makepad's `Dock`, called by the `App`/`ReviewPane` widgets in `main.rs`.

use std::{collections::HashMap, io::Write, path::Path};

use concats_config::{Area, Placement};

#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::{
    makepad_widgets::{makepad_micro_serde::*, *},
    review_doc::Stream,
};

/// The document stream a dock tab renders. `Stream` names the stream, the dock's
/// `LiveId` names the tab. `open` lists the document's open file tabs: a File
/// tab is its dock tab, so the caller has to say which ids are live, or a
/// terminal's tab would answer to one.
pub(crate) fn model_tab_of(tab_id: LiveId, open: &[u64]) -> Option<Stream> {
    crate::review_doc::STREAMS
        .into_iter()
        .chain(open.iter().map(|id| Stream::File(*id)))
        .find(|t| stream_tab_spec(*t).id == tab_id)
}

#[derive(Clone, Copy)]
pub(crate) struct TabSpec<'a> {
    pub id: LiveId,
    pub kind: LiveId,
    pub template: LiveId,
    pub title: &'a str,
}

// NOTE: the one tab↔stream table; every dock lookup goes through it.
// coland-ignore: @coland/high_call_degree
pub(crate) fn stream_tab_spec(tab: Stream) -> TabSpec<'static> {
    let (id, kind, template, title) = match tab {
        Stream::Guide => (id!(guide_tab), id!(GuidePane), id!(GuideTab), "Guide"),
        Stream::Sessions => (
            id!(sessions_tab),
            id!(SessionsPane),
            id!(SessionsTab),
            "Sessions",
        ),
        Stream::Commits => (
            id!(commits_tab),
            id!(CommitsPane),
            id!(CommitsTab),
            "Commits",
        ),
        Stream::Files => (id!(files_tab), id!(FilesPane), id!(FilesTab), "File Diff"),
        Stream::Comments => (
            id!(comments_tab),
            id!(CommentsPane),
            id!(CommentsTab),
            "Comments",
        ),
        Stream::File(tab) => (LiveId(tab), id!(FilePane), id!(FileTab), "File"),
    };
    TabSpec {
        id,
        kind,
        template,
        title,
    }
}

/// The Settings tab. It is a File tab like any other: the settings are a file,
/// and a stream of their own would answer every question the file path already
/// answers — how it lowers, how you type into it, how it saves — a second time.
/// Only the content and what saving means differ.
pub(crate) fn settings_tab_spec() -> TabSpec<'static> {
    TabSpec {
        id: id!(settings_tab),
        kind: id!(SettingsPane),
        template: id!(SettingsTab),
        title: "Settings",
    }
}

/// The dock tab that shows `path`, so opening a file twice selects the tab it
/// is already in rather than stacking a second copy of it.
pub(crate) fn file_tab_id(path: &str) -> LiveId {
    LiveId::from_str(&format!("file:{path}"))
}

/// The file browser's tab. It starts in the right panel and moves like any
/// other.
pub(crate) fn browser_tab_spec() -> TabSpec<'static> {
    TabSpec {
        id: id!(browser_tab),
        kind: id!(BrowserPane),
        template: id!(BrowserTab),
        title: "Files",
    }
}

/// The permanent terminal tab, the one the `>_` button opens.
pub(crate) fn terminal_tab_spec() -> TabSpec<'static> {
    TabSpec {
        id: id!(terminal_tab),
        kind: id!(TerminalPane),
        template: id!(TerminalTab),
        title: "Terminal",
    }
}

/// The tab bar a view opens in when it isn't open yet: its area's.
pub(crate) fn area_bar(area: Area) -> LiveId {
    match area {
        Area::Center => id!(main_tabs),
        Area::Left => id!(left_tabs),
        Area::Right => id!(right_tabs),
        Area::Bottom => id!(bottom_tabs),
    }
}

/// The area `stream`'s tab opens in, as placed in Settings.
pub(crate) fn stream_area(placement: &Placement, stream: Stream) -> Area {
    match stream {
        Stream::Guide => placement.guide,
        Stream::Sessions => placement.sessions,
        Stream::Commits => placement.commits,
        Stream::Files => placement.diff,
        Stream::Comments => placement.comments,
        Stream::File(_) => placement.file,
    }
}

/// The stream tabs placed in `area`, File Diff first: what opens in that area
/// goes beside them, next to File Diff when it can.
pub(crate) fn stream_tabs_in(area: Area) -> Vec<LiveId> {
    let placement = crate::theme::active_placement();
    std::iter::once(Stream::Files)
        .chain(crate::review_doc::STREAMS)
        .filter(|stream| stream_area(&placement, *stream) == area)
        .map(|stream| stream_tab_spec(stream).id)
        .collect()
}

/// The terminal tabs, the permanent one first. Judged by kind, so it holds for
/// the permanent tab and every `+`-created session alike.
pub(crate) fn terminal_tabs(dock: &DockRef) -> Vec<LiveId> {
    let mut tabs: Vec<LiveId> = dock
        .clone_state()
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, item)| {
            matches!(item, DockItem::Tab { kind, .. } if *kind == terminal_tab_spec().kind)
        })
        .map(|(tab, _)| tab)
        .collect();
    tabs.sort_by_key(|tab| (*tab != terminal_tab_spec().id, tab.0));
    tabs
}

/// Open `spec`'s tab unless it is open: in the bar of the first open sibling,
/// else in `home`. Opening does not select it, because reconciling the
/// streams opens tabs nobody asked to look at.
pub(crate) fn open_tab(
    cx: &mut Cx,
    dock: &DockRef,
    spec: TabSpec<'_>,
    siblings: &[LiveId],
    home: LiveId,
) {
    if dock.find_tab_bar_of_tab(spec.id).is_some() {
        return;
    }
    let bar = siblings
        .iter()
        .find_map(|tab| dock.find_tab_bar_of_tab(*tab))
        .map_or(home, |(bar, _)| bar);
    // NOTE: keeping File Diff last gives reopened streams a stable position.
    let after = dock
        .find_tab_bar_of_tab(id!(files_tab))
        .filter(|(files_bar, _)| *files_bar == bar)
        .and_then(|(_, position)| position.checked_sub(1));
    dock.create_tab(
        cx,
        bar,
        spec.id,
        spec.kind,
        spec.title.into(),
        spec.template,
        after,
    );
}

/// Whether `tab` is anywhere under `item`, however that part of the dock has
/// been split since: how a panel knows it holds a tab.
pub(crate) fn holds(items: &HashMap<LiveId, DockItem>, item: LiveId, tab: LiveId) -> bool {
    match items.get(&item) {
        Some(DockItem::Splitter { a, b, .. }) => holds(items, *a, tab) || holds(items, *b, tab),
        Some(DockItem::Tabs { tabs, .. }) => tabs.contains(&tab),
        _ => false,
    }
}

/// The tab each tab bar under `item` has selected: what that part of the dock
/// shows.
pub(crate) fn shown_tabs(items: &HashMap<LiveId, DockItem>, item: LiveId) -> Vec<LiveId> {
    match items.get(&item) {
        Some(DockItem::Splitter { a, b, .. }) => [*a, *b]
            .into_iter()
            .flat_map(|side| shown_tabs(items, side))
            .collect(),
        Some(DockItem::Tabs { tabs, selected, .. }) => {
            tabs.get(*selected).copied().into_iter().collect()
        }
        _ => Vec::new(),
    }
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
/// arrangement, splits, panel sizes.
///
/// The `-5` marks the shape of the dock tree and the names it saves: `-5` added
/// the left panel and named the side panels by side. A saved tab names its
/// template and kind, and one this version no longer declares restores as an
/// unstyled tab or a dead pane. A layout from another version has a name we
/// don't read, so it is ignored; one reset beats a dock missing a panel.
const LAYOUT_FILE: &str = "concats-app-layout-5.ron";

// NOTE: bare `pub`, not `pub(crate)`: makepad's hand-rolled SerRon/DeRon derive
// parser does `eat_ident("pub")` then expects `struct`, so it accepts private
// or bare-`pub` structs but chokes on a `pub(crate)` visibility group.
#[derive(SerRon, DeRon)]
pub struct DockLayoutRon {
    pub dock_items: HashMap<LiveId, DockItem>,
    /// The size each panel reopens at.
    pub left_restore: f64,
    pub right_restore: f64,
    pub bottom_restore: f64,
}

/// Load the layout persisted for this repo. A file whose tabs reference a kind
/// this version doesn't declare (older/newer app) is rejected whole — a dead
/// pane is worse than the default layout.
pub(crate) fn load_layout(git_dir: &Path) -> Option<DockLayoutRon> {
    let text = std::fs::read_to_string(git_dir.join(LAYOUT_FILE)).ok()?;
    let mut state = DockLayoutRon::deserialize_ron(&text).ok()?;
    let known: Vec<LiveId> = crate::review_doc::STREAMS
        .into_iter()
        .map(|stream| stream_tab_spec(stream).kind)
        .chain([
            id!(FilePane),
            id!(TerminalPane),
            id!(SettingsPane),
            id!(BrowserPane),
        ])
        .collect();
    for item in state.dock_items.values() {
        if let DockItem::Tab { kind, .. } = item
            && !known.contains(kind)
        {
            return None;
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

pub(crate) fn save_layout(git_dir: &Path, state: &DockLayoutRon) -> std::io::Result<()> {
    let mut pending = tempfile::NamedTempFile::new_in(git_dir)?;
    pending.write_all(state.serialize_ron().as_bytes())?;
    pending.as_file().sync_all()?;
    pending
        .persist(git_dir.join(LAYOUT_FILE))
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_with_a_comments_tab_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let spec = stream_tab_spec(Stream::Comments);
        let items = HashMap::from([
            (
                id!(root),
                DockItem::Tabs {
                    tabs: vec![spec.id],
                    selected: 0,
                    closable: false,
                    hide_tab_bar: false,
                },
            ),
            (
                spec.id,
                DockItem::Tab {
                    name: spec.title.into(),
                    template: spec.template,
                    kind: spec.kind,
                },
            ),
        ]);
        let layout = DockLayoutRon {
            dock_items: items,
            left_restore: 260.0,
            right_restore: 260.0,
            bottom_restore: 220.0,
        };
        save_layout(dir.path(), &layout).unwrap();
        let layout = load_layout(dir.path()).expect("the saved layout loads");
        assert!(layout.dock_items.contains_key(&spec.id));
    }

    /// A panel is its splitter's side, however the user has split it since.
    #[test]
    fn a_tab_in_a_split_of_the_panel_is_in_the_panel() {
        let tabs = |tabs: Vec<LiveId>| DockItem::Tabs {
            tabs,
            selected: 0,
            closable: false,
            hide_tab_bar: false,
        };
        let splitter = |a, b| DockItem::Splitter {
            axis: SplitterAxis::Horizontal,
            align: SplitterAlign::FromB(0.0),
            a,
            b,
        };
        let items = HashMap::from([
            (id!(body_split), splitter(id!(main_tabs), id!(lower))),
            (id!(lower), splitter(id!(bottom_tabs), id!(more))),
            (id!(main_tabs), tabs(vec![id!(files_tab), id!(moved)])),
            (id!(bottom_tabs), tabs(vec![id!(terminal_tab)])),
            (id!(more), tabs(vec![id!(split)])),
        ]);
        assert!(holds(&items, id!(lower), id!(terminal_tab)));
        assert!(holds(&items, id!(lower), id!(split)));
        assert!(!holds(&items, id!(lower), id!(moved)));
        assert_eq!(
            shown_tabs(&items, id!(lower)),
            [id!(terminal_tab), id!(split)]
        );
    }
}
