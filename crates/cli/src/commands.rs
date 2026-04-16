use std::{io, path::PathBuf};

use concats_config::{ConfigCliArgs, load_config, save_config};
use concats_hooks::{Agent, InstallScope};
use concats_registry::{fetch_registry, install_agents};

use crate::{
    app::App,
    cli::{Cli, Commands, HooksAction},
    launch,
};

/// Run the CLI command selected by the parsed arguments.
///
/// # Errors
///
/// Returns an error if the selected subcommand fails.
pub async fn run(cli: Cli) -> miette::Result<()> {
    match cli.command {
        Some(Commands::Hook {
            agent,
            event,
            payload,
        }) => run_hook_command(&agent, event.as_deref(), payload),
        Some(Commands::Hooks { action }) => run_hooks_action(action),
        Some(Commands::Run {
            agent,
            workspace,
            print,
            extra_args,
        }) => run_agent_command(agent, workspace, print, extra_args).await,
        Some(Commands::Sessions { workspace }) => run_sessions_tui(workspace).await,
        None => run_sessions_tui(None).await,
    }
}

/// Read an agent hook payload and dispatch it to the matching handler.
///
/// # Errors
///
/// Returns an error if the payload cannot be read, the agent or event is
/// unknown, or hook dispatch fails.
pub fn run_hook_command(
    agent: &str,
    event: Option<&str>,
    payload: Option<String>,
) -> miette::Result<()> {
    let payload_json = match payload {
        Some(p) => p,
        None => io::read_to_string(io::stdin())
            .map_err(|error| miette::miette!("failed to read stdin: {error}"))?,
    };

    let agent: Agent = agent.parse().map_err(|e: String| miette::miette!("{e}"))?;
    agent
        .dispatch(event, &payload_json)
        .map_err(|error| miette::miette!("{error}"))
}

/// Execute a hook-management subcommand.
///
/// # Errors
///
/// Returns an error if the project root cannot be resolved or the hook
/// settings cannot be installed/removed.
pub fn run_hooks_action(action: HooksAction) -> miette::Result<()> {
    match action {
        HooksAction::Install {
            agents,
            path,
            global,
        } => run_hooks_install(&agents, &resolve_scope(path, global)?),
        HooksAction::Uninstall {
            agents,
            path,
            global,
        } => run_hooks_uninstall(&agents, &resolve_scope(path, global)?),
        HooksAction::Status { path, global } => {
            run_hooks_status(&resolve_scope(path, global)?);
            Ok(())
        }
    }
}

fn run_hooks_install(agents: &[String], scope: &InstallScope) -> miette::Result<()> {
    let binary = concats_hooks::install::binary_path();
    let targets = resolve_agents(agents)?;
    let mut installed = Vec::new();

    for agent in &targets {
        match agent.install(&binary, scope) {
            Ok(()) => installed.push(agent.cli_name()),
            Err(error) => eprintln!("warning: failed to install hooks for {agent}: {error}"),
        }
    }

    if installed.is_empty() {
        eprintln!("no agents were installed");
    } else {
        eprintln!("hooks installed for: {}", installed.join(", "));
    }
    Ok(())
}

fn run_hooks_uninstall(agents: &[String], scope: &InstallScope) -> miette::Result<()> {
    let targets: Vec<Agent> = if agents.is_empty() {
        Agent::ALL.to_vec()
    } else {
        agents
            .iter()
            .map(|s| s.parse::<Agent>().map_err(|e| miette::miette!("{e}")))
            .collect::<miette::Result<Vec<_>>>()?
    };

    let mut removed = Vec::new();
    for agent in &targets {
        match agent.uninstall(scope) {
            Ok(()) => removed.push(agent.cli_name()),
            Err(error) => eprintln!("warning: failed to uninstall hooks for {agent}: {error}"),
        }
    }

    if removed.is_empty() {
        eprintln!("no hooks were removed");
    } else {
        eprintln!("hooks removed for: {}", removed.join(", "));
    }
    Ok(())
}

fn run_hooks_status(scope: &InstallScope) {
    eprintln!("{:<12} {:<10} Installed", "Agent", "Detected");
    eprintln!("{:<12} {:<10} ---------", "-----", "--------");

    for agent in Agent::ALL {
        let detected_str = if agent.is_detected() { "yes" } else { "no" };
        let installed_str = if agent.is_installed(scope) {
            "yes"
        } else {
            "no"
        };
        eprintln!(
            "{:<12} {detected_str:<10} {installed_str}",
            agent.cli_name()
        );
    }
}

fn resolve_scope(path: Option<PathBuf>, global: bool) -> miette::Result<InstallScope> {
    if global {
        return Ok(InstallScope::User);
    }
    let root = path
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|error| miette::miette!("cannot determine cwd: {error}"))?;
    Ok(InstallScope::Project { root })
}

/// Resolve agent names from CLI args, defaulting to auto-detected agents.
fn resolve_agents(args: &[String]) -> miette::Result<Vec<Agent>> {
    if args.is_empty() {
        let detected: Vec<Agent> = Agent::ALL
            .iter()
            .copied()
            .filter(|a| a.is_detected())
            .collect();
        if detected.is_empty() {
            return Err(miette::miette!(
                "no agents detected. Specify agents explicitly: concats hooks install claude codex ..."
            ));
        }
        Ok(detected)
    } else {
        args.iter()
            .map(|s| s.parse::<Agent>().map_err(|e| miette::miette!("{e}")))
            .collect()
    }
}

/// Resolve an agent and either exec into it or print the command.
///
/// # Errors
///
/// Returns an error if configuration cannot be loaded, the agent cannot be
/// resolved, or the exec syscall fails.
async fn run_agent_command(
    agent: Option<String>,
    workspace: Option<PathBuf>,
    print: bool,
    extra_args: Vec<String>,
) -> miette::Result<()> {
    let cli_args = ConfigCliArgs {
        default_agent: agent.clone(),
        workspace,
    };
    let mut config = load_config(&cli_args)?;

    let agent_id = agent
        .or(config.default_agent.clone())
        .ok_or_else(|| miette::miette!("no agent specified. Usage: concats run <agent-name>"))?;

    if resolve_agent_id(&agent_id, &config).is_none() {
        eprintln!("agent '{agent_id}' not found in config, fetching from registry...");
        sync_registry(&mut config).await?;
    }

    let resolved_id = resolve_agent_id(&agent_id, &config).ok_or_else(|| {
        let mut available: Vec<_> = config.agents.keys().cloned().collect();
        available.sort();
        miette::miette!(
            "agent '{agent_id}' not found in the registry.\n\
             Available agents: {}",
            available.join(", ")
        )
    })?;

    let agent_config = &config.agents[&resolved_id];

    if print {
        launch::print_agent_command(agent_config, &extra_args);
        Ok(())
    } else {
        launch::exec_agent(agent_config, &extra_args)
    }
}

/// Launch the TUI for browsing recorded sessions.
///
/// # Errors
///
/// Returns an error if the workspace directory cannot be resolved or the TUI
/// exits with an error.
async fn run_sessions_tui(workspace: Option<PathBuf>) -> miette::Result<()> {
    let workspace_root = workspace
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|error| miette::miette!("could not determine current directory: {error}"))?;

    let mut app = App::new(workspace_root);
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
