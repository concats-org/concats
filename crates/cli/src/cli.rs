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

    /// Handle a Claude Code hook event (reads JSON from stdin).
    Hook {
        /// The hook event name (`SessionStart`, `UserPromptSubmit`,
        /// `PostToolUse`, `Stop`).
        event: String,
    },

    /// Manage Claude Code hook integration.
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
}

#[derive(Subcommand)]
pub enum HooksAction {
    /// Install concats hooks into .claude/settings.json.
    Install {
        /// Project root directory (defaults to current directory).
        #[arg(short, long)]
        path: Option<PathBuf>,
    },
}
