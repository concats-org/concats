use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect, Size};

use crate::{
    app::{Action, App, FocusedPanel},
    tabs::{ActiveTab, ClickTarget},
    ui,
};

/// Handle key events and map them to actions or direct app changes.
pub async fn handle_key_events(key: KeyEvent, app: &mut App<'_>) -> miette::Result<()> {
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
                app.handle_action(Action::CreateSession(selected)).await?;
            }
            KeyCode::Esc => {
                app.agent_picker = None;
            }
            _ => {}
        }
        return Ok(());
    }

    // Global keybindings (always active).
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.handle_action(Action::Quit).await?;
            return Ok(());
        }
        // New session: Ctrl+N.
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.handle_action(Action::NewSession).await?;
            return Ok(());
        }
        // Close active session: Ctrl+W.
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.handle_action(Action::CloseActiveSession).await?;
            return Ok(());
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
                            return Ok(());
                        }
                        tab_idx += 1;
                    }
                    crate::tabs::TabBarEntry::NewButton => {
                        // Skip — not switchable via Ctrl+N number.
                    }
                    crate::tabs::TabBarEntry::Utility { tab, .. } => {
                        if tab_idx == idx {
                            app.switch_tab(*tab);
                            return Ok(());
                        }
                        tab_idx += 1;
                    }
                }
            }
            return Ok(());
        }
        _ => {}
    }

    // Tab-specific keybindings.
    match app.active_tab {
        ActiveTab::Session(_) => handle_session_keys(app, key).await?,
        ActiveTab::Sessions => handle_sessions_keys(app, key).await?,
        _ => {}
    }
    Ok(())
}

async fn handle_session_keys(app: &mut App<'_>, key: KeyEvent) -> miette::Result<()> {
    let is_session_tab = matches!(app.active_tab, ActiveTab::Session(_));
    let is_submit = key.code == KeyCode::Enter
        && !key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
        && app.active_session().is_some_and(|t| !t.waiting)
        && is_session_tab
        && app.agent_picker.is_none();

    if is_submit {
        if let Some(tab) = app.active_session_mut() {
            tab.send_prompt().await;
        }
        return Ok(());
    }

    if key.code == KeyCode::Enter
        && key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
        && app.active_session().is_some_and(|t| !t.waiting)
        && is_session_tab
        && app.agent_picker.is_none()
    {
        // Insert newline in textarea for Alt+Enter / Shift+Enter.
        if let Some(tab) = app.active_session_mut() {
            tab.textarea.insert_newline();
        }
        return Ok(());
    }

    let Some(tab) = app.active_session_mut() else {
        return Ok(());
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
    Ok(())
}

async fn handle_sessions_keys(app: &mut App<'_>, key: KeyEvent) -> miette::Result<()> {
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
                app.handle_action(Action::Fork).await?;
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
    Ok(())
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

/// Handle mouse events.
pub async fn handle_mouse_events(
    mouse: MouseEvent,
    app: &mut App<'_>,
    size: Size,
) -> miette::Result<()> {
    let terminal_area = Rect::new(0, 0, size.width, size.height);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Check if click is on the tab/menu bar (last row).
            if mouse.row == size.height.saturating_sub(1)
                && let Some(target) = target_from_click(mouse.column, app)
            {
                match target {
                    ClickTarget::SwitchTab(tab) => {
                        app.switch_tab(tab);
                    }
                    ClickTarget::CloseSession(id) => {
                        app.close_session(id);
                    }
                    ClickTarget::NewSession => {
                        app.handle_action(Action::NewSession).await?;
                    }
                }
            }
        }
        MouseEventKind::ScrollUp => {
            scroll_under_mouse(app, terminal_area, mouse.column, mouse.row, -3);
        }
        MouseEventKind::ScrollDown => {
            scroll_under_mouse(app, terminal_area, mouse.column, mouse.row, 3);
        }
        _ => {}
    }
    Ok(())
}

fn target_from_click(x: u16, app: &App<'_>) -> Option<ClickTarget> {
    let x = x as usize;
    for (target, start, end) in ui::tab_click_hitboxes(app) {
        if x >= start && x < end {
            return Some(target);
        }
    }
    None
}

fn scroll_under_mouse(
    app: &mut App<'_>,
    terminal_area: Rect,
    column: u16,
    row: u16,
    delta: i16,
) -> bool {
    if !matches!(app.active_tab, ActiveTab::Session(_)) {
        return false;
    }

    let Some(tab) = app.active_session_mut() else {
        return false;
    };

    let root_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(ui::TAB_BAR_HEIGHT)])
        .split(terminal_area);
    let main_area = root_chunks[0];
    if !rect_contains(main_area, column, row) {
        return false;
    }

    let agent_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(ui::session_input_height(tab, main_area.width)),
        ])
        .split(main_area);
    let conversation_area = agent_chunks[0];
    if !rect_contains(conversation_area, column, row) {
        return false;
    }

    if tab.show_stderr {
        let panel_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(conversation_area);

        if rect_contains(panel_chunks[0], column, row) {
            tab.focused_panel = FocusedPanel::Conversation;
            tab.scroll_offset = apply_scroll_delta(tab.scroll_offset, delta);
            return true;
        }

        if rect_contains(panel_chunks[1], column, row) {
            tab.focused_panel = FocusedPanel::Stderr;
            tab.stderr_scroll = apply_scroll_delta(tab.stderr_scroll, delta);
            return true;
        }
    } else {
        tab.focused_panel = FocusedPanel::Conversation;
        tab.scroll_offset = apply_scroll_delta(tab.scroll_offset, delta);
        return true;
    }

    false
}

fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    row >= rect.y
        && row < rect.y.saturating_add(rect.height)
        && column >= rect.x
        && column < rect.x.saturating_add(rect.width)
}
