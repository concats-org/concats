use std::path::PathBuf;

use concats_core::session_history::{SessionInfo, TurnInfo};

/// Available tabs in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Agent,
    Sessions,
    Settings,
    Help,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[Tab::Agent, Tab::Sessions, Tab::Settings, Tab::Help]
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::Agent => "Agent",
            Tab::Sessions => "Sessions",
            Tab::Settings => "Settings",
            Tab::Help => "Help",
        }
    }

    /// Shortcut key shown in tab bar (1-indexed).
    pub fn index(self) -> usize {
        match self {
            Tab::Agent => 0,
            Tab::Sessions => 1,
            Tab::Settings => 2,
            Tab::Help => 3,
        }
    }
}

/// State for the Sessions tab.
pub struct SessionsTabState {
    pub sessions: Vec<SessionInfo>,
    /// Index of the selected session in the sessions list.
    pub selected_session: usize,
    /// When a session is expanded, holds its turns and selected turn index.
    pub expanded: Option<ExpandedSession>,
    /// Scroll offset for the sessions list (reserved for future use).
    #[allow(dead_code)]
    pub scroll_offset: usize,
    /// Path to the git repository.
    pub repo_path: PathBuf,
}

/// State for an expanded session showing its turns.
pub struct ExpandedSession {
    pub session_index: usize,
    pub turns: Vec<TurnInfo>,
    pub selected_turn: usize,
}

impl SessionsTabState {
    pub fn new(repo_path: PathBuf) -> Self {
        Self {
            sessions: Vec::new(),
            selected_session: 0,
            expanded: None,
            scroll_offset: 0,
            repo_path,
        }
    }

    /// Refresh the sessions list from git refs.
    pub fn refresh(&mut self) {
        match concats_core::session_history::list_sessions(&self.repo_path) {
            Ok(sessions) => {
                self.sessions = sessions;
                if self.selected_session >= self.sessions.len() {
                    self.selected_session = self.sessions.len().saturating_sub(1);
                }
                // Collapse any expansion since data changed.
                self.expanded = None;
            }
            Err(e) => {
                tracing::warn!("failed to list sessions: {e}");
            }
        }
    }

    pub fn select_next(&mut self) {
        if let Some(ref mut expanded) = self.expanded {
            if expanded.selected_turn + 1 < expanded.turns.len() {
                expanded.selected_turn += 1;
            }
        } else if !self.sessions.is_empty() {
            self.selected_session = (self.selected_session + 1).min(self.sessions.len() - 1);
        }
    }

    pub fn select_prev(&mut self) {
        if let Some(ref mut expanded) = self.expanded {
            expanded.selected_turn = expanded.selected_turn.saturating_sub(1);
        } else {
            self.selected_session = self.selected_session.saturating_sub(1);
        }
    }

    /// Toggle expansion of the selected session.
    pub fn toggle_expand(&mut self) {
        if self.expanded.is_some() {
            self.expanded = None;
            return;
        }

        if let Some(session) = self.sessions.get(self.selected_session) {
            match concats_core::session_history::load_session_turns(&self.repo_path, &session.id) {
                Ok(turns) => {
                    self.expanded = Some(ExpandedSession {
                        session_index: self.selected_session,
                        turns,
                        selected_turn: 0,
                    });
                }
                Err(e) => {
                    tracing::warn!("failed to load turns for session {}: {e}", session.id);
                }
            }
        }
    }

    /// Get the commit OID for the currently selected turn (when expanded).
    pub fn selected_turn_oid(&self) -> Option<git2::Oid> {
        let expanded = self.expanded.as_ref()?;
        let turn = expanded.turns.get(expanded.selected_turn)?;
        // Convert our Oid wrapper back to git2::Oid by parsing the string.
        let oid_str = turn.commit_oid.to_string();
        git2::Oid::from_str(&oid_str).ok()
    }

    /// Get info about the selected session and turn for fork display.
    pub fn selected_fork_info(&self) -> Option<(String, u32)> {
        let expanded = self.expanded.as_ref()?;
        let session = self.sessions.get(expanded.session_index)?;
        let turn = expanded.turns.get(expanded.selected_turn)?;
        Some((session.id.clone(), turn.turn_number))
    }

    /// Returns the total number of visible rows (sessions + expanded turns).
    #[allow(dead_code)]
    pub fn visible_row_count(&self) -> usize {
        let mut count = self.sessions.len();
        if let Some(ref expanded) = self.expanded {
            count += expanded.turns.len();
        }
        count
    }
}
