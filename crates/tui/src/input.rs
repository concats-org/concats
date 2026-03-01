use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, FocusedPanel};
use crate::tabs::ActiveTab;

/// Action returned by input handling that needs to be processed by the event loop.
pub enum InputAction {
    None,
    Fork,
    /// Open the agent picker (or create session immediately if single agent).
    NewSession,
    /// Close the active session tab.
    CloseActiveSession,
    /// Create a session with the agent at the given index in available_agents.
    CreateSession(usize),
}

/// Handle a key event, returning an action if one needs to be processed.
pub fn handle_key_event(app: &mut App, key: KeyEvent) -> InputAction {
    // If agent picker is open, intercept all keys for picker navigation.
    if let Some(ref mut picker) = app.agent_picker {
        match key.code {
            KeyCode::Up => {
                picker.selected = picker.selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if picker.selected + 1 < picker.agents.len() {
                    picker.selected += 1;
                }
            }
            KeyCode::Enter => {
                let selected = picker.selected;
                app.agent_picker = None;
                return InputAction::CreateSession(selected);
            }
            KeyCode::Esc => {
                app.agent_picker = None;
            }
            _ => {}
        }
        return InputAction::None;
    }

    // Global keybindings (always active).
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
            return InputAction::None;
        }
        // New session: Ctrl+N.
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return InputAction::NewSession;
        }
        // Close active session: Ctrl+W.
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return InputAction::CloseActiveSession;
        }
        // Tab switching: Ctrl+1..9.
        KeyCode::Char(c @ '1'..='9') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let idx = (c as usize) - ('1' as usize); // 0-based
            let entries = app.tab_bar_entries();
            // Map position to the correct tab (skip NewButton entries).
            let mut tab_idx = 0;
            for entry in &entries {
                match entry {
                    crate::tabs::TabBarEntry::Session { id, .. } => {
                        if tab_idx == idx {
                            app.switch_tab(ActiveTab::Session(*id));
                            return InputAction::None;
                        }
                        tab_idx += 1;
                    }
                    crate::tabs::TabBarEntry::NewButton => {
                        // Skip — not switchable via Ctrl+N number.
                    }
                    crate::tabs::TabBarEntry::Utility { tab, .. } => {
                        if tab_idx == idx {
                            app.switch_tab(*tab);
                            return InputAction::None;
                        }
                        tab_idx += 1;
                    }
                }
            }
            return InputAction::None;
        }
        _ => {}
    }

    // Tab-specific keybindings.
    match app.active_tab {
        ActiveTab::Session(_) => handle_session_keys(app, key),
        ActiveTab::Sessions => handle_sessions_keys(app, key),
        _ => InputAction::None,
    }
}

fn handle_session_keys(app: &mut App, key: KeyEvent) -> InputAction {
    let Some(tab) = app.active_session_mut() else {
        return InputAction::None;
    };

    match key.code {
        // Toggle stderr panel visibility.
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            tab.show_stderr = !tab.show_stderr;
            if !tab.show_stderr {
                tab.focused_panel = FocusedPanel::Conversation;
            }
        }
        // Switch focus between panels when stderr is visible.
        KeyCode::Tab if tab.show_stderr => {
            tab.focused_panel = match tab.focused_panel {
                FocusedPanel::Conversation => FocusedPanel::Stderr,
                FocusedPanel::Stderr => FocusedPanel::Conversation,
            };
        }
        // Scroll the focused panel.
        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
            scroll_focused_panel(tab, -1);
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
            scroll_focused_panel(tab, 1);
        }
        KeyCode::PageUp => scroll_focused_panel(tab, -8),
        KeyCode::PageDown => scroll_focused_panel(tab, 8),
        _ if !tab.waiting => {
            tab.textarea.input(key);
        }
        _ => {}
    }
    InputAction::None
}

fn scroll_focused_panel(tab: &mut crate::app::SessionTab, delta: i16) {
    match tab.focused_panel {
        FocusedPanel::Conversation => {
            tab.scroll_offset = apply_scroll_delta(tab.scroll_offset, delta);
        }
        FocusedPanel::Stderr => {
            tab.stderr_scroll = apply_scroll_delta(tab.stderr_scroll, delta);
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
            } else if let Some(tab) = app.session_tabs.first() {
                // Switch to the first session tab (if any).
                app.switch_tab(ActiveTab::Session(tab.id));
            }
        }
        _ => {}
    }
    InputAction::None
}
