use std::{io, path::PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
};
use tokio::sync::mpsc;

use crate::{
    action::Action,
    components::{
        Component,
        chrome::{ChromeComponent, ChromeModel, TAB_BAR_HEIGHT},
        sessions::SessionsBrowserComponent,
        static_page::StaticPageComponent,
    },
    tabs::{ActiveTab, TabBarEntry},
    tui::{Event, Tui},
};

pub struct App {
    active_tab: ActiveTab,
    should_quit: bool,
    action_tx: mpsc::UnboundedSender<Action>,
    action_rx: mpsc::UnboundedReceiver<Action>,
    chrome: ChromeComponent,
    sessions_browser: SessionsBrowserComponent,
    help_page: StaticPageComponent,
    settings_page: StaticPageComponent,
}

impl App {
    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        let (action_tx, action_rx) = mpsc::unbounded_channel();

        let mut chrome = ChromeComponent::new();
        chrome.register_action_handler(action_tx.clone());

        let mut sessions_browser = SessionsBrowserComponent::new(workspace_root);
        sessions_browser.register_action_handler(action_tx.clone());

        let mut help_page = StaticPageComponent::new(
            "Help",
            "Ctrl+1-9: switch tabs | Up/Down: navigate | Enter: expand | r: refresh | Ctrl+C: quit",
        );
        help_page.register_action_handler(action_tx.clone());

        let mut settings_page = StaticPageComponent::new("Settings", "Not yet implemented.");
        settings_page.register_action_handler(action_tx.clone());

        Self {
            active_tab: ActiveTab::Sessions,
            should_quit: false,
            action_tx,
            action_rx,
            chrome,
            sessions_browser,
            help_page,
            settings_page,
        }
    }

    /// Run the TUI event loop until the app exits.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal cannot be initialized or restored, or
    /// if event routing or drawing fails.
    pub async fn run(&mut self) -> miette::Result<()> {
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = Terminal::new(backend)
            .map_err(|error| miette::miette!("failed to create terminal: {error}"))?;
        let mut tui = Tui::new(terminal);
        tui.enter()?;

        self.send_action(Action::SwitchTab(self.active_tab));
        self.send_action(Action::Render);

        while !self.should_quit {
            tokio::select! {
                maybe_event = tui.next() => {
                    match maybe_event {
                        Some(Event::Tick) => self.send_action(Action::Tick),
                        Some(Event::Render) => self.send_action(Action::Render),
                        Some(Event::Resize(size)) => self.send_action(Action::Resize(size)),
                        Some(Event::Key(key)) => self.route_key(key)?,
                        Some(Event::Mouse(mouse)) => self.route_mouse(mouse, tui.size()?)?,
                        None => break,
                    }
                }
                maybe_action = self.action_rx.recv() => {
                    match maybe_action {
                        Some(action) => self.handle_action(&action, &mut tui)?,
                        None => break,
                    }
                }
            }
        }

        tui.exit()?;
        Ok(())
    }

    fn tab_bar_entries() -> Vec<TabBarEntry> {
        vec![
            TabBarEntry::Utility {
                tab: ActiveTab::Sessions,
                label: "Sessions",
            },
            TabBarEntry::Utility {
                tab: ActiveTab::Settings,
                label: "Settings",
            },
            TabBarEntry::Utility {
                tab: ActiveTab::Help,
                label: "Help",
            },
        ]
    }

    fn build_chrome_model(&self) -> ChromeModel {
        ChromeModel {
            active_tab: self.active_tab,
            entries: Self::tab_bar_entries(),
            waiting: false,
            status: String::new(),
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame) {
        self.chrome.sync(self.build_chrome_model());

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(TAB_BAR_HEIGHT)])
            .split(frame.area());

        match self.active_tab {
            ActiveTab::Session(_) | ActiveTab::Sessions => {
                self.sessions_browser.draw(frame, chunks[0]);
            }
            ActiveTab::Settings => self.settings_page.draw(frame, chunks[0]),
            ActiveTab::Help => self.help_page.draw(frame, chunks[0]),
        }

        self.chrome.draw(frame, chunks[1]);
    }

    fn send_action(&self, action: Action) {
        let _ = self.action_tx.send(action);
    }

    fn route_key(&mut self, key: KeyEvent) -> miette::Result<()> {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.send_action(Action::Quit);
                return Ok(());
            }
            KeyCode::Char(c @ '1'..='9') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let index = (c as usize) - ('1' as usize);
                let entries = Self::tab_bar_entries();
                if let Some(TabBarEntry::Utility { tab, .. }) = entries.get(index) {
                    self.send_action(Action::SwitchTab(*tab));
                }
                return Ok(());
            }
            _ => {}
        }

        match self.active_tab {
            ActiveTab::Sessions => self.sessions_browser.handle_key_event(key)?,
            ActiveTab::Settings => self.settings_page.handle_key_event(key)?,
            ActiveTab::Help => self.help_page.handle_key_event(key)?,
            ActiveTab::Session(_) => {}
        }

        Ok(())
    }

    fn route_mouse(
        &mut self,
        mouse: MouseEvent,
        size: ratatui::layout::Size,
    ) -> miette::Result<()> {
        let root = Rect::new(0, 0, size.width, size.height);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(TAB_BAR_HEIGHT)])
            .split(root);

        if mouse.row == chunks[1].y {
            return self.chrome.handle_mouse_event(mouse, chunks[1]);
        }

        match self.active_tab {
            ActiveTab::Sessions => self.sessions_browser.handle_mouse_event(mouse, chunks[0])?,
            ActiveTab::Settings => self.settings_page.handle_mouse_event(mouse, chunks[0])?,
            ActiveTab::Help => self.help_page.handle_mouse_event(mouse, chunks[0])?,
            ActiveTab::Session(_) => {}
        }

        Ok(())
    }

    fn handle_action(&mut self, action: &Action, tui: &mut Tui) -> miette::Result<()> {
        match action {
            Action::Render => tui.draw(|frame| self.draw(frame))?,
            Action::Quit => self.should_quit = true,
            Action::SwitchTab(tab) => self.active_tab = *tab,
            Action::SessionsBack => self.handle_sessions_back(),
            _ => {}
        }
        self.update_components(action)?;
        Ok(())
    }

    fn handle_sessions_back(&mut self) {
        if self.sessions_browser.has_detail() {
            self.send_action(Action::SessionsCloseDetail);
        }
    }

    fn update_components(&mut self, action: &Action) -> miette::Result<()> {
        self.chrome.update(action)?;
        self.sessions_browser.update(action)?;
        Ok(())
    }
}
