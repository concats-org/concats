use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Concats – git-native session history for coding agents")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Launch the TUI for interacting with an ACP-compatible coding agent.
    Run {
        /// Agent to use (name from config or ACP registry).
        agent: Option<String>,

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
