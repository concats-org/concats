use std::path::PathBuf;

use concats_core::session_history::{SessionInfo, TurnInfo};

/// Which tab is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    /// An active agent session tab, identified by session ID.
    Session(u32),
    /// The session history browser.
    Sessions,
    Settings,
    Help,
}

/// An entry in the tab bar for rendering and click handling.
#[derive(Debug, Clone)]
pub enum TabBarEntry {
    /// An active session tab.
    Session { id: u32, label: String },
    /// The [+] button for creating new sessions.
    NewButton,
    /// A utility tab (Sessions, Settings, Help).
    Utility { tab: ActiveTab, label: &'static str },
}

/// Click target returned from tab bar hit-testing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickTarget {
    /// Switch to this tab.
    SwitchTab(ActiveTab),
    /// Close a session tab.
    CloseSession(u32),
    /// Open agent picker / create new session.
    NewSession,
}

/// Which panel of the sessions browser has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionsPanelFocus {
    List,
    Detail,
}

/// State for the detail (right) panel showing checkpoints.
pub struct DetailPanel {
    pub session_index: usize,
    pub turns: Vec<TurnInfo>,
    pub list_state: tui_widget_list::ListState,
}

/// State for the Sessions tab.
pub struct SessionsTabState {
    pub sessions: Vec<SessionInfo>,
    /// Detail panel state (right side), shown when a session is opened.
    pub detail: Option<DetailPanel>,
    /// ListView state for the session list (left panel).
    pub list_state: tui_widget_list::ListState,
    /// Which panel currently has focus.
    pub focus: SessionsPanelFocus,
    /// Path to the git repository.
    pub repo_path: PathBuf,
}

impl SessionsTabState {
    pub fn new(repo_path: PathBuf) -> Self {
        Self {
            sessions: Vec::new(),
            detail: None,
            list_state: tui_widget_list::ListState::default(),
            focus: SessionsPanelFocus::List,
            repo_path,
        }
    }

    /// The currently selected session index (driven by list_state).
    pub fn selected_session(&self) -> usize {
        self.list_state.selected.unwrap_or(0)
    }

    /// Refresh the sessions list from git refs.
    pub fn refresh(&mut self) {
        match concats_core::session_history::list_sessions(&self.repo_path) {
            Ok(sessions) => {
                self.sessions = sessions;
                // Clamp selection.
                let sel = self.selected_session().min(self.sessions.len().saturating_sub(1));
                self.list_state.select(Some(sel));
                // Close detail panel since data changed.
                self.detail = None;
                self.focus = SessionsPanelFocus::List;
            }
            Err(e) => {
                tracing::warn!("failed to list sessions: {e}");
            }
        }
    }

    pub fn select_next(&mut self) {
        match self.focus {
            SessionsPanelFocus::List => {
                if !self.sessions.is_empty() {
                    let cur = self.selected_session();
                    let next = (cur + 1).min(self.sessions.len() - 1);
                    self.list_state.select(Some(next));
                }
            }
            SessionsPanelFocus::Detail => {
                self.scroll_detail(1);
            }
        }
    }

    pub fn select_prev(&mut self) {
        match self.focus {
            SessionsPanelFocus::List => {
                let cur = self.selected_session();
                self.list_state.select(Some(cur.saturating_sub(1)));
            }
            SessionsPanelFocus::Detail => {
                self.scroll_detail(-1);
            }
        }
    }

    /// Scroll the detail panel viewport by `delta` rows.
    pub fn scroll_detail(&mut self, delta: i16) {
        if let Some(ref mut detail) = self.detail {
            detail.list_state.scroll_by(delta);
        }
    }

    /// Open the detail panel for the currently selected session.
    pub fn open_detail(&mut self) {
        let idx = self.selected_session();
        if let Some(session) = self.sessions.get(idx) {
            match concats_core::session_history::load(&self.repo_path, &session.id) {
                Ok(turns) => {
                    self.detail = Some(DetailPanel {
                        session_index: idx,
                        turns,
                        list_state: tui_widget_list::ListState::default(),
                    });
                    self.focus = SessionsPanelFocus::Detail;
                }
                Err(e) => {
                    tracing::warn!("failed to load turns for session {}: {e}", session.id);
                }
            }
        }
    }

    /// Close the detail panel and return focus to the list.
    pub fn close_detail(&mut self) {
        self.detail = None;
        self.focus = SessionsPanelFocus::List;
    }

    /// Get the tip OID for the currently selected session in the list.
    pub fn selected_session_tip_oid(&self) -> Option<git2::Oid> {
        let session = self.sessions.get(self.selected_session())?;
        let oid_str = session.tip_oid.to_string();
        git2::Oid::from_str(&oid_str).ok()
    }

    /// Get info about the selected session for fork display.
    pub fn selected_fork_info(&self) -> Option<(String, u32)> {
        let idx = match self.focus {
            SessionsPanelFocus::Detail => {
                self.detail.as_ref().map(|d| d.session_index)?
            }
            SessionsPanelFocus::List => self.selected_session(),
        };
        let session = self.sessions.get(idx)?;
        Some((session.id.clone(), session.turn_count.saturating_sub(1)))
    }
}
