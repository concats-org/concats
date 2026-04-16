use ratatui::layout::Size;

use crate::tabs::ActiveTab;

pub enum Action {
    Tick,
    Render,
    Resize(Size),
    Quit,
    SwitchTab(ActiveTab),
    SessionsSelectNext,
    SessionsSelectPrev,
    SessionsOpenDetail,
    SessionsCloseDetail,
    SessionsRefresh,
    SessionsScrollDetail(i16),
    SessionsBack,
}
