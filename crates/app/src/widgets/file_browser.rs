//! The file browser: the tree at the range's "to" position, docked right.
//!
//! A diff shows what a range changed; a review also needs the code around it,
//! and a place to say something about a file the change *should* have touched.
//! So this lists every blob of the head — changed or not — dots what the range
//! did to it, and asks the pane to open whichever one is picked.
//!
//! The tree is a function of the range, so it is rebuilt only when a load
//! lands. Which files the head has comes from `ReviewDoc::tree`; what happened
//! to them comes from the File Diff stream, which covers the range exactly
//! once (the same stance `review_doc::changed_keys` takes).

use std::collections::{HashMap, HashSet};

use concats_diff::Row;

use crate::{
    makepad_widgets::{
        file_tree::{FileTree, GitStatusDotKind},
        *,
    },
    FrameData,
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.FONT
    use mod.widgets.C_CHROME
    use mod.widgets.C_BORDER
    use mod.widgets.C_TEXT
    use mod.widgets.C_DIM
    use mod.widgets.C_FAINT
    use mod.widgets.C_ELEMENT_HOVER

    mod.widgets.FileBrowser = #(FileBrowser::register_widget(vm)) {
        // Fill, not a fixed width: the pane's width is the splitter's to set,
        // and a fixed one leaves a dead strip whenever the sidebar is dragged
        // wider. The 260 default lives on the splitter that opens it.
        width: Fill
        height: Fill
        flow: Down

        // Which revision everything below is at. Without it a listing of HEAD~3
        // and one of the working copy look the same.
        header := SolidView {
            width: Fill height: 26
            draw_bg.color: C_CHROME
            flow: Right
            align: Align{x: 0.0, y: 0.5}
            padding: Inset{left: 12 right: 12}
            rev_chip := Label {
                width: Fill
                text: ""
                draw_text.color: C_DIM
                draw_text.text_style: FONT{font_size: 8.25}
            }
        }
        SolidView { width: Fill height: 1 draw_bg.color: C_BORDER }

        tree := FileTree {
            width: Fill height: Fill
            node_height: 22.0

            // The stock node templates read makepad's own theme — a different
            // grey, and a zebra stripe this app doesn't use. Flatten both rows
            // onto the chrome so the sidebar reads as one surface.
            file_node +: {
                padding: Inset{left: 12}
                draw_bg +: {
                    color_1: C_CHROME
                    color_2: C_CHROME
                    color_active: C_ELEMENT_HOVER
                }
                draw_text +: {
                    color: C_DIM
                    color_active: C_TEXT
                    text_style: FONT{font_size: 8.25}
                }
            }
            folder_node +: {
                padding: Inset{left: 12}
                draw_bg +: {
                    color_1: C_CHROME
                    color_2: C_CHROME
                    color_active: C_ELEMENT_HOVER
                }
                draw_text +: {
                    color: C_TEXT
                    color_active: C_TEXT
                    text_style: FONT{font_size: 8.25}
                }
                draw_icon +: { color: C_FAINT  color_active: C_DIM }
            }
            // The run below the last row is part of the same surface. The
            // stock filler paints the zebra; colors arrive as uniforms because
            // module `let`s don't reach shader fns.
            filler +: {
                color_chrome: uniform(mod.app_theme.color_chrome)
                pixel: fn() { return self.color_chrome }
            }
        }
    }
}

/// One node of the head tree. Folders exist only as path separators in the
/// listing, so they are materialized here rather than read.
struct Node {
    /// Repo-relative path: what a click opens, and `LiveId::from_str`'s input.
    path: String,
    name: String,
    /// `None` for a blob.
    children: Option<Vec<LiveId>>,
    status: GitStatusDotKind,
}

/// What the browser asks of the pane. The pane owns the dock, so opening a tab
/// is not this widget's to do.
#[derive(Clone, Debug, Default)]
pub enum FileBrowserAction {
    OpenFile(String),
    #[default]
    None,
}

#[derive(Script, ScriptHook, Widget)]
pub struct FileBrowser {
    #[deref]
    view: View,
    #[rust]
    nodes: HashMap<LiveId, Node>,
    /// The repo row every path hangs off — see `build`.
    #[rust]
    root: LiveId,
    /// The document generation `nodes` was built for. The tree is a function
    /// of the range, so opening a file (which bumps `rows_rev`, not
    /// `generation`) must not rebuild it.
    #[rust]
    built: u64,
}

/// What this range did to each path it touched, as a dot.
///
/// NOTE: `Mixed` and not `Deleted` for what went — the file tree's shader
/// blends the `Deleted` dot to zero alpha, so it would draw nothing at all.
/// `Mixed` is the same red.
fn status_dots(
    files_rows: &[Row],
    added: &HashSet<String>,
    at_head: &HashSet<&str>,
) -> HashMap<String, GitStatusDotKind> {
    files_rows
        .iter()
        .filter_map(|r| match r {
            Row::FileHeader { path, .. } => Some(path),
            _ => None,
        })
        .map(|path| {
            let kind = if !at_head.contains(path.as_str()) {
                GitStatusDotKind::Mixed
            } else if added.contains(path) {
                GitStatusDotKind::New
            } else {
                GitStatusDotKind::Modified
            };
            (path.clone(), kind)
        })
        .collect()
}

/// Two different kinds under one folder: that is what `Mixed` means.
fn merge(a: GitStatusDotKind, b: GitStatusDotKind) -> GitStatusDotKind {
    if a == b {
        a
    } else {
        GitStatusDotKind::Mixed
    }
}

/// Materialize `path` and every folder above it, folding `status` into each
/// ancestor on the way down.
fn insert(
    nodes: &mut HashMap<LiveId, Node>,
    roots: &mut Vec<LiveId>,
    path: &str,
    status: GitStatusDotKind,
) {
    let mut parent: Option<LiveId> = None;
    let mut at = 0;
    while at < path.len() {
        let end = path[at..]
            .find('/')
            .map(|i| at + i)
            .unwrap_or_else(|| path.len());
        let prefix = &path[..end];
        let id = LiveId::from_str(prefix);
        let is_folder = end < path.len();

        let fresh = !nodes.contains_key(&id);
        if fresh {
            nodes.insert(
                id,
                Node {
                    path: prefix.to_string(),
                    name: path[at..end].to_string(),
                    children: is_folder.then(Vec::new),
                    status,
                },
            );
            match parent
                .and_then(|p| nodes.get_mut(&p))
                .and_then(|n| n.children.as_mut())
            {
                Some(children) => children.push(id),
                None => roots.push(id),
            }
        } else if let Some(node) = nodes.get_mut(&id) {
            node.status = merge(node.status, status);
        }

        parent = Some(id);
        at = end + 1;
    }
}

/// Fold `status` into a path's surviving ancestors without listing the path
/// itself — what a deletion leaves behind at the head.
fn tint_ancestors(nodes: &mut HashMap<LiveId, Node>, path: &str, status: GitStatusDotKind) {
    for (at, _) in path.match_indices('/') {
        if let Some(node) = nodes.get_mut(&LiveId::from_str(&path[..at])) {
            node.status = merge(node.status, status);
        }
    }
}

/// The head listing as drawable nodes, folders first at every level, all of it
/// under one row named for the repo, which is where the tree is drawn from.
///
/// NOTE: the repo row is not decoration. `FileTree::draw_folder` skips the
/// status dot at depth 0, so without it every top-level folder would draw
/// undotted however much changed inside it.
///
/// `status` covers every path the range touched, including ones absent from
/// `paths`: a file deleted at the head has no blob to list, but a folder whose
/// only change is that deletion should still show a dot.
fn build(
    paths: &[String],
    status: &HashMap<String, GitStatusDotKind>,
    repo: &str,
) -> (HashMap<LiveId, Node>, LiveId) {
    let mut nodes: HashMap<LiveId, Node> = HashMap::new();
    let mut roots: Vec<LiveId> = Vec::new();

    for path in paths {
        let kind = status.get(path).copied().unwrap_or(GitStatusDotKind::None);
        insert(&mut nodes, &mut roots, path, kind);
    }
    for (path, kind) in status {
        if !nodes.contains_key(&LiveId::from_str(path)) {
            tint_ancestors(&mut nodes, path, *kind);
        }
    }

    let order = |nodes: &HashMap<LiveId, Node>, id: &LiveId| {
        let n = &nodes[id];
        (n.children.is_none(), n.name.to_lowercase())
    };
    let sort = |ids: &mut Vec<LiveId>, nodes: &HashMap<LiveId, Node>| {
        ids.sort_by_key(|id| order(nodes, id));
    };
    sort(&mut roots, &nodes);
    let folders: Vec<LiveId> = nodes
        .iter()
        .filter(|(_, n)| n.children.is_some())
        .map(|(id, _)| *id)
        .collect();
    for id in folders {
        let Some(mut children) = nodes.get_mut(&id).and_then(|n| n.children.take()) else {
            continue;
        };
        sort(&mut children, &nodes);
        if let Some(node) = nodes.get_mut(&id) {
            node.children = Some(children);
        }
    }

    // The empty path: the prefix every listed path shares, and one no blob can
    // occupy — so it cannot collide with a node `insert` made.
    let root = LiveId::from_str("");
    let status = roots
        .iter()
        .map(|id| nodes[id].status)
        .reduce(merge)
        .unwrap_or(GitStatusDotKind::None);
    nodes.insert(
        root,
        Node {
            path: String::new(),
            name: repo.to_string(),
            children: Some(roots),
            status,
        },
    );
    (nodes, root)
}

fn draw_node(cx: &mut Cx2d, tree: &mut FileTree, nodes: &HashMap<LiveId, Node>, id: LiveId) {
    let Some(node) = nodes.get(&id) else {
        return;
    };
    match &node.children {
        // Err means collapsed or culled: the widget already unwound its own
        // stack, so the children are skipped without an end_folder.
        Some(children) => {
            if tree
                .begin_folder_with_status(cx, id, &node.name, node.status)
                .is_ok()
            {
                for child in children {
                    draw_node(cx, tree, nodes, *child);
                }
                tree.end_folder();
            }
        }
        None => tree.file_with_status(cx, id, &node.name, node.status),
    }
}

impl Widget for FileBrowser {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(frame) = scope.data.get::<FrameData>() else {
            return DrawStep::done();
        };
        let d = frame.document.clone();
        let rebuilt = self.built != d.generation;
        if rebuilt {
            self.built = d.generation;
            let at_head: HashSet<&str> = d.tree.iter().map(String::as_str).collect();
            let status = status_dots(&d.files_rows, &d.added, &at_head);
            let repo = std::path::Path::new(&d.repo)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| d.repo.clone());
            (self.nodes, self.root) = build(&d.tree, &status, &repo);
        }
        // The head as a rev and as an oid: a branch name moves, and a review
        // that outlives a push should still say which commit it read.
        let rev = match d.head_oid {
            Some(oid) => format!("{} · {}", d.head, &oid.to_string()[..7]),
            None => d.head.clone(),
        };
        self.view.label(cx, ids!(rev_chip)).set_text(cx, &rev);

        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut tree) = step.as_file_tree().borrow_mut() {
                // A fresh tree starts every folder shut, which for the repo row
                // means one collapsed line and no listing at all.
                if rebuilt {
                    tree.set_folder_is_open(cx, self.root, true, Animate::No);
                }
                draw_node(cx, &mut tree, &self.nodes, self.root);
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        self.widget_match_event(cx, event, scope);
    }
}

impl WidgetMatchEvent for FileBrowser {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, _scope: &mut Scope) {
        // A folder needs no handling — the tree owns its own open/closed state.
        let Some(id) = self.view.file_tree(cx, ids!(tree)).file_clicked(actions) else {
            return;
        };
        if let Some(node) = self.nodes.get(&id) {
            let path = node.path.clone();
            cx.widget_action(self.widget_uid(), FileBrowserAction::OpenFile(path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn name_of(nodes: &HashMap<LiveId, Node>, path: &str) -> String {
        nodes[&LiveId::from_str(path)].name.clone()
    }

    /// The names of one node's children, in draw order.
    fn children_of(nodes: &HashMap<LiveId, Node>, id: LiveId) -> Vec<&str> {
        nodes[&id]
            .children
            .as_ref()
            .unwrap()
            .iter()
            .map(|id| nodes[id].name.as_str())
            .collect()
    }

    #[test]
    fn folders_are_materialized_from_the_path_separators() {
        let (nodes, root) = build(
            &paths(&["a/b/c.rs", "a/d.rs", "top.md"]),
            &HashMap::new(),
            "repo",
        );

        // `a/b` is a node even though the listing only ever named blobs.
        assert_eq!(name_of(&nodes, "a"), "a");
        assert_eq!(name_of(&nodes, "a/b"), "b");
        assert_eq!(name_of(&nodes, "a/b/c.rs"), "c.rs");
        // Everything hangs off the repo row, so no real folder sits at the
        // depth where the tree widget skips the status dot.
        assert_eq!(nodes[&root].name, "repo");
        // Folders sort before files at every level.
        assert_eq!(children_of(&nodes, root), ["a", "top.md"]);
        assert_eq!(children_of(&nodes, LiveId::from_str("a")), ["b", "d.rs"]);
    }

    #[test]
    fn two_kinds_under_one_folder_read_as_mixed() {
        let status = HashMap::from([
            ("a/new.rs".to_string(), GitStatusDotKind::New),
            ("a/edit.rs".to_string(), GitStatusDotKind::Modified),
            ("b/one.rs".to_string(), GitStatusDotKind::New),
            ("b/two.rs".to_string(), GitStatusDotKind::New),
        ]);
        let (nodes, _) = build(
            &paths(&["a/new.rs", "a/edit.rs", "b/one.rs", "b/two.rs"]),
            &status,
            "repo",
        );

        assert_eq!(
            nodes[&LiveId::from_str("a")].status,
            GitStatusDotKind::Mixed
        );
        // One kind throughout cascades unchanged.
        assert_eq!(nodes[&LiveId::from_str("b")].status, GitStatusDotKind::New);
    }

    #[test]
    fn a_deleted_path_tints_its_folders_without_being_listed() {
        let status = HashMap::from([("a/gone.rs".to_string(), GitStatusDotKind::Mixed)]);
        let (nodes, _) = build(&paths(&["a/kept.rs"]), &status, "repo");

        assert!(!nodes.contains_key(&LiveId::from_str("a/gone.rs")));
        assert_eq!(
            nodes[&LiveId::from_str("a")].status,
            GitStatusDotKind::Mixed
        );
    }

    #[test]
    fn a_path_the_range_never_touched_gets_no_dot() {
        let (nodes, _) = build(&paths(&["a/quiet.rs"]), &HashMap::new(), "repo");

        assert_eq!(
            nodes[&LiveId::from_str("a/quiet.rs")].status,
            GitStatusDotKind::None
        );
    }

    #[test]
    fn status_dots_reads_new_edited_and_deleted_off_the_range() {
        let files_rows = ["new.rs", "edit.rs", "gone.rs"]
            .map(|path| Row::FileHeader {
                path: path.into(),
                lang: "rust",
                adds: 1,
                dels: 0,
                from: None,
                similarity: None,
            })
            .to_vec();
        let added = HashSet::from(["new.rs".to_string()]);
        let at_head = HashSet::from(["new.rs", "edit.rs", "quiet.rs"]);

        let dots = status_dots(&files_rows, &added, &at_head);

        assert_eq!(dots["new.rs"], GitStatusDotKind::New);
        assert_eq!(dots["edit.rs"], GitStatusDotKind::Modified);
        // Changed but absent from the head tree: deleted.
        assert_eq!(dots["gone.rs"], GitStatusDotKind::Mixed);
        // Untouched by the range, so no entry at all.
        assert!(!dots.contains_key("quiet.rs"));
    }
}
