use std::{
    io,
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
};

use concats_core::{
    Repository,
    diff::{self, DiffStatus},
    session::{self, Session},
    turn::{self, Turn, TurnEntryKind},
};
use concats_hooks::{InstallScope, all_agents, find_agent};

use crate::cli::{Cli, Commands, HooksAction};

type HookAgent = &'static dyn concats_hooks::Agent;

/// Run the CLI command selected by the parsed arguments, and answer with the
/// code to exit on.
///
/// Most commands are 0-or-error; the review half distinguishes *findings* (1)
/// from *bad input* (2), which is what makes it usable from a hook or CI.
///
/// # Errors
///
/// Returns an error if the selected subcommand fails.
pub fn run(cli: Cli) -> miette::Result<ExitCode> {
    #[cfg(feature = "review")]
    if let Some(Commands::Review(command)) = cli.command {
        return Ok(crate::review::run(command));
    }
    session_command(cli).map(|()| ExitCode::SUCCESS)
}

fn session_command(cli: Cli) -> miette::Result<()> {
    match cli.command {
        Some(Commands::Init { path }) => run_init(path.as_deref()),
        Some(Commands::Hook {
            agent,
            event,
            payload,
        }) => run_hook_command(&agent, event.as_deref(), payload),
        Some(Commands::Hooks { action }) => run_hooks_action(action),
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
        #[cfg(feature = "review")]
        Some(Commands::Review(_)) => unreachable!("handled in run"),
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

    let agent = resolve_agent(agent)?;
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

fn run_init(path: Option<&Path>) -> miette::Result<()> {
    let root = concats_hooks::find_worktree_root(path)
        .map_err(|error| miette::miette!("not inside a git repository: {error}"))?;
    let binary = concats_hooks::helpers::binary_path().map_err(|e| miette::miette!("{e}"))?;

    let detected: Vec<HookAgent> = all_agents()
        .iter()
        .copied()
        .filter(|agent| agent.is_detected())
        .collect();

    if detected.is_empty() {
        return Err(miette::miette!(
            "no coding agents detected. Install an agent (claude, codex, cursor, ...) \
             or use `concats hooks install <agent>` to wire up hooks explicitly."
        ));
    }

    install_hooks(&detected, &InstallScope::Project { root }, &binary);
    Ok(())
}

fn run_hooks_install(agents: &[String], scope: &InstallScope) -> miette::Result<()> {
    let binary = concats_hooks::helpers::binary_path()?;
    let targets = agents
        .iter()
        .map(|name| resolve_agent(name))
        .collect::<miette::Result<Vec<_>>>()?;
    install_hooks(&targets, scope, &binary);
    Ok(())
}

fn install_hooks(targets: &[HookAgent], scope: &InstallScope, binary: &Path) {
    let mut installed = Vec::new();
    for &agent in targets {
        match agent.install(binary, scope) {
            Ok(()) => installed.push(agent.name()),
            Err(error) => eprintln!("warning: failed to install hooks for {agent}: {error}"),
        }
    }

    if installed.is_empty() {
        eprintln!("no agents were installed");
    } else {
        eprintln!("hooks installed for: {}", installed.join(", "));
    }
}

fn run_hooks_uninstall(agents: &[String], scope: &InstallScope) -> miette::Result<()> {
    let targets: Vec<HookAgent> = if agents.is_empty() {
        all_agents().to_vec()
    } else {
        agents
            .iter()
            .map(|name| resolve_agent(name))
            .collect::<miette::Result<Vec<_>>>()?
    };

    let mut removed = Vec::new();
    for agent in targets {
        match agent.uninstall(scope) {
            Ok(()) => removed.push(agent.name()),
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

    for &agent in all_agents() {
        let detected_str = if agent.is_detected() { "yes" } else { "no" };
        let installed_str = if agent.is_installed(scope) {
            "yes"
        } else {
            "no"
        };
        eprintln!("{:<12} {detected_str:<10} {installed_str}", agent.name());
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

fn resolve_agent(name: &str) -> miette::Result<HookAgent> {
    find_agent(name).ok_or_else(|| unknown_agent_error(name))
}

fn unknown_agent_error(name: &str) -> miette::Report {
    let names: Vec<_> = all_agents().iter().map(|agent| agent.name()).collect();
    miette::miette!(
        "unknown agent: {name}. Expected one of: {}",
        names.join(", ")
    )
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
        println!(
            "{:<40} {:>3} turns  {modified}",
            session.id.as_ref(),
            turns.len(),
        );
    }
    // Sessions travel over plain git; concats deliberately ships no push of
    // its own. Said here because refs/agent/* are not in default refspecs, so
    // a bare `git push` never carries them.
    println!(
        "
push a session (and its snapshots) with git:
           git push <remote> 'refs/agent/sessions/<id>' 'refs/agent/snapshots/<id>'"
    );

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

    if let Err(e) = turn::restore(&resolved.session, &resolved.turn, force) {
        if let concats_core::error::Error::RestoreConflict { paths } = &e {
            let listing = paths
                .iter()
                .take(20)
                .map(|p| format!("  {p}"))
                .collect::<Vec<_>>()
                .join("\n");
            let suffix = if paths.len() > 20 {
                format!("\n  ... and {} more", paths.len() - 20)
            } else {
                String::new()
            };
            return Err(miette::miette!(
                "checkout would overwrite local changes in {} file{}:\n\
                 {listing}{suffix}\n\n\
                 use --force to discard these changes",
                paths.len(),
                if paths.len() == 1 { "" } else { "s" },
            ));
        }
        return Err(miette::miette!("{e}"));
    }

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
    let repo = gix::open(&root).map_err(|e| miette::miette!("{e}"))?;
    Ok(Rc::new(repo))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn resolve_agent_uses_registry_case_insensitively() {
        assert_eq!(resolve_agent("ClAuDe").unwrap().name(), "claude");
    }
}
