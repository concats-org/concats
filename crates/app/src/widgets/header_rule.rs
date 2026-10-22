//! The line under a window's header, coloured by what the window shows: its
//! checkout and range. Two windows side by side can be told apart at a glance,
//! and the same review looks the same every time it is opened. Grey until a
//! load says what the window shows.

use std::{
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
};

use concats_theme::Rgba;

#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::{makepad_widgets::*, theme::paint};

script_mod! {
    use mod.prelude.widgets.*

    mod.widgets.HeaderRule = #(HeaderRule::register_widget(vm)) {
        width: Fill height: 1
        draw_line +: {}
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct HeaderRule {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_line: DrawColor,

    /// The checkout and the range as written.
    #[rust]
    shows: Option<(PathBuf, String)>,
}

impl HeaderRule {
    pub fn show(&mut self, cx: &mut Cx, root: PathBuf, range: String) {
        let shows = Some((root, range));
        if self.shows != shows {
            self.shows = shows;
            self.draw_line.redraw(cx);
        }
    }
}

impl Widget for HeaderRule {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let theme = crate::theme::active_theme();
        let color = match &self.shows {
            Some((root, range)) => window_color(&theme.window_palette(), root, range),
            None => theme.border,
        };
        self.draw_line.color = paint(color);
        self.draw_line.draw_walk(cx, walk);
        DrawStep::done()
    }
}

/// The palette entry for a checkout at a range. `DefaultHasher::new` has fixed
/// keys, so the same pair picks the same entry run after run.
fn window_color(palette: &[Rgba], root: &Path, range: &str) -> Rgba {
    let mut hasher = DefaultHasher::new();
    (root, range).hash(&mut hasher);
    let len = u64::try_from(palette.len()).expect("a palette fits in u64");
    palette[usize::try_from(hasher.finish() % len).expect("an index below the palette's length")]
}

#[cfg(test)]
mod tests {
    use concats_theme::Theme;

    use super::*;

    #[test]
    fn a_checkout_and_range_keep_their_colour_and_others_get_their_own() {
        let palette = Theme::concats().window_palette();
        let root = Path::new("/work/concats");
        let colour = window_color(&palette, root, "main...WORKTREE");
        assert_eq!(window_color(&palette, root, "main...WORKTREE"), colour);
        assert_ne!(window_color(&palette, root, "HEAD~1...HEAD"), colour);
        assert_ne!(
            window_color(&palette, Path::new("/work/concats-gix"), "main...WORKTREE"),
            colour
        );
    }
}
