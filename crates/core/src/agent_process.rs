use std::{collections::HashMap, path::Path, process::Stdio};

use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use crate::error::{Error, Result};

/// A spawned agent subprocess with piped stdin/stdout for ACP communication.
pub struct AgentProcess {
    child: Child,
}

/// The stdio streams taken from the agent process for ACP communication.
pub struct AgentStreams {
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

impl AgentProcess {
    /// Spawn an agent subprocess.
    ///
    /// stdin and stdout are piped for ACP protocol communication.
    /// stderr is inherited so agent diagnostic output goes to the parent terminal.
    pub fn spawn(
        command: &str,
        args: &[String],
        cwd: &Path,
        env: &HashMap<String, String>,
    ) -> Result<Self> {
        let child = Command::new(command)
            .args(args)
            .envs(env)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::process(format!("failed to spawn agent '{command}': {e}")))?;

        Ok(Self { child })
    }

    /// Take ownership of the stdio streams for ACP communication.
    ///
    /// Can only be called once; subsequent calls return an error.
    pub fn take_streams(&mut self) -> Result<AgentStreams> {
        let stdin = self
            .child
            .stdin
            .take()
            .ok_or_else(|| Error::process("agent stdin already taken"))?;
        let stdout = self
            .child
            .stdout
            .take()
            .ok_or_else(|| Error::process("agent stdout already taken"))?;
        let stderr = self
            .child
            .stderr
            .take()
            .ok_or_else(|| Error::process("agent stderr already taken"))?;
        Ok(AgentStreams {
            stdin,
            stdout,
            stderr,
        })
    }

    /// Wait for the agent process to exit.
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        self.child
            .wait()
            .await
            .map_err(|e| Error::process(format!("failed to wait for agent: {e}")))
    }

    /// Kill the agent process.
    pub async fn kill(&mut self) -> Result<()> {
        self.child
            .kill()
            .await
            .map_err(|e| Error::process(format!("failed to kill agent: {e}")))
    }
}
