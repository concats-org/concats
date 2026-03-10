use std::{io, path::PathBuf};

use concats_acp::{SessionHandle, start_session};
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
        agent_picker::AgentPickerComponent,
        chrome::{ChromeComponent, ChromeModel, TAB_BAR_HEIGHT},
        session::SessionComponent,
        sessions::SessionsBrowserComponent,
        static_page::StaticPageComponent,
    },
    launch::{SessionLaunchSpec, SessionTabConfig, fork_tab_label},
    tabs::{ActiveTab, TabBarEntry},
    tui::{Event, Tui},
};

pub struct ForkRequest {
    pub commit_oid: concats_core::Oid,
    pub source_session_id: String,
}

pub struct App {
    session_tabs: Vec<SessionComponent>,
    active_tab: ActiveTab,
    next_session_id: u32,
    should_quit: bool,
    pub auto_push: bool,
    pub push_remote: String,
    workspace_root: PathBuf,
    available_agents: Vec<(String, concats_config::AgentConfig)>,
    action_tx: mpsc::UnboundedSender<Action>,
    action_rx: mpsc::UnboundedReceiver<Action>,
    chrome: ChromeComponent,
    sessions_browser: SessionsBrowserComponent,
    help_page: StaticPageComponent,
    settings_page: StaticPageComponent,
    agent_picker: Option<AgentPickerComponent>,
}

impl App {
    #[must_use]
    pub fn new(
        workspace_root: PathBuf,
        available_agents: Vec<(String, concats_config::AgentConfig)>,
    ) -> Self {
        let (action_tx, action_rx) = mpsc::unbounded_channel();

        let mut chrome = ChromeComponent::new();
        chrome.register_action_handler(action_tx.clone());

        let mut sessions_browser = SessionsBrowserComponent::new(workspace_root.clone());
        sessions_browser.register_action_handler(action_tx.clone());

        let mut help_page = StaticPageComponent::new(
            "Help",
            "Ctrl+N: new session | Ctrl+W: close tab | Ctrl+1-9: switch tabs | Up/Down: navigate | Enter: expand | f: fork | r: refresh | Ctrl+C: quit",
        );
        help_page.register_action_handler(action_tx.clone());

        let mut settings_page = StaticPageComponent::new("Settings", "Not yet implemented.");
        settings_page.register_action_handler(action_tx.clone());

        Self {
            session_tabs: Vec::new(),
            active_tab: ActiveTab::Sessions,
            next_session_id: 0,
            should_quit: false,
            auto_push: false,
            push_remote: String::from("origin"),
            workspace_root,
            available_agents,
            action_tx,
            action_rx,
            chrome,
            sessions_browser,
            help_page,
            settings_page,
            agent_picker: None,
        }
    }

    pub fn set_active_tab(&mut self, tab: ActiveTab) {
        self.active_tab = tab;
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
                        Some(action) => self.handle_action(action, &mut tui).await?,
                        None => break,
                    }
                }
            }
        }

        tui.exit()?;
        Ok(())
    }

    #[must_use]
    pub fn active_session(&self) -> Option<&SessionComponent> {
        if let ActiveTab::Session(id) = self.active_tab {
            self.session_tabs.iter().find(|tab| tab.id() == id)
        } else {
            None
        }
    }

    pub fn active_session_mut(&mut self) -> Option<&mut SessionComponent> {
        if let ActiveTab::Session(id) = self.active_tab {
            self.session_tabs.iter_mut().find(|tab| tab.id() == id)
        } else {
            None
        }
    }

    pub fn add_session(
        &mut self,
        session: SessionHandle,
        label: &str,
        tab_config: SessionTabConfig,
    ) -> u32 {
        let id = self.next_session_id;
        self.next_session_id += 1;

        let final_label = self.deduplicate_label(label);
        let mut component = SessionComponent::new(id, final_label, session, tab_config);
        component.register_action_handler(self.action_tx.clone());

        let Some(mut session_rx) = component.session_handle_mut().take_event_rx() else {
            component.push_system_message("Session event stream was already attached.");
            component.mark_closed();
            self.session_tabs.push(component);
            return id;
        };
        let action_tx = self.action_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = session_rx.recv().await {
                if action_tx
                    .send(Action::SessionEvent { tab_id: id, event })
                    .is_err()
                {
                    return;
                }
            }
            let _ = action_tx.send(Action::SessionClosed(id));
        });

        self.session_tabs.push(component);
        id
    }

    fn tab_bar_entries(&self) -> Vec<TabBarEntry> {
        let mut entries = self
            .session_tabs
            .iter()
            .map(|tab| TabBarEntry::Session {
                id: tab.id(),
                label: tab.label().to_string(),
            })
            .collect::<Vec<_>>();
        entries.push(TabBarEntry::NewButton);
        entries.push(TabBarEntry::Utility {
            tab: ActiveTab::Sessions,
            label: "Sessions",
        });
        entries.push(TabBarEntry::Utility {
            tab: ActiveTab::Settings,
            label: "Settings",
        });
        entries.push(TabBarEntry::Utility {
            tab: ActiveTab::Help,
            label: "Help",
        });
        entries
    }

    fn build_chrome_model(&self) -> ChromeModel {
        let (waiting, status) = self
            .active_session()
            .map_or((false, "no session".into()), |tab| {
                (tab.waiting(), tab.status().to_string())
            });

        ChromeModel {
            active_tab: self.active_tab,
            entries: self.tab_bar_entries(),
            waiting,
            status,
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame) {
        self.chrome.sync(self.build_chrome_model());

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(TAB_BAR_HEIGHT)])
            .split(frame.area());

        match self.active_tab {
            ActiveTab::Session(id) => {
                if let Some(index) = self.session_tabs.iter().position(|tab| tab.id() == id) {
                    self.session_tabs[index].draw(frame, chunks[0]);
                } else {
                    self.help_page.draw(frame, chunks[0]);
                }
            }
            ActiveTab::Sessions => self.sessions_browser.draw(frame, chunks[0]),
            ActiveTab::Settings => self.settings_page.draw(frame, chunks[0]),
            ActiveTab::Help => self.help_page.draw(frame, chunks[0]),
        }

        self.chrome.draw(frame, chunks[1]);

        if let Some(agent_picker) = &mut self.agent_picker {
            agent_picker.draw(frame, frame.area());
        }
    }

    fn send_action(&self, action: Action) {
        let _ = self.action_tx.send(action);
    }

    fn route_key(&mut self, key: KeyEvent) -> miette::Result<()> {
        if let Some(agent_picker) = &mut self.agent_picker {
            return agent_picker.handle_key_event(key);
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.send_action(Action::Quit);
                return Ok(());
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.send_action(Action::OpenAgentPicker);
                return Ok(());
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.send_action(Action::CloseActiveSession);
                return Ok(());
            }
            KeyCode::Char(c @ '1'..='9') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let index = (c as usize) - ('1' as usize);
                let entries = self.tab_bar_entries();
                let mut tab_index = 0usize;
                for entry in &entries {
                    match entry {
                        TabBarEntry::Session { id, .. } => {
                            if tab_index == index {
                                self.send_action(Action::SwitchTab(ActiveTab::Session(*id)));
                                return Ok(());
                            }
                            tab_index += 1;
                        }
                        TabBarEntry::NewButton => {}
                        TabBarEntry::Utility { tab, .. } => {
                            if tab_index == index {
                                self.send_action(Action::SwitchTab(*tab));
                                return Ok(());
                            }
                            tab_index += 1;
                        }
                    }
                }
                return Ok(());
            }
            _ => {}
        }

        match self.active_tab {
            ActiveTab::Session(_) => {
                if let Some(tab) = self.active_session_mut() {
                    tab.handle_key_event(key)?;
                }
            }
            ActiveTab::Sessions => self.sessions_browser.handle_key_event(key)?,
            ActiveTab::Settings => self.settings_page.handle_key_event(key)?,
            ActiveTab::Help => self.help_page.handle_key_event(key)?,
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
            ActiveTab::Session(_) => {
                if let Some(tab) = self.active_session_mut() {
                    tab.handle_mouse_event(mouse, chunks[0])?;
                }
            }
            ActiveTab::Sessions => self.sessions_browser.handle_mouse_event(mouse, chunks[0])?,
            ActiveTab::Settings => self.settings_page.handle_mouse_event(mouse, chunks[0])?,
            ActiveTab::Help => self.help_page.handle_mouse_event(mouse, chunks[0])?,
        }

        Ok(())
    }

    async fn handle_action(&mut self, action: Action, tui: &mut Tui) -> miette::Result<()> {
        match action {
            Action::SessionEvent { tab_id, event } => {
                self.apply_session_event(tab_id, event);
                return Ok(());
            }
            Action::SessionClosed(tab_id) => {
                self.mark_session_closed(tab_id);
                return Ok(());
            }
            action => {
                self.handle_non_session_action(&action, tui).await?;
                self.update_components(&action)?;
            }
        }
        Ok(())
    }

    fn deduplicate_label(&self, label: &str) -> String {
        if !self.session_tabs.iter().any(|tab| tab.label() == label) {
            return label.to_string();
        }

        for suffix in 2.. {
            let candidate = format!("{label} ({suffix})");
            if !self.session_tabs.iter().any(|tab| tab.label() == candidate) {
                return candidate;
            }
        }
        unreachable!()
    }

    fn remove_session(&mut self, id: u32) {
        if let Some(position) = self.session_tabs.iter().position(|tab| tab.id() == id) {
            self.session_tabs.remove(position);

            if self.active_tab == ActiveTab::Session(id) {
                let neighbor = self
                    .session_tabs
                    .get(position)
                    .or_else(|| {
                        position
                            .checked_sub(1)
                            .and_then(|index| self.session_tabs.get(index))
                    })
                    .map(SessionComponent::id);
                self.active_tab = neighbor.map_or(ActiveTab::Sessions, ActiveTab::Session);
            }
        }
    }

    fn close_session(&mut self, id: u32) {
        if let Some(tab) = self.session_tabs.iter_mut().find(|tab| tab.id() == id)
            && tab.request_close()
        {
            return;
        }

        self.remove_session(id);
    }

    fn create_session_from_agent(&mut self, agent_index: usize) {
        let Some((agent_id, agent_config)) = self.available_agents.get(agent_index).cloned() else {
            return;
        };

        let launch = SessionLaunchSpec::new(
            self.workspace_root.clone(),
            &agent_id,
            &agent_config,
            self.auto_push,
            &self.push_remote,
            None,
            None,
        );

        match self.start_session_tab(launch) {
            Ok(new_id) => self.active_tab = ActiveTab::Session(new_id),
            Err(error) => {
                if let Some(tab) = self.active_session_mut() {
                    tab.push_system_message(format!("Failed to start session: {error}"));
                }
            }
        }
    }

    fn fork_from_selected(&self) -> Option<ForkRequest> {
        let (source_session_id, commit_oid) = self.sessions_browser.selected_fork_info()?;
        Some(ForkRequest {
            commit_oid,
            source_session_id,
        })
    }

    fn handle_fork(&mut self) {
        let Some(fork_request) = self.fork_from_selected() else {
            self.push_active_session_message("No turn selected to fork from.");
            return;
        };

        self.warn_if_fork_will_overwrite_changes();
        if let Err(error) = self.restore_fork_checkpoint(&fork_request) {
            self.push_active_session_message(format!(
                "Failed to restore working directory: {error}"
            ));
            return;
        }

        let Some((agent_id, agent_config, auto_push, push_remote)) = self.fork_agent_context()
        else {
            return;
        };

        let launch = SessionLaunchSpec::new(
            self.workspace_root.clone(),
            &agent_id,
            &agent_config,
            auto_push,
            &push_remote,
            Some(fork_request.commit_oid),
            Some(fork_tab_label(&fork_request.source_session_id)),
        );

        match self.start_session_tab(launch) {
            Ok(new_id) => {
                if let Some(tab) = self.session_tabs.iter_mut().find(|tab| tab.id() == new_id) {
                    tab.queue_fork_message(
                        &fork_request.source_session_id,
                        fork_request.commit_oid,
                    );
                }
                self.active_tab = ActiveTab::Session(new_id);
            }
            Err(error) => {
                self.push_active_session_message(format!("Failed to start fork: {error}"));
            }
        }
    }

    fn apply_session_event(&mut self, tab_id: u32, event: concats_acp::SessionEvent) {
        if let Some(tab) = self.session_tabs.iter_mut().find(|tab| tab.id() == tab_id) {
            tab.handle_session_event(event);
        }
    }

    fn mark_session_closed(&mut self, tab_id: u32) {
        if let Some(index) = self.session_tabs.iter().position(|tab| tab.id() == tab_id) {
            if self.session_tabs[index].close_requested() {
                self.remove_session(tab_id);
            } else {
                self.session_tabs[index].mark_closed();
            }
        }
    }

    async fn handle_non_session_action(
        &mut self,
        action: &Action,
        tui: &mut Tui,
    ) -> miette::Result<()> {
        match action {
            Action::Render => tui.draw(|frame| self.draw(frame))?,
            Action::Quit => self.should_quit = true,
            Action::SwitchTab(tab) => self.active_tab = *tab,
            Action::OpenAgentPicker => self.open_agent_picker(),
            Action::CloseAgentPicker => self.agent_picker = None,
            Action::CreateSession(agent_index) => {
                self.agent_picker = None;
                self.create_session_from_agent(*agent_index);
            }
            Action::CloseSession(id) => self.close_session(*id),
            Action::CloseActiveSession => self.close_active_session(),
            Action::SessionSubmitPrompt(tab_id) => self.submit_prompt(*tab_id).await,
            Action::SessionsBack => self.handle_sessions_back(),
            Action::ForkSelected => self.handle_fork(),
            _ => {}
        }
        Ok(())
    }

    fn open_agent_picker(&mut self) {
        if self.available_agents.len() == 1 {
            self.send_action(Action::CreateSession(0));
            return;
        }

        if self.available_agents.is_empty() {
            self.push_active_session_message("No agents configured.");
            return;
        }

        if self.agent_picker.is_none() {
            let mut picker = AgentPickerComponent::new(
                self.available_agents
                    .iter()
                    .map(|(id, cfg)| (id.clone(), cfg.display_name(id)))
                    .collect(),
            );
            picker.register_action_handler(self.action_tx.clone());
            self.agent_picker = Some(picker);
        }
    }

    fn close_active_session(&mut self) {
        if let ActiveTab::Session(id) = self.active_tab {
            self.close_session(id);
        }
    }

    async fn submit_prompt(&mut self, tab_id: u32) {
        if let Some(tab) = self.session_tabs.iter_mut().find(|tab| tab.id() == tab_id) {
            tab.send_prompt().await;
        }
    }

    fn handle_sessions_back(&mut self) {
        if self.sessions_browser.has_detail() {
            self.send_action(Action::SessionsCloseDetail);
        } else if let Some(tab) = self.session_tabs.first() {
            self.send_action(Action::SwitchTab(ActiveTab::Session(tab.id())));
        }
    }

    fn update_components(&mut self, action: &Action) -> miette::Result<()> {
        self.chrome.update(action)?;
        self.sessions_browser.update(action)?;
        if let Some(agent_picker) = &mut self.agent_picker {
            agent_picker.update(action)?;
        }
        for tab in &mut self.session_tabs {
            tab.update(action)?;
        }
        Ok(())
    }

    fn push_active_session_message(&mut self, message: impl Into<String>) {
        if let Some(tab) = self.active_session_mut() {
            tab.push_system_message(message);
        }
    }

    fn warn_if_fork_will_overwrite_changes(&mut self) {
        if let Ok(repo) = git2::Repository::open(&self.workspace_root)
            && let Ok(statuses) = repo.statuses(None)
            && statuses.iter().any(|status| {
                status.status().intersects(
                    git2::Status::WT_MODIFIED
                        | git2::Status::WT_NEW
                        | git2::Status::INDEX_MODIFIED
                        | git2::Status::INDEX_NEW,
                )
            })
        {
            self.push_active_session_message(
                "Warning: uncommitted changes in working directory will be overwritten by fork.",
            );
        }
    }

    fn restore_fork_checkpoint(
        &self,
        fork_request: &ForkRequest,
    ) -> concats_core::error::Result<()> {
        let session =
            concats_core::session::open(&self.workspace_root, &fork_request.source_session_id)?;
        let checkpoint = concats_core::checkpoint::get(&session, fork_request.commit_oid)?;
        concats_core::checkpoint::restore(&checkpoint)
    }

    fn fork_agent_context(&self) -> Option<(String, concats_config::AgentConfig, bool, String)> {
        if let Some(active) = self.active_session() {
            return Some((
                active.agent_label().to_string(),
                concats_config::AgentConfig {
                    name: active.agent_label().to_string(),
                    command: active.agent_command().to_string(),
                    args: active.agent_args().to_vec(),
                    env: active.agent_env().clone(),
                },
                active.auto_push(),
                active.push_remote().to_string(),
            ));
        }

        self.available_agents.first().map(|(id, cfg)| {
            (
                id.clone(),
                cfg.clone(),
                self.auto_push,
                self.push_remote.clone(),
            )
        })
    }

    fn start_session_tab(
        &mut self,
        launch: SessionLaunchSpec,
    ) -> Result<u32, concats_acp::error::Error> {
        let SessionLaunchSpec {
            label,
            session_config,
            tab_config,
        } = launch;
        let session = start_session(session_config)?;
        Ok(self.add_session(session, &label, tab_config))
    }
}
