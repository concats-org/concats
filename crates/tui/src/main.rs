use std::io;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use crossterm::ExecutableCommand;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use tokio_stream::StreamExt;

use concats_config::{ConfigCliArgs, load_config, save_config};
use concats_core::session::{SessionConfig, start_session};
use concats_registry::{fetch_registry, install_agents};

use tui::app::{self, AgentPickerState};
use tui::input::{self, InputAction};
use tui::tabs::{ActiveTab, ClickTarget};
use tui::ui;

#[derive(Parser)]
#[command(about = "Concats \u{2013} git-native session history for coding agents")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch the TUI for interacting with an ACP-compatible coding agent.
    Run {
        /// Agent to use (name from config or ACP registry).
        agent: Option<String>,

        /// Workspace root directory (defaults to current directory).
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Handle a Claude Code hook event (reads JSON from stdin).
    Hook {
        /// The hook event name (SessionStart, UserPromptSubmit, PostToolUse, Stop).
        event: String,
    },

    /// Manage Claude Code hook integration.
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
}

#[derive(Subcommand)]
enum HooksAction {
    /// Install concats hooks into .claude/settings.json.
    Install {
        /// Project root directory (defaults to current directory).
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
}

fn main() -> miette::Result<()> {
    // Initialize tracing (logs go to stderr to avoid interfering with TUI).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Hook { event }) => run_hook_command(&event),
        Some(Commands::Hooks { action }) => run_hooks_action(action),
        Some(Commands::Run { agent, workspace }) => run_tui_command(agent, workspace),
        // Default to `run` when no subcommand is given (backwards compat).
        None => run_tui_command(None, None),
    }
}

// -- Hook command ────────────────────────────────────────────────────

fn run_hook_command(event: &str) -> miette::Result<()> {
    let stdin = io::read_to_string(io::stdin())
        .map_err(|e| miette::miette!("failed to read stdin: {e}"))?;

    match event {
        "SessionStart" => {
            let payload: concats_core::hook::SessionStartPayload = serde_json::from_str(&stdin)
                .map_err(|e| miette::miette!("invalid SessionStart payload: {e}"))?;
            concats_core::hook::handle_session_start(&payload)
                .map_err(|e| miette::miette!("{e}"))?;
        }
        "UserPromptSubmit" => {
            let payload: concats_core::hook::UserPromptSubmitPayload = serde_json::from_str(&stdin)
                .map_err(|e| miette::miette!("invalid UserPromptSubmit payload: {e}"))?;
            concats_core::hook::handle_user_prompt_submit(&payload)
                .map_err(|e| miette::miette!("{e}"))?;
        }
        "PostToolUse" => {
            let payload: concats_core::hook::PostToolUsePayload = serde_json::from_str(&stdin)
                .map_err(|e| miette::miette!("invalid PostToolUse payload: {e}"))?;
            concats_core::hook::handle_post_tool_use(&payload)
                .map_err(|e| miette::miette!("{e}"))?;
        }
        "Stop" => {
            let payload: concats_core::hook::StopPayload = serde_json::from_str(&stdin)
                .map_err(|e| miette::miette!("invalid Stop payload: {e}"))?;
            concats_core::hook::handle_stop(&payload).map_err(|e| miette::miette!("{e}"))?;
        }
        _ => {
            return Err(miette::miette!(
                "unknown hook event: {event}. Expected one of: SessionStart, UserPromptSubmit, PostToolUse, Stop"
            ));
        }
    }

    Ok(())
}

// -- Hooks management ────────────────────────────────────────────────

fn run_hooks_action(action: HooksAction) -> miette::Result<()> {
    match action {
        HooksAction::Install { path } => {
            let project_root =
                path.unwrap_or_else(|| std::env::current_dir().expect("cannot determine cwd"));
            let binary_name = std::env::current_exe()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "concats".into());
            concats_core::hook::install_hooks(&project_root, &binary_name)
                .map_err(|e| miette::miette!("{e}"))?;
            eprintln!(
                "hooks installed in {}",
                project_root.join(".claude/settings.json").display()
            );
            Ok(())
        }
    }
}

// -- TUI command (original main logic) ───────────────────────────────

fn run_tui_command(agent: Option<String>, workspace: Option<PathBuf>) -> miette::Result<()> {
    let cli_args = ConfigCliArgs {
        default_agent: agent.clone(),
        workspace: workspace.clone(),
    };

    let mut config = load_config(&cli_args)?;

    // Build the multi-threaded tokio runtime (needed for registry fetch and TUI).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| miette::miette!("failed to build runtime: {e}"))?;

    // Resolve which agent to use.
    let agent_id = agent
        .or(config.default_agent.clone())
        .ok_or_else(|| miette::miette!("no agent specified. Usage: concats run <agent-name>"))?;

    // Look up the agent in config; if missing, try to install from registry.
    if resolve_agent_id(&agent_id, &config).is_none() {
        eprintln!("agent '{agent_id}' not found in config, fetching from ACP registry...");
        rt.block_on(sync_registry(&mut config))?;
    }

    let resolved_id = resolve_agent_id(&agent_id, &config).ok_or_else(|| {
        let mut available: Vec<_> = config.agents.keys().cloned().collect();
        available.sort();
        miette::miette!(
            "agent '{agent_id}' not found in the ACP registry.\n\
             Available agents: {}",
            available.join(", ")
        )
    })?;

    let agent_config = config.agents[&resolved_id].clone();

    let workspace_root = config
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("could not determine current directory"));

    // Build the list of available agents for the picker.
    let mut available_agents: Vec<(String, concats_config::AgentConfig)> = config
        .agents
        .iter()
        .map(|(id, cfg)| (id.clone(), cfg.clone()))
        .collect();
    available_agents.sort_by(|a, b| a.0.cmp(&b.0));

    let auto_push = config.sync.auto_push;
    let push_remote = config.sync.remote.clone();

    let session_config = SessionConfig {
        agent_command: agent_config.command.clone(),
        agent_args: agent_config.args.clone(),
        workspace_root: workspace_root.clone(),
        env: agent_config.env.clone(),
        fork_from: None,
        auto_push,
        push_remote: push_remote.clone(),
    };

    // Start the initial session (spawns a dedicated thread).
    let session = start_session(session_config).map_err(|e| miette::miette!("{e}"))?;

    rt.block_on(run_tui(
        session,
        workspace_root,
        agent_config,
        resolved_id,
        available_agents,
        auto_push,
        push_remote,
    ))?;
    Ok(())
}

/// Resolve a user-provided agent name to an agent ID in the config.
///
/// Tries, in order: exact match, prefix match, contains match.
/// Returns `None` if no match or if the match is ambiguous.
fn resolve_agent_id(input: &str, config: &concats_config::Config) -> Option<String> {
    // Exact match.
    if config.agents.contains_key(input) {
        return Some(input.to_string());
    }

    let input_lower = input.to_lowercase();

    // Prefix match (e.g. "claude" matches "claude-acp").
    let prefix_matches: Vec<_> = config
        .agents
        .keys()
        .filter(|id| id.to_lowercase().starts_with(&input_lower))
        .collect();
    if prefix_matches.len() == 1 {
        return Some(prefix_matches[0].clone());
    }

    // Contains match (e.g. "copilot" matches "github-copilot").
    let contains_matches: Vec<_> = config
        .agents
        .keys()
        .filter(|id| id.to_lowercase().contains(&input_lower))
        .collect();
    if contains_matches.len() == 1 {
        return Some(contains_matches[0].clone());
    }

    None
}

/// Fetch the ACP registry, merge agents into config, and save.
async fn sync_registry(config: &mut concats_config::model::Config) -> miette::Result<()> {
    let registry = fetch_registry().await?;
    eprintln!(
        "fetched {} agents from ACP registry (v{})",
        registry.agents.len(),
        registry.version
    );
    install_agents(&registry, config);
    save_config(config)?;
    eprintln!("saved {} agents to config", config.agents.len());
    Ok(())
}

async fn run_tui(
    initial_session: concats_core::session::SessionHandle,
    workspace_root: PathBuf,
    initial_agent_config: concats_config::AgentConfig,
    resolved_agent_id: String,
    available_agents: Vec<(String, concats_config::AgentConfig)>,
    auto_push: bool,
    push_remote: String,
) -> miette::Result<()> {
    // Set up terminal.
    enable_raw_mode().map_err(|e| miette::miette!("failed to enable raw mode: {e}"))?;
    let mut stdout = io::stdout();
    stdout
        .execute(EnterAlternateScreen)
        .map_err(|e| miette::miette!("failed to enter alternate screen: {e}"))?;
    stdout
        .execute(EnableMouseCapture)
        .map_err(|e| miette::miette!("failed to enable mouse capture: {e}"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| miette::miette!("failed to create terminal: {e}"))?;

    let mut app = app::App::new(workspace_root, available_agents);
    app.auto_push = auto_push;
    app.push_remote = push_remote;

    // Add the initial session as the first tab.
    let initial_label = if initial_agent_config.name.trim().is_empty() {
        resolved_agent_id.clone()
    } else {
        initial_agent_config.name.clone()
    };
    let initial_id = app.add_session(
        initial_session,
        initial_label,
        &resolved_agent_id,
        &initial_agent_config,
    );
    app.switch_tab(ActiveTab::Session(initial_id));

    let result = event_loop(&mut terminal, &mut app).await;

    // Restore terminal.
    disable_raw_mode().ok();
    io::stdout().execute(DisableMouseCapture).ok();
    io::stdout().execute(LeaveAlternateScreen).ok();

    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut app::App<'_>,
) -> miette::Result<()> {
    let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(80));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut reader = EventStream::new();

    let mut needs_draw = true;

    loop {
        if needs_draw {
            terminal
                .draw(|f| ui::render(f, &mut *app))
                .map_err(|e| miette::miette!("draw error: {e}"))?;
            needs_draw = false;
        }

        // Poll for crossterm events and session events via fan-in channel.
        tokio::select! {
            // Tick for spinner animation.
            _ = tick_interval.tick() => {
                let any_waiting = app.session_tabs.iter().any(|t| t.waiting);
                if any_waiting {
                    app.tick = app.tick.wrapping_add(1);
                    needs_draw = true;
                }
            }
            // Check for keyboard/mouse input.
            maybe_event = reader.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        let is_session_tab = matches!(app.active_tab, ActiveTab::Session(_));
                        let is_submit = key.code == KeyCode::Enter
                            && !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
                            && app.active_session().is_some_and(|t| !t.waiting)
                            && is_session_tab
                            && app.agent_picker.is_none();
                        if is_submit {
                            if let Some(tab) = app.active_session_mut() {
                                tab.send_prompt().await;
                            }
                        } else if key.code == KeyCode::Enter
                            && key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
                            && app.active_session().is_some_and(|t| !t.waiting)
                            && is_session_tab
                            && app.agent_picker.is_none()
                        {
                            // Insert newline in textarea for Alt+Enter / Shift+Enter.
                            if let Some(tab) = app.active_session_mut() {
                                tab.textarea.insert_newline();
                            }
                        } else {
                            let action = input::handle_key_event(app, key);
                            handle_input_action(app, action).await;
                        }
                        needs_draw = true;
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        let size = terminal.size().unwrap_or_default();
                        let terminal_area = Rect::new(0, 0, size.width, size.height);
                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                // Check if click is on the tab/menu bar (last row).
                                if mouse.row == size.height.saturating_sub(1) {
                                    if let Some(target) = target_from_click(mouse.column, app) {
                                        match target {
                                            ClickTarget::SwitchTab(tab) => {
                                                app.switch_tab(tab);
                                            }
                                            ClickTarget::CloseSession(id) => {
                                                app.close_session(id);
                                            }
                                            ClickTarget::NewSession => {
                                                handle_input_action(app, InputAction::NewSession).await;
                                            }
                                        }
                                    }
                                    needs_draw = true;
                                }
                            }
                            MouseEventKind::ScrollUp => {
                                if scroll_under_mouse(
                                    app,
                                    terminal_area,
                                    mouse.column,
                                    mouse.row,
                                    -3,
                                ) {
                                    needs_draw = true;
                                }
                            }
                            MouseEventKind::ScrollDown => {
                                if scroll_under_mouse(
                                    app,
                                    terminal_area,
                                    mouse.column,
                                    mouse.row,
                                    3,
                                ) {
                                    needs_draw = true;
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            // Check for session events via the fan-in channel.
            session_event = app.session_event_rx.recv() => {
                if let Some((tab_id, fan_in_event)) = session_event {
                    if let Some(tab) = app.session_tabs.iter_mut().find(|t| t.id == tab_id) {
                        match fan_in_event {
                            app::FanInEvent::Session(event) => {
                                tab.handle_session_event(event);
                            }
                            app::FanInEvent::ChannelClosed => {
                                tab.waiting = false;
                                tab.status = "session ended".into();
                            }
                        }
                    }
                }
                needs_draw = true;
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

/// Handle an InputAction that requires async work or state changes.
async fn handle_input_action(app: &mut app::App<'_>, action: InputAction) {
    match action {
        InputAction::None => {}
        InputAction::Fork => {
            handle_fork(app).await;
        }
        InputAction::NewSession => {
            if app.available_agents.len() == 1 {
                // Single agent -- create immediately.
                create_session_from_agent(app, 0);
            } else if app.available_agents.is_empty() {
                // No agents configured.
                if let Some(tab) = app.active_session_mut() {
                    tab.messages
                        .push(app::Message::System("No agents configured.".into()));
                }
            } else {
                // Open the agent picker.
                app.agent_picker = Some(AgentPickerState {
                    agents: app
                        .available_agents
                        .iter()
                        .map(|(id, cfg)| {
                            let display = if cfg.name.trim().is_empty() {
                                id.clone()
                            } else {
                                cfg.name.clone()
                            };
                            (id.clone(), display)
                        })
                        .collect(),
                    selected: 0,
                });
            }
        }
        InputAction::CloseActiveSession => {
            if let ActiveTab::Session(id) = app.active_tab {
                app.close_session(id);
            }
        }
        InputAction::CreateSession(agent_idx) => {
            create_session_from_agent(app, agent_idx);
        }
    }
}

/// Create a new session tab from an agent at the given index.
fn create_session_from_agent(app: &mut app::App<'_>, agent_idx: usize) {
    let Some((agent_id, agent_config)) = app.available_agents.get(agent_idx).cloned() else {
        return;
    };

    let session_config = SessionConfig {
        agent_command: agent_config.command.clone(),
        agent_args: agent_config.args.clone(),
        workspace_root: app.workspace_root.clone(),
        env: agent_config.env.clone(),
        fork_from: None,
        auto_push: app.auto_push,
        push_remote: app.push_remote.clone(),
    };

    match start_session(session_config) {
        Ok(session) => {
            let label = if agent_config.name.trim().is_empty() {
                agent_id.clone()
            } else {
                agent_config.name.clone()
            };
            let new_id = app.add_session(session, label, &agent_id, &agent_config);
            app.switch_tab(ActiveTab::Session(new_id));
        }
        Err(e) => {
            if let Some(tab) = app.active_session_mut() {
                tab.messages.push(app::Message::System(format!(
                    "Failed to start session: {e}"
                )));
            }
        }
    }
}

/// Map an x-coordinate click on the tab/menu row to a ClickTarget.
fn target_from_click(x: u16, app: &app::App<'_>) -> Option<ClickTarget> {
    let x = x as usize;
    for (target, start, end) in ui::tab_click_hitboxes(app) {
        if x >= start && x < end {
            return Some(target);
        }
    }
    None
}

fn scroll_under_mouse(
    app: &mut app::App<'_>,
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
            tab.focused_panel = app::FocusedPanel::Conversation;
            tab.scroll_offset = apply_scroll_delta(tab.scroll_offset, delta);
            return true;
        }

        if rect_contains(panel_chunks[1], column, row) {
            tab.focused_panel = app::FocusedPanel::Stderr;
            tab.stderr_scroll = apply_scroll_delta(tab.stderr_scroll, delta);
            return true;
        }
    } else {
        tab.focused_panel = app::FocusedPanel::Conversation;
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

fn apply_scroll_delta(current: u16, delta: i16) -> u16 {
    if delta >= 0 {
        current.saturating_add(delta as u16)
    } else {
        current.saturating_sub(delta.unsigned_abs())
    }
}

/// Handle a fork request from the Sessions tab.
/// Creates a **new tab** instead of replacing the current session.
async fn handle_fork(app: &mut app::App<'_>) {
    let fork_request = match app.fork_from_selected_turn() {
        Some(req) => req,
        None => {
            if let Some(tab) = app.active_session_mut() {
                tab.messages.push(app::Message::System(
                    "No turn selected to fork from.".into(),
                ));
            }
            return;
        }
    };

    // Check for uncommitted changes.
    if let Ok(repo) = git2::Repository::open(&app.workspace_root)
        && let Ok(statuses) = repo.statuses(None)
    {
        let dirty = statuses.iter().any(|s| {
            s.status().intersects(
                git2::Status::WT_MODIFIED
                    | git2::Status::WT_NEW
                    | git2::Status::INDEX_MODIFIED
                    | git2::Status::INDEX_NEW,
            )
        });
        if dirty {
            if let Some(tab) = app.active_session_mut() {
                tab.messages.push(app::Message::System(
                    "Warning: uncommitted changes in working directory will be overwritten by fork."
                        .into(),
                ));
            }
        }
    }

    // Restore working directory to the selected commit.
    if let Err(e) = concats_core::session_history::restore_workdir_to_commit(
        &app.workspace_root,
        fork_request.commit_oid,
    ) {
        if let Some(tab) = app.active_session_mut() {
            tab.messages.push(app::Message::System(format!(
                "Failed to restore working directory: {e}"
            )));
        }
        return;
    }

    // Prefer the active session's agent config; fall back to the first available agent.
    let (agent_id, agent_config, auto_push, push_remote) =
        if let Some(active) = app.active_session() {
            let cfg = concats_config::AgentConfig {
                name: active.agent_label.clone(),
                command: active.agent_command.clone(),
                args: active.agent_args.clone(),
                env: active.agent_env.clone(),
            };
            (
                active.agent_label.clone(),
                cfg,
                active.auto_push,
                active.push_remote.clone(),
            )
        } else if let Some((id, cfg)) = app.available_agents.first() {
            (
                id.clone(),
                cfg.clone(),
                app.auto_push,
                app.push_remote.clone(),
            )
        } else {
            return;
        };

    // Start a new session forked from the selected commit.
    let session_config = SessionConfig {
        agent_command: agent_config.command.clone(),
        agent_args: agent_config.args.clone(),
        workspace_root: app.workspace_root.clone(),
        env: agent_config.env.clone(),
        fork_from: Some(fork_request.commit_oid),
        auto_push,
        push_remote,
    };

    match start_session(session_config) {
        Ok(new_session) => {
            let label = format!(
                "fork:{}",
                &fork_request.source_session_id[..8.min(fork_request.source_session_id.len())]
            );
            let new_id = app.add_session(new_session, label, &agent_id, &agent_config);

            // Queue fork message on the new tab.
            if let Some(new_tab) = app.session_tabs.iter_mut().find(|t| t.id == new_id) {
                new_tab.queue_fork_message(
                    &fork_request.source_session_id,
                    fork_request.source_turn,
                    fork_request.commit_oid,
                );
            }

            // Switch to the new tab.
            app.switch_tab(ActiveTab::Session(new_id));
        }
        Err(e) => {
            if let Some(tab) = app.active_session_mut() {
                tab.messages
                    .push(app::Message::System(format!("Failed to start fork: {e}")));
            }
        }
    }
}
