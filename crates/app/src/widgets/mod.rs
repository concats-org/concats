//! The app's widget/component library. Each widget lives in its own module,
//! co-located with the `script_mod!` DSL that defines it; `styles.rs` holds the
//! shared style tokens they compose from. This mirrors makepad-studio and
//! Robrix's `shared/` layout — `mod.widgets.*` is the shared-component registry.

pub mod card_cap;
pub mod collapsed_run;
pub mod diff_line;
pub mod drop_shadow;
pub mod file_browser;
pub mod gutter;
pub mod review_list;
pub mod review_pane;
pub mod seen_bar;
pub mod styles;

pub use diff_line::DiffLine;
pub use file_browser::FileBrowserAction;
pub use gutter::{Gutter, GutterAction};
pub use review_list::ReviewList;
pub use review_pane::ReviewPane;
pub use seen_bar::SeenBar;

use crate::makepad_widgets::ScriptVm;

/// Register the shared styles and every widget into the script namespace, in
/// dependency order: styles first (the widgets read `mod.widgets.FONT` etc.),
/// then the leaf widgets before the ones that compose them — `ReviewList` embeds
/// `Gutter` + `DiffLine`, and `ReviewPane` embeds `ReviewList` and the terminal —
/// before the app window layout that embeds `ReviewPane`.
pub fn script_mod(vm: &mut ScriptVm) {
    styles::script_mod(vm);
    card_cap::script_mod(vm);
    drop_shadow::script_mod(vm);
    diff_line::script_mod(vm);
    gutter::script_mod(vm);
    collapsed_run::script_mod(vm);
    seen_bar::script_mod(vm);
    file_browser::script_mod(vm);
    review_list::script_mod(vm);
    review_pane::script_mod(vm);
}
