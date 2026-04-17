use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Concats \u{2013} git-native session history for coding agents")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Launch an agent, replacing the current process via exec.
    Run {
        /// Agent to use (name from config or registry).
        agent: Option<String>,

        /// Workspace root directory (defaults to current directory).
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// Print the resolved command instead of executing it.
        #[arg(long)]
        print: bool,

        /// Extra arguments passed through to the agent command.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },

    /// Show the turn history of a session.
    Log {
        /// Session ref (e.g. session-a, session-a~3, abc1234).
        #[arg(name = "ref")]
        session_ref: String,

        /// Show only the last N turns.
        #[arg(short = 'n', long)]
        count: Option<usize>,

        /// Workspace root directory (defaults to current directory).
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Restore the working tree to a session turn's snapshot.
    Checkout {
        /// Session ref (e.g. session-a, session-a~3, abc1234).
        #[arg(name = "ref")]
        session_ref: String,

        /// Force checkout even if the worktree has uncommitted changes.
        #[arg(short, long)]
        force: bool,

        /// Suppress human-readable output.
        #[arg(short, long)]
        quiet: bool,

        /// Workspace root directory (defaults to current directory).
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Browse recorded sessions in a TUI.
    Sessions {
        /// Workspace root directory (defaults to current directory).
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Handle an agent hook event (reads JSON from stdin unless --payload is given).
    Hook {
        /// The agent (claude, codex, cursor, windsurf, gemini, copilot, droid, amp, opencode).
        agent: String,

        /// Hook event name (agent-specific). Optional for single-event agents like Codex.
        event: Option<String>,

        /// JSON payload as CLI argument (used by Codex instead of stdin).
        #[arg(long)]
        payload: Option<String>,
    },

    /// Manage hook integrations for coding agents.
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
}

#[derive(Subcommand)]
pub enum HooksAction {
    /// Install concats hooks for detected agents.
    Install {
        /// Agents to install for (default: auto-detect installed agents).
        agents: Vec<String>,

        /// Project root for Claude project-level hooks (defaults to current directory).
        #[arg(short, long)]
        path: Option<PathBuf>,

        /// Install Claude hooks at user-level (~/.claude/settings.json).
        #[arg(long)]
        global: bool,
    },

    /// Remove concats hooks from agent configurations.
    Uninstall {
        /// Agents to uninstall for (default: all with hooks installed).
        agents: Vec<String>,

        /// Project root for Claude project-level hooks (defaults to current directory).
        #[arg(short, long)]
        path: Option<PathBuf>,

        /// Target user-level Claude hooks (~/.claude/settings.json).
        #[arg(long)]
        global: bool,
    },

    /// Show which agents have concats hooks installed.
    Status {
        /// Project root for Claude project-level hooks (defaults to current directory).
        #[arg(short, long)]
        path: Option<PathBuf>,

        /// Check user-level Claude hooks (~/.claude/settings.json).
        #[arg(long)]
        global: bool,
    },
}
