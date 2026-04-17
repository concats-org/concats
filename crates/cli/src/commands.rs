use std::{io, path::PathBuf, rc::Rc};

use concats_config::{ConfigCliArgs, load_config};
use concats_core::{
    Repository,
    diff::{self, DiffStatus},
    session::{self, Session},
    turn::{self, Turn, TurnEntryKind},
};
use concats_hooks::{Agent, InstallScope};

use crate::{
    cli::{Cli, Commands, HooksAction},
    launch,
};

/// Run the CLI command selected by the parsed arguments.
///
/// # Errors
///
/// Returns an error if the selected subcommand fails.
pub fn run(cli: Cli) -> miette::Result<()> {
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
        }) => run_agent_command(agent, workspace, print, &extra_args),
        Some(Commands::Log {
            session_ref,
            count,
            workspace,
        }) => run_log(&session_ref, count, workspace),
        Some(Commands::Checkout {
            session_ref,
            force,
            quiet,
            workspace,
        }) => run_checkout(&session_ref, force, quiet, workspace),
        Some(Commands::Sessions { workspace }) => run_sessions_list(workspace),
        None => {
            use clap::CommandFactory;
            Cli::command()
                .print_help()
                .map_err(|e| miette::miette!("{e}"))
        }
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
fn run_agent_command(
    agent: Option<String>,
    workspace: Option<PathBuf>,
    print: bool,
    extra_args: &[String],
) -> miette::Result<()> {
    let cli_args = ConfigCliArgs {
        default_agent: agent.clone(),
        workspace,
    };
    let config = load_config(&cli_args)?;

    let agent_id = agent
        .or(config.default_agent.clone())
        .ok_or_else(|| miette::miette!("no agent specified. Usage: concats run <agent-name>"))?;

    let resolved_id = resolve_agent_id(&agent_id, &config).ok_or_else(|| {
        let mut available: Vec<_> = config.agents.keys().cloned().collect();
        available.sort();
        miette::miette!(
            "agent '{agent_id}' not found.\n\
             Available agents: {}",
            available.join(", ")
        )
    })?;

    let agent_config = &config.agents[&resolved_id];

    if print {
        launch::print_agent_command(agent_config, extra_args);
        Ok(())
    } else {
        launch::exec_agent(agent_config, extra_args)
    }
}

fn run_sessions_list(workspace: Option<PathBuf>) -> miette::Result<()> {
    let repo = open_repo(workspace)?;
    let sessions = session::list(&repo).map_err(|e| miette::miette!("{e}"))?;

    if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
    }

    for session in &sessions {
        let turns = turn::list(session).map_err(|e| miette::miette!("{e}"))?;
        let modified = session::modified_at(session)
            .ok()
            .map(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_else(|_| t.unix_timestamp().to_string())
            })
            .unwrap_or_default();
        let name = session
            .name
            .as_deref()
            .unwrap_or_else(|| session.id.as_ref());
        println!("{:<40} {:>3} turns  {modified}", name, turns.len(),);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// concats log
// ---------------------------------------------------------------------------

fn run_log(
    session_ref: &str,
    count: Option<usize>,
    workspace: Option<PathBuf>,
) -> miette::Result<()> {
    let repo = open_repo(workspace)?;
    let resolved = resolve_session_ref(&repo, session_ref)?;
    let turns = turn::list(&resolved.session).map_err(|e| miette::miette!("{e}"))?;

    // Slice: up to the resolved turn (inclusive), then optionally tail by -n.
    let end = turns.len() - resolved.offset_from_tip;
    let visible = &turns[..end];
    let visible = match count {
        Some(n) => &visible[visible.len().saturating_sub(n)..],
        None => visible,
    };

    println!("session {} ({} turns)\n", resolved.session.id, turns.len());

    for turn in visible {
        print_turn(&resolved.session, turn);
    }

    Ok(())
}

fn print_turn(session: &Session, turn: &Turn) {
    let ts = turn
        .created_at
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| turn.created_at.unix_timestamp().to_string());
    println!("turn {} {ts}", turn.oid.short());

    for entry in turn.entries() {
        match &entry.kind {
            TurnEntryKind::Prompt { text } => {
                println!("  > {text}");
            }
            TurnEntryKind::Response { text } => {
                for line in text.lines() {
                    println!("  {line}");
                }
            }
            TurnEntryKind::ToolCall { kind } => {
                println!("  tool {kind}");
            }
        }
    }

    if let Ok(diffs) = diff::for_turn(session, turn)
        && !diffs.is_empty()
    {
        println!();
        for file in &diffs {
            let icon = match &file.status {
                DiffStatus::Added => "A",
                DiffStatus::Modified => "M",
                DiffStatus::Deleted => "D",
                DiffStatus::Renamed { .. } => "R",
            };
            let path = match &file.status {
                DiffStatus::Renamed { old_path } => format!("{old_path} -> {}", file.path),
                _ => file.path.clone(),
            };
            println!("  {icon} {path}");
        }
    }

    println!();
}

// ---------------------------------------------------------------------------
// concats checkout
// ---------------------------------------------------------------------------

fn run_checkout(
    session_ref: &str,
    force: bool,
    quiet: bool,
    workspace: Option<PathBuf>,
) -> miette::Result<()> {
    let repo = open_repo(workspace)?;
    let resolved = resolve_session_ref(&repo, session_ref)?;

    if !force {
        let statuses = repo
            .statuses(None)
            .map_err(|e| miette::miette!("failed to read worktree status: {e}"))?;
        let dirty: Vec<_> = statuses
            .iter()
            .filter(|entry| {
                !entry
                    .status()
                    .intersects(git2::Status::IGNORED | git2::Status::CURRENT)
            })
            .filter_map(|entry| entry.path().map(String::from))
            .collect();
        if !dirty.is_empty() {
            let listing = dirty
                .iter()
                .take(10)
                .map(|p| format!("  {p}"))
                .collect::<Vec<_>>()
                .join("\n");
            let suffix = if dirty.len() > 10 {
                format!("\n  ... and {} more", dirty.len() - 10)
            } else {
                String::new()
            };
            return Err(miette::miette!(
                "worktree has uncommitted changes:\n{listing}{suffix}\n\nuse --force to override"
            ));
        }
    }

    turn::restore(&resolved.session, &resolved.turn).map_err(|e| miette::miette!("{e}"))?;

    if !quiet {
        let ref_display = if resolved.offset_from_tip > 0 {
            format!("{}~{}", resolved.session.id, resolved.offset_from_tip)
        } else {
            resolved.session.id.to_string()
        };
        println!(
            "Checked out turn {} (from session {}).\n\n\
             Continue from this point. To see the full conversation up to here:\n  \
             concats log {ref_display}\n\n\
             The working tree has been restored to the state at this turn.",
            resolved.turn.oid.short(),
            resolved.session.id,
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// session ref resolution
// ---------------------------------------------------------------------------

struct ResolvedRef {
    session: Session,
    turn: Turn,
    offset_from_tip: usize,
}

fn resolve_session_ref(repo: &Rc<Repository>, input: &str) -> miette::Result<ResolvedRef> {
    let (name, offset) = parse_tilde_suffix(input);

    // Try session-name resolution first.
    if let Ok(session) = session::open(Rc::clone(repo), name) {
        let turns = turn::list(&session).map_err(|e| miette::miette!("{e}"))?;
        if turns.is_empty() {
            return Err(miette::miette!("session '{name}' has no turns"));
        }
        let index = turns.len().checked_sub(1 + offset).ok_or_else(|| {
            miette::miette!(
                "offset ~{offset} out of range; session '{name}' has {} turns",
                turns.len()
            )
        })?;
        let turn = turns[index].clone();
        return Ok(ResolvedRef {
            session,
            turn,
            offset_from_tip: offset,
        });
    }

    // Bare SHA prefix — search across all sessions.
    if offset > 0 {
        return Err(miette::miette!(
            "session '{name}' not found (tilde suffix only works with session names)"
        ));
    }

    let sessions = session::list(repo).map_err(|e| miette::miette!("{e}"))?;
    for session in &sessions {
        let Ok(turns) = turn::list(session) else {
            continue;
        };
        for (i, turn) in turns.iter().enumerate() {
            if turn.oid.to_string().starts_with(input) {
                return Ok(ResolvedRef {
                    session: session.clone(),
                    turn: turn.clone(),
                    offset_from_tip: turns.len() - 1 - i,
                });
            }
        }
    }

    Err(miette::miette!("no session or turn matching '{input}'"))
}

/// Parse "session-a~3" → ("session-a", 3), "session-a~2~1" → ("session-a", 3).
fn parse_tilde_suffix(input: &str) -> (&str, usize) {
    let mut total_offset = 0usize;
    let mut name = input;
    while let Some(pos) = name.rfind('~') {
        let suffix = &name[pos + 1..];
        let n: usize = suffix.parse().unwrap_or(1);
        total_offset += n;
        name = &name[..pos];
    }
    (name, total_offset)
}

fn open_repo(workspace: Option<PathBuf>) -> miette::Result<Rc<Repository>> {
    let root = workspace
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|e| miette::miette!("cannot determine cwd: {e}"))?;
    let repo = Repository::open(&root).map_err(|e| miette::miette!("{e}"))?;
    Ok(Rc::new(repo))
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
