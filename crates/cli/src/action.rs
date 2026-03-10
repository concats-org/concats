use crossterm::event::KeyEvent;
use ratatui::layout::Size;

use crate::tabs::ActiveTab;

pub enum Action {
    Tick,
    Render,
    Resize(Size),
    Quit,
    SwitchTab(ActiveTab),
    OpenAgentPicker,
    CloseAgentPicker,
    CreateSession(usize),
    CloseSession(u32),
    CloseActiveSession,
    SessionInput {
        tab_id: u32,
        key: KeyEvent,
    },
    SessionInsertNewline(u32),
    SessionSubmitPrompt(u32),
    SessionToggleStderr(u32),
    SessionCycleFocus(u32),
    SessionFocusConversation(u32),
    SessionFocusStderr(u32),
    SessionScrollConversation {
        tab_id: u32,
        delta: i16,
    },
    SessionScrollStderr {
        tab_id: u32,
        delta: i16,
    },
    SessionsSelectNext,
    SessionsSelectPrev,
    SessionsOpenDetail,
    SessionsCloseDetail,
    SessionsRefresh,
    SessionsScrollDetail(i16),
    SessionsBack,
    ForkSelected,
    AgentPickerSelectNext,
    AgentPickerSelectPrev,
    SessionEvent {
        tab_id: u32,
        event: concats_acp::SessionEvent,
    },
    SessionClosed(u32),
}
