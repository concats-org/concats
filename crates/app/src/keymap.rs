//! Commands, and the keys bound to them.
//!
//! Every action a key can run has a name here, in one table: config.toml binds
//! keys to these names, the menu shows the key each one has, and the palette
//! lists them. A binding is one stroke (`cmd-w`) or two (`cmd-k cmd-w`); the
//! defaults below apply first, and `[app.keys]` overrides them key by key, an
//! empty name unbinding the key.

use std::collections::BTreeMap;

use concats_config::Area;

use crate::{
    makepad_widgets::{KeyCode, KeyModifiers},
    review_doc::Stream,
};

/// An action a key, the menu or the palette can run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Command {
    NewFile,
    NewWindow,
    CloseWindow,
    CloseTab,
    CloseOtherTabs,
    CloseSavedTabs,
    CloseAllTabs,
    NextTab,
    PreviousTab,
    /// The tab at this 1-based position in the current tab bar.
    Tab(usize),
    Settings,
    /// Settings, where the key bindings are.
    Keymap,
    /// Open or shut the panel on this side, whatever it holds.
    TogglePanel(Area),
    /// The terminal: reveal it and type into it.
    OpenTerminal,
    /// Another terminal, beside the others.
    NewTerminal,
    /// The file browser: reveal it, keyboard in its tree.
    OpenBrowser,
    /// A stream's tab: reveal it, keyboard in its list.
    OpenStream(Stream),
    /// The repo picker, under the repo name.
    OpenRepo,
    /// The range picker for the base.
    PickBase,
    /// The range picker for the head.
    PickHead,
    /// Load the range again.
    Reload,
}

/// Every command: its name in config.toml, and its title in the menu and the
/// palette.
pub(crate) const COMMANDS: &[(Command, &str, &str)] = &[
    (Command::NewFile, "new_file", "New File"),
    (Command::NewWindow, "new_window", "New Window"),
    (Command::CloseWindow, "close_window", "Close Window"),
    (Command::CloseTab, "close_tab", "Close Tab"),
    (
        Command::CloseOtherTabs,
        "close_other_tabs",
        "Close Other Tabs",
    ),
    (
        Command::CloseSavedTabs,
        "close_saved_tabs",
        "Close Saved Tabs",
    ),
    (Command::CloseAllTabs, "close_all_tabs", "Close All Tabs"),
    (Command::NextTab, "next_tab", "Next Tab"),
    (Command::PreviousTab, "previous_tab", "Previous Tab"),
    (Command::Tab(1), "tab_1", "Tab 1"),
    (Command::Tab(2), "tab_2", "Tab 2"),
    (Command::Tab(3), "tab_3", "Tab 3"),
    (Command::Tab(4), "tab_4", "Tab 4"),
    (Command::Tab(5), "tab_5", "Tab 5"),
    (Command::Tab(6), "tab_6", "Tab 6"),
    (Command::Tab(7), "tab_7", "Tab 7"),
    (Command::Tab(8), "tab_8", "Tab 8"),
    (Command::Tab(9), "tab_9", "Tab 9"),
    (Command::Settings, "settings", "Settings"),
    (Command::Keymap, "keymap", "Keymap"),
    (
        Command::TogglePanel(Area::Left),
        "toggle_left_panel",
        "Toggle Left Panel",
    ),
    (
        Command::TogglePanel(Area::Right),
        "toggle_right_panel",
        "Toggle Right Panel",
    ),
    (
        Command::TogglePanel(Area::Bottom),
        "toggle_bottom_panel",
        "Toggle Bottom Panel",
    ),
    (Command::OpenTerminal, "open_terminal", "Open Terminal"),
    (Command::NewTerminal, "new_terminal", "New Terminal"),
    (Command::OpenBrowser, "open_browser", "Open File Browser"),
    (
        Command::OpenStream(Stream::Guide),
        "open_guide",
        "Open Guide",
    ),
    (
        Command::OpenStream(Stream::Sessions),
        "open_sessions",
        "Open Sessions",
    ),
    (
        Command::OpenStream(Stream::Commits),
        "open_commits",
        "Open Commits",
    ),
    (
        Command::OpenStream(Stream::Files),
        "open_diff",
        "Open File Diff",
    ),
    (
        Command::OpenStream(Stream::Comments),
        "open_comments",
        "Open Comments",
    ),
    (Command::OpenRepo, "open_repo", "Open Repository"),
    (Command::PickBase, "pick_base", "Pick Base"),
    (Command::PickHead, "pick_head", "Pick Head"),
    (Command::Reload, "reload", "Reload"),
];

/// The keys every command has until config.toml says otherwise.
const DEFAULTS: &[(&str, &str)] = &[
    ("cmd-n", "new_file"),
    ("cmd-shift-n", "new_window"),
    ("cmd-shift-w", "close_window"),
    ("cmd-w", "close_tab"),
    ("alt-cmd-t", "close_other_tabs"),
    ("cmd-k u", "close_saved_tabs"),
    ("cmd-k cmd-w", "close_all_tabs"),
    ("alt-cmd-right", "next_tab"),
    ("alt-cmd-left", "previous_tab"),
    ("ctrl-1", "tab_1"),
    ("ctrl-2", "tab_2"),
    ("ctrl-3", "tab_3"),
    ("ctrl-4", "tab_4"),
    ("ctrl-5", "tab_5"),
    ("ctrl-6", "tab_6"),
    ("ctrl-7", "tab_7"),
    ("ctrl-8", "tab_8"),
    ("ctrl-9", "tab_9"),
    ("cmd-,", "settings"),
    ("cmd-k cmd-s", "keymap"),
    ("cmd-b", "toggle_left_panel"),
    ("alt-cmd-b", "toggle_right_panel"),
    ("cmd-j", "toggle_bottom_panel"),
    ("ctrl-`", "open_terminal"),
    ("ctrl-shift-`", "new_terminal"),
    ("cmd-shift-e", "open_browser"),
    // The views, in the status bar's order left to right: starting points,
    // to change in config.toml.
    ("alt-cmd-1", "open_comments"),
    ("alt-cmd-2", "open_diff"),
    ("alt-cmd-3", "open_commits"),
    ("alt-cmd-4", "open_sessions"),
    ("alt-cmd-5", "open_guide"),
    ("cmd-o", "open_repo"),
    ("cmd-r", "reload"),
];

impl Command {
    /// The command config.toml calls `name`.
    pub(crate) fn named(name: &str) -> Option<Command> {
        COMMANDS
            .iter()
            .find(|(_, n, _)| *n == name)
            .map(|(command, ..)| *command)
    }

    pub(crate) fn name(self) -> &'static str {
        COMMANDS
            .iter()
            .find(|(command, ..)| *command == self)
            .map_or("", |(_, name, _)| name)
    }

    pub(crate) fn title(self) -> &'static str {
        COMMANDS
            .iter()
            .find(|(command, ..)| *command == self)
            .map_or("", |(.., title)| title)
    }
}

/// One key press: a key and the modifiers held with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Stroke {
    pub key: KeyCode,
    pub modifiers: KeyModifiers,
}

/// The keys `text` names, the way config.toml writes them: modifiers and a
/// key joined by `-` (`alt-cmd-down`), two strokes apart by a space for a
/// chord (`cmd-k cmd-w`).
pub(crate) fn parse(text: &str) -> Result<Vec<Stroke>, String> {
    let strokes: Vec<Stroke> = text
        .split_whitespace()
        .map(parse_stroke)
        .collect::<Result<_, _>>()?;
    match strokes.len() {
        1 | 2 => Ok(strokes),
        _ => Err(format!("{text:?} is not one key or a two-key chord")),
    }
}

fn parse_stroke(text: &str) -> Result<Stroke, String> {
    // The key is the last part, so `cmd--` is Cmd and minus.
    let (mods, key) = match text.rsplit_once('-') {
        Some((mods, "")) => match mods.strip_suffix('-') {
            Some(mods) => (mods, "-"),
            None => return Err(format!("no key in {text:?}")),
        },
        Some((mods, key)) => (mods, key),
        None => ("", text),
    };
    let mut modifiers = KeyModifiers::default();
    for name in mods.split('-').filter(|name| !name.is_empty()) {
        match name {
            "cmd" => modifiers.logo = true,
            "shift" => modifiers.shift = true,
            "alt" => modifiers.alt = true,
            "ctrl" => modifiers.control = true,
            _ => return Err(format!("unknown modifier {name:?} in {text:?}")),
        }
    }
    let key = key_code(key).ok_or_else(|| format!("unknown key {key:?} in {text:?}"))?;
    Ok(Stroke { key, modifiers })
}

/// The key config.toml calls `name`: a letter, a digit, punctuation, or a
/// named key.
fn key_code(name: &str) -> Option<KeyCode> {
    const LETTERS: [KeyCode; 26] = [
        KeyCode::KeyA,
        KeyCode::KeyB,
        KeyCode::KeyC,
        KeyCode::KeyD,
        KeyCode::KeyE,
        KeyCode::KeyF,
        KeyCode::KeyG,
        KeyCode::KeyH,
        KeyCode::KeyI,
        KeyCode::KeyJ,
        KeyCode::KeyK,
        KeyCode::KeyL,
        KeyCode::KeyM,
        KeyCode::KeyN,
        KeyCode::KeyO,
        KeyCode::KeyP,
        KeyCode::KeyQ,
        KeyCode::KeyR,
        KeyCode::KeyS,
        KeyCode::KeyT,
        KeyCode::KeyU,
        KeyCode::KeyV,
        KeyCode::KeyW,
        KeyCode::KeyX,
        KeyCode::KeyY,
        KeyCode::KeyZ,
    ];
    const DIGITS: [KeyCode; 10] = [
        KeyCode::Key0,
        KeyCode::Key1,
        KeyCode::Key2,
        KeyCode::Key3,
        KeyCode::Key4,
        KeyCode::Key5,
        KeyCode::Key6,
        KeyCode::Key7,
        KeyCode::Key8,
        KeyCode::Key9,
    ];
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_lowercase() {
            return Some(LETTERS[c as usize - 'a' as usize]);
        }
        if c.is_ascii_digit() {
            return Some(DIGITS[c as usize - '0' as usize]);
        }
    }
    Some(match name {
        "`" => KeyCode::Backtick,
        "-" => KeyCode::Minus,
        "=" => KeyCode::Equals,
        "[" => KeyCode::LBracket,
        "]" => KeyCode::RBracket,
        "\\" => KeyCode::Backslash,
        ";" => KeyCode::Semicolon,
        "'" => KeyCode::Quote,
        "," => KeyCode::Comma,
        "." => KeyCode::Period,
        "/" => KeyCode::Slash,
        "left" => KeyCode::ArrowLeft,
        "right" => KeyCode::ArrowRight,
        "up" => KeyCode::ArrowUp,
        "down" => KeyCode::ArrowDown,
        "escape" => KeyCode::Escape,
        "enter" => KeyCode::ReturnKey,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Space,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        _ => return None,
    })
}

/// Keys the way a Mac menu writes them: `⌘K ⌘W`, `⌥⌘→`, `⌃1`.
pub(crate) fn label(strokes: &[Stroke]) -> String {
    strokes
        .iter()
        .map(|Stroke { key, modifiers }| {
            let mods = [
                (modifiers.control, "⌃"),
                (modifiers.alt, "⌥"),
                (modifiers.shift, "⇧"),
                (modifiers.logo, "⌘"),
            ]
            .into_iter()
            .filter_map(|(held, symbol)| held.then_some(symbol))
            .collect::<String>();
            format!("{mods}{}", key_label(*key))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// How a key reads on a keycap.
fn key_label(key: KeyCode) -> String {
    let named = match key {
        KeyCode::ArrowLeft => "←",
        KeyCode::ArrowRight => "→",
        KeyCode::ArrowUp => "↑",
        KeyCode::ArrowDown => "↓",
        KeyCode::Escape => "⎋",
        KeyCode::ReturnKey => "↩",
        KeyCode::Tab => "⇥",
        KeyCode::Space => "Space",
        KeyCode::Backspace => "⌫",
        KeyCode::Delete => "⌦",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_string();
    }
    // Letters, digits and punctuation: the name config.toml uses.
    ('a'..='z')
        .chain('0'..='9')
        .chain("`-=[]\\;',./".chars())
        .find(|c| key_code(&c.to_string()) == Some(key))
        .map_or_else(
            || format!("{key:?}"),
            |c| c.to_ascii_uppercase().to_string(),
        )
}

/// What a key press does, given the chord it may complete.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Press {
    Run(Command),
    /// The first stroke of a chord: wait for the second.
    Pending,
    /// A chord's second stroke that completes nothing: the chord is dropped,
    /// and so is the key.
    Cancelled,
    /// Not a key of the keymap: it goes on to the widgets.
    Unbound,
}

/// Which keys run which commands.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Keymap {
    bindings: Vec<(Vec<Stroke>, Command)>,
}

impl Keymap {
    /// The defaults with `overrides` applied: each key there is bound to the
    /// command it names, or unbound by an empty name.
    pub(crate) fn new(overrides: &BTreeMap<String, String>) -> Result<Keymap, String> {
        let defaults = DEFAULTS
            .iter()
            .map(|(keys, name)| ((*keys).to_string(), (*name).to_string()));
        let entries = defaults.chain(overrides.iter().map(|(k, n)| (k.clone(), n.clone())));
        let mut bindings: Vec<(Vec<Stroke>, Command)> = Vec::new();
        for (keys, name) in entries {
            let strokes = parse(&keys)?;
            bindings.retain(|(bound, _)| *bound != strokes);
            if name.is_empty() {
                continue;
            }
            let command =
                Command::named(&name).ok_or_else(|| format!("unknown command {name:?}"))?;
            bindings.push((strokes, command));
        }
        Ok(Keymap { bindings })
    }

    /// What `stroke` does, after `pending`, the first stroke of a chord if
    /// one is waiting.
    pub(crate) fn press(&self, pending: Option<Stroke>, stroke: Stroke) -> Press {
        let keys: Vec<Stroke> = pending.into_iter().chain([stroke]).collect();
        if let Some((_, command)) = self.bindings.iter().find(|(bound, _)| *bound == keys) {
            return Press::Run(*command);
        }
        if pending.is_some() {
            return Press::Cancelled;
        }
        if self
            .bindings
            .iter()
            .any(|(bound, _)| bound.len() == 2 && bound[0] == stroke)
        {
            return Press::Pending;
        }
        Press::Unbound
    }

    /// The key a macOS menu item can show for `command`: one stroke of Cmd,
    /// or Shift-Cmd, and a key; a menu can't carry Option, Control or a chord.
    pub(crate) fn menu_key(&self, command: Command) -> Option<(KeyCode, bool)> {
        self.bindings
            .iter()
            .find_map(|(keys, bound)| match keys.as_slice() {
                [Stroke { key, modifiers }]
                    if *bound == command
                        && modifiers.logo
                        && !modifiers.alt
                        && !modifiers.control =>
                {
                    Some((*key, modifiers.shift))
                }
                _ => None,
            })
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::new(&BTreeMap::new()).expect("the default keys parse")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(text: &str) -> Stroke {
        parse(text).expect("parses")[0]
    }

    #[test]
    fn keys_parse_the_way_config_writes_them() {
        for text in [
            "cmd-w",
            "alt-cmd-right",
            "ctrl-1",
            "ctrl-`",
            "ctrl-shift-`",
            "alt-cmd-down",
            "cmd-k cmd-w",
            "cmd-k p",
            "cmd--",
            "cmd-,",
        ] {
            assert!(parse(text).is_ok(), "{text}");
        }
        let w = stroke("cmd-shift-w");
        assert_eq!(w.key, KeyCode::KeyW);
        assert!(w.modifiers.logo && w.modifiers.shift && !w.modifiers.alt);
        assert_eq!(stroke("cmd--").key, KeyCode::Minus);
        for text in ["", "hyper-w", "cmd-wat", "cmd-k cmd-k cmd-k", "cmd-"] {
            assert!(parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn overrides_apply_over_the_defaults() {
        let keys = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<_, _>>()
        };
        let map = Keymap::new(&keys(&[("cmd-w", ""), ("cmd-e", "close_tab")])).expect("valid");
        assert_eq!(map.press(None, stroke("cmd-w")), Press::Unbound);
        assert_eq!(
            map.press(None, stroke("cmd-e")),
            Press::Run(Command::CloseTab)
        );
        assert_eq!(
            map.menu_key(Command::CloseTab),
            Some((KeyCode::KeyE, false))
        );
        assert!(Keymap::new(&keys(&[("cmd-e", "no_such_thing")])).is_err());
        assert!(Keymap::new(&keys(&[("cmd-wat", "close_tab")])).is_err());
    }

    #[test]
    fn a_chord_waits_for_its_second_stroke() {
        let map = Keymap::default();
        let k = stroke("cmd-k");
        assert_eq!(map.press(None, k), Press::Pending);
        assert_eq!(
            map.press(Some(k), stroke("cmd-w")),
            Press::Run(Command::CloseAllTabs)
        );
        assert_eq!(
            map.press(Some(k), stroke("u")),
            Press::Run(Command::CloseSavedTabs)
        );
        assert_eq!(map.press(Some(k), stroke("escape")), Press::Cancelled);
        assert_eq!(map.press(Some(k), stroke("x")), Press::Cancelled);
        assert_eq!(map.press(None, stroke("x")), Press::Unbound);
        // A menu shows only single Cmd keys.
        assert_eq!(
            map.menu_key(Command::NewWindow),
            Some((KeyCode::KeyN, true))
        );
        assert_eq!(map.menu_key(Command::CloseAllTabs), None);
        assert_eq!(map.menu_key(Command::NextTab), None);
    }

    #[test]
    fn keys_read_the_way_a_menu_writes_them() {
        for text in [
            "cmd-k cmd-w",
            "alt-cmd-right",
            "ctrl-1",
            "ctrl-shift-`",
            "cmd-,",
        ] {
            assert!(
                !label(&parse(text).expect("parses")).contains("Key"),
                "{text}"
            );
        }
        assert_eq!(label(&parse("cmd-k cmd-w").expect("parses")), "⌘K ⌘W");
        assert_eq!(label(&parse("alt-cmd-right").expect("parses")), "⌥⌘→");
        assert_eq!(label(&parse("ctrl-shift-`").expect("parses")), "⌃⇧`");
    }

    #[test]
    fn every_command_has_one_name() {
        for (command, name, title) in COMMANDS {
            assert_eq!(Command::named(name), Some(*command), "{name}");
            assert_eq!(command.name(), *name);
            assert_eq!(command.title(), *title);
        }
    }
}
