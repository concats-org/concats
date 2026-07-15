use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "concats",
    about = "Concats \u{2013} git-native session history for coding agents"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Set up concats in the current repository: install hooks for every
    /// detected agent at the project level.
    Init {
        /// Project root (defaults to the enclosing git worktree).
        #[arg(short, long)]
        path: Option<PathBuf>,
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

    /// List recorded sessions.
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

    /// The review commands — `manifest`, `lint`, `submit`, `comments`, and the
    /// rest. Flattened, so they read as `concats comments add`, and present
    /// only in the build that ships beside the app.
    #[cfg(feature = "review")]
    #[command(flatten)]
    Review(crate::review::ReviewCommands),
}

#[derive(Subcommand)]
pub enum HooksAction {
    /// Install concats hooks for one or more named agents.
    ///
    /// To install for every detected agent at once, use `concats init`.
    Install {
        /// Agents to install for.
        #[arg(required = true)]
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
