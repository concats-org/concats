use std::io;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use cli::app::{App};
use cli::tabs::{ActiveTab};
use cli::event::{Event, EventHandler};
use cli::tui::Tui;
use cli::handler;

use concats_config::{ConfigCliArgs, load_config, save_config};
use concats_core::session::{SessionConfig, start_session};
use concats_registry::{fetch_registry, install_agents};

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

#[tokio::main]
async fn main() -> miette::Result<()> {
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
        Some(Commands::Run { agent, workspace }) => run_tui_command(agent, workspace).await,
        // Default to `run` when no subcommand is given (backwards compat).
        None => run_tui_command(None, None).await,
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

// -- TUI command ─────────────────────────────────────────────────────

async fn run_tui_command(agent: Option<String>, workspace: Option<PathBuf>) -> miette::Result<()> {
    let cli_args = ConfigCliArgs {
        default_agent: agent.clone(),
        workspace: workspace.clone(),
    };

    let mut config = load_config(&cli_args)?;

    // Resolve which agent to use.
    let agent_id = agent
        .or(config.default_agent.clone())
        .ok_or_else(|| miette::miette!("no agent specified. Usage: concats run <agent-name>"))?;

    // Look up the agent in config; if missing, try to install from registry.
    if resolve_agent_id(&agent_id, &config).is_none() {
        eprintln!("agent '{agent_id}' not found in config, fetching from ACP registry...");
        sync_registry(&mut config).await?;
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

    // Start the initial session.
    let session = start_session(session_config).map_err(|e| miette::miette!("{e}"))?;

    // Initialize the terminal user interface.
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend).map_err(|e| miette::miette!("failed to create terminal: {e}"))?;
    let events = EventHandler::new(std::time::Duration::from_millis(80));
    let mut tui = Tui::new(terminal, events);
    tui.init()?;

    // Initialize the app.
    let mut app = App::new(workspace_root, available_agents);
    app.auto_push = auto_push;
    app.push_remote = push_remote;

    // Add the initial session as the first tab.
    let initial_label = if agent_config.name.trim().is_empty() {
        resolved_id.clone()
    } else {
        agent_config.name.clone()
    };
    let initial_id = app.add_session(
        session,
        initial_label,
        &resolved_id,
        &agent_config,
    );
    app.switch_tab(ActiveTab::Session(initial_id));

    // Start the event loop.
    while !app.should_quit {
        tui.terminal.draw(|f| cli::ui::render(f, &mut app))
            .map_err(|e| miette::miette!("draw error: {e}"))?;

        tokio::select! {
            maybe_event = tui.events.next() => {
                match maybe_event {
                    Some(Event::Tick) => app.tick(),
                    Some(Event::Key(key_event)) => handler::handle_key_events(key_event, &mut app).await?,
                    Some(Event::Mouse(mouse_event)) => handler::handle_mouse_events(mouse_event, &mut app, tui.terminal.size().unwrap_or_default()).await?,
                    Some(Event::Resize(_x, _y)) => {
                        // Ratatui handles resize automatically during draw.
                    }
                    None => {}
                }
            }
            session_event = app.session_event_rx.recv() => {
                if let Some((tab_id, fan_in_event)) = session_event {
                   app.handle_fan_in_event(tab_id, fan_in_event);
                }
            }
        }
    }

    // Exit the terminal user interface.
    tui.exit()?;

    Ok(())
}

fn resolve_agent_id(input: &str, config: &concats_config::Config) -> Option<String> {
    if config.agents.contains_key(input) {
        return Some(input.to_string());
    }
    let input_lower = input.to_lowercase();
    let prefix_matches: Vec<_> = config
        .agents
        .keys()
        .filter(|id| id.to_lowercase().starts_with(&input_lower))
        .collect();
    if prefix_matches.len() == 1 {
        return Some(prefix_matches[0].clone());
    }
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

async fn sync_registry(config: &mut concats_config::model::Config) -> miette::Result<()> {
    let registry = fetch_registry().await?;
    install_agents(&registry, config);
    save_config(config)?;
    Ok(())
}
