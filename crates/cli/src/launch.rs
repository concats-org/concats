use std::process::Command;

use concats_config::AgentConfig;

/// Build a [`Command`] for an agent from its configuration.
#[must_use]
pub fn build_agent_command(agent_config: &AgentConfig) -> Command {
    let mut cmd = Command::new(&agent_config.command);
    cmd.args(&agent_config.args);
    for (key, value) in &agent_config.env {
        cmd.env(key, value);
    }
    cmd
}

/// Replace the current process with the agent command (Unix `execvp`).
///
/// # Errors
///
/// Returns an error if the `exec` syscall fails.
pub fn exec_agent(agent_config: &AgentConfig, extra_args: &[String]) -> miette::Result<()> {
    use std::os::unix::process::CommandExt;

    let mut cmd = build_agent_command(agent_config);
    cmd.args(extra_args);
    let error = cmd.exec();
    Err(miette::miette!("exec failed: {error}"))
}

/// Print the resolved command that would be executed, then exit.
pub fn print_agent_command(agent_config: &AgentConfig, extra_args: &[String]) {
    let mut parts = vec![agent_config.command.clone()];
    parts.extend(agent_config.args.clone());
    parts.extend(extra_args.iter().cloned());

    let formatted: Vec<String> = parts
        .iter()
        .map(|arg| {
            if arg.contains(' ') || arg.contains('"') || arg.contains('\'') || arg.is_empty() {
                format!("'{}'", arg.replace('\'', "'\\''"))
            } else {
                arg.clone()
            }
        })
        .collect();
    println!("{}", formatted.join(" "));
}
