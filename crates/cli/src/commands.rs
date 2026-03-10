use std::{io, path::PathBuf};

use concats_acp::start_session;
use concats_config::{ConfigCliArgs, load_config, save_config};
use concats_registry::{fetch_registry, install_agents};

use crate::{
    app::App,
    cli::{Cli, Commands, HooksAction},
    launch::SessionLaunchSpec,
};

/// Run the CLI command selected by the parsed arguments.
///
/// # Errors
///
/// Returns an error if the selected subcommand fails.
pub async fn run(cli: Cli) -> miette::Result<()> {
    match cli.command {
        Some(Commands::Hook { event }) => run_hook_command(&event),
        Some(Commands::Hooks { action }) => run_hooks_action(action),
        Some(Commands::Run { agent, workspace }) => run_tui_command(agent, workspace).await,
        None => run_tui_command(None, None).await,
    }
}

/// Read a Claude hook payload from stdin and dispatch it.
///
/// # Errors
///
/// Returns an error if stdin cannot be read, the hook event name is unknown, or
/// hook dispatch fails.
pub fn run_hook_command(event: &str) -> miette::Result<()> {
    let stdin = io::read_to_string(io::stdin())
        .map_err(|error| miette::miette!("failed to read stdin: {error}"))?;

    match event {
        "SessionStart" | "UserPromptSubmit" | "PostToolUse" | "Stop" => {
            concats_hooks::claude::dispatch(event, &stdin)
                .map_err(|error| miette::miette!("{error}"))
        }
        _ => Err(miette::miette!(
            "unknown hook event: {event}. Expected one of: SessionStart, UserPromptSubmit, PostToolUse, Stop"
        )),
    }
}

/// Execute a hook-management subcommand.
///
/// # Errors
///
/// Returns an error if the project root cannot be resolved or the hook
/// settings cannot be installed.
pub fn run_hooks_action(action: HooksAction) -> miette::Result<()> {
    match action {
        HooksAction::Install { path } => {
            let project_root = path
                .map_or_else(std::env::current_dir, Ok)
                .map_err(|error| miette::miette!("cannot determine cwd: {error}"))?;
            let binary_name = std::env::current_exe()
                .ok()
                .and_then(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "concats".into());
            concats_hooks::claude::install(&project_root, &binary_name)
                .map_err(|error| miette::miette!("{error}"))?;
            eprintln!(
                "hooks installed in {}",
                project_root.join(".claude/settings.json").display()
            );
            Ok(())
        }
    }
}

/// Start the TUI with the selected agent and workspace.
///
/// # Errors
///
/// Returns an error if configuration cannot be loaded, the agent cannot be
/// resolved, the registry sync fails, the initial session cannot be started,
/// or the TUI exits with an error.
pub async fn run_tui_command(
    agent: Option<String>,
    workspace: Option<PathBuf>,
) -> miette::Result<()> {
    let cli_args = ConfigCliArgs {
        default_agent: agent.clone(),
        workspace: workspace.clone(),
    };
    let mut config = load_config(&cli_args)?;

    let agent_id = agent
        .or(config.default_agent.clone())
        .ok_or_else(|| miette::miette!("no agent specified. Usage: concats run <agent-name>"))?;

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

    let workspace_root = config
        .workspace
        .clone()
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|error| miette::miette!("could not determine current directory: {error}"))?;

    let mut available_agents: Vec<(String, concats_config::AgentConfig)> = config
        .agents
        .iter()
        .map(|(id, cfg)| (id.clone(), cfg.clone()))
        .collect();
    available_agents.sort_by(|left, right| left.0.cmp(&right.0));

    let launch = SessionLaunchSpec::new(
        workspace_root.clone(),
        &resolved_id,
        &config.agents[&resolved_id],
        config.sync.auto_push,
        &config.sync.remote,
        None,
        None,
    );
    let SessionLaunchSpec {
        label,
        session_config,
        tab_config,
    } = launch;
    let session = start_session(session_config).map_err(|error| miette::miette!("{error}"))?;

    let mut app = App::new(workspace_root, available_agents);
    app.auto_push = config.sync.auto_push;
    app.push_remote = config.sync.remote.clone();

    let initial_id = app.add_session(session, &label, tab_config);
    app.set_active_tab(crate::tabs::ActiveTab::Session(initial_id));

    app.run().await
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
