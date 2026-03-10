/// Which tab is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Session(u32),
    Sessions,
    Settings,
    Help,
}

/// An entry in the tab bar for rendering and click handling.
#[derive(Debug, Clone)]
pub enum TabBarEntry {
    Session { id: u32, label: String },
    NewButton,
    Utility { tab: ActiveTab, label: &'static str },
}

/// Click target returned from tab bar hit-testing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickTarget {
    SwitchTab(ActiveTab),
    CloseSession(u32),
    NewSession,
}
