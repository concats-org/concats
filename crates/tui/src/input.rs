use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, FocusedPanel};
use crate::tabs::Tab;

/// Action returned by input handling that needs to be processed by the event loop.
pub enum InputAction {
    None,
    Fork,
}

/// Handle a key event, returning an action if one needs to be processed.
pub fn handle_key_event(app: &mut App, key: KeyEvent) -> InputAction {
    // Global keybindings (always active).
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
            return InputAction::None;
        }
        // Tab switching: Ctrl+1..4.
        KeyCode::Char('1') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.switch_tab(Tab::Agent);
            return InputAction::None;
        }
        KeyCode::Char('2') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.switch_tab(Tab::Sessions);
            return InputAction::None;
        }
        KeyCode::Char('3') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.switch_tab(Tab::Settings);
            return InputAction::None;
        }
        KeyCode::Char('4') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.switch_tab(Tab::Help);
            return InputAction::None;
        }
        _ => {}
    }

    // Tab-specific keybindings.
    match app.active_tab {
        Tab::Agent => handle_agent_keys(app, key),
        Tab::Sessions => handle_sessions_keys(app, key),
        _ => InputAction::None,
    }
}

fn handle_agent_keys(app: &mut App, key: KeyEvent) -> InputAction {
    match key.code {
        // Toggle stderr panel visibility.
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.show_stderr = !app.show_stderr;
            if !app.show_stderr {
                app.focused_panel = FocusedPanel::Conversation;
            }
        }
        // Switch focus between panels when stderr is visible.
        KeyCode::Tab if app.show_stderr => {
            app.focused_panel = match app.focused_panel {
                FocusedPanel::Conversation => FocusedPanel::Stderr,
                FocusedPanel::Stderr => FocusedPanel::Conversation,
            };
        }
        // Scroll the focused panel.
        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => scroll_focused_panel(app, -1),
        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
            scroll_focused_panel(app, 1)
        }
        KeyCode::PageUp => scroll_focused_panel(app, -8),
        KeyCode::PageDown => scroll_focused_panel(app, 8),
        _ if !app.waiting => {
            app.textarea.input(key);
        }
        _ => {}
    }
    InputAction::None
}

fn scroll_focused_panel(app: &mut App, delta: i16) {
    match app.focused_panel {
        FocusedPanel::Conversation => {
            app.scroll_offset = apply_scroll_delta(app.scroll_offset, delta);
        }
        FocusedPanel::Stderr => {
            app.stderr_scroll = apply_scroll_delta(app.stderr_scroll, delta);
        }
    }
}

fn apply_scroll_delta(current: u16, delta: i16) -> u16 {
    if delta >= 0 {
        current.saturating_add(delta as u16)
    } else {
        current.saturating_sub(delta.unsigned_abs())
    }
}

fn handle_sessions_keys(app: &mut App, key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.sessions_state.select_prev();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.sessions_state.select_next();
        }
        KeyCode::Enter => {
            app.sessions_state.toggle_expand();
        }
        KeyCode::Char('f') => {
            if app.sessions_state.expanded.is_some() {
                return InputAction::Fork;
            }
        }
        KeyCode::Char('r') => {
            app.sessions_state.refresh();
        }
        KeyCode::Esc => {
            if app.sessions_state.expanded.is_some() {
                app.sessions_state.expanded = None;
            } else {
                app.switch_tab(Tab::Agent);
            }
        }
        _ => {}
    }
    InputAction::None
}
