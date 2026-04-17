use ratatui::layout::Size;

pub enum Action {
    Tick,
    Render,
    Resize(Size),
    Quit,
    SessionsSelectNext,
    SessionsSelectPrev,
    SessionsOpenDetail,
    SessionsCloseDetail,
    SessionsRefresh,
    SessionsScrollDetail(i16),
}
