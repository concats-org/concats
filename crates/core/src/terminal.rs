use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::error::{Error, Result};

struct TerminalState {
    child: tokio::process::Child,
    output: Rc<RefCell<String>>,
}

/// Manages terminal (child process) instances within a single-threaded context.
///
/// Uses `RefCell` because this lives in a `!Send` `LocalSet` context.
pub struct TerminalManager {
    terminals: RefCell<HashMap<String, TerminalState>>,
    next_id: Cell<u64>,
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalManager {
    pub fn new() -> Self {
        Self {
            terminals: RefCell::new(HashMap::new()),
            next_id: Cell::new(1),
        }
    }

    /// Create a new terminal running the given command.
    pub async fn create(
        &self,
        command: &str,
        args: &[String],
        cwd: Option<&str>,
    ) -> Result<String> {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let terminal_id = format!("term-{id}");

        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::terminal(format!("failed to spawn {command}: {e}")))?;

        let output = Rc::new(RefCell::new(String::new()));

        // Spawn a local task to continuously read stdout into the shared buffer.
        if let Some(mut stdout) = child.stdout.take() {
            let output_clone = Rc::clone(&output);
            tokio::task::spawn_local(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    match stdout.read(&mut buf).await {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            let text = String::from_utf8_lossy(&buf[..n]);
                            output_clone.borrow_mut().push_str(&text);
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        // Also drain stderr into the same buffer.
        if let Some(mut stderr) = child.stderr.take() {
            let output_clone = Rc::clone(&output);
            tokio::task::spawn_local(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    match stderr.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let text = String::from_utf8_lossy(&buf[..n]);
                            output_clone.borrow_mut().push_str(&text);
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        self.terminals
            .borrow_mut()
            .insert(terminal_id.clone(), TerminalState { child, output });

        Ok(terminal_id)
    }

    /// Read available output from a terminal.
    pub async fn read_output(&self, terminal_id: &str) -> Result<String> {
        let terminals = self.terminals.borrow();
        let state = terminals
            .get(terminal_id)
            .ok_or_else(|| Error::terminal(format!("unknown terminal: {terminal_id}")))?;
        Ok(state.output.borrow().clone())
    }

    /// Kill a terminal's process.
    pub fn kill(&self, terminal_id: &str) -> Result<()> {
        let mut terminals = self.terminals.borrow_mut();
        let state = terminals
            .get_mut(terminal_id)
            .ok_or_else(|| Error::terminal(format!("unknown terminal: {terminal_id}")))?;
        state
            .child
            .start_kill()
            .map_err(|e| Error::terminal(format!("failed to kill {terminal_id}: {e}")))?;
        Ok(())
    }

    /// Wait for a terminal process to exit and return its exit code.
    pub async fn wait_for_exit(&self, terminal_id: &str) -> Result<i32> {
        // Take the child out so we can await it without holding the borrow.
        let mut child = self
            .terminals
            .borrow_mut()
            .remove(terminal_id)
            .ok_or_else(|| Error::terminal(format!("unknown terminal: {terminal_id}")))?
            .child;

        let status = child
            .wait()
            .await
            .map_err(|e| Error::terminal(format!("failed to wait for {terminal_id}: {e}")))?;

        Ok(status.code().unwrap_or(-1))
    }

    /// Release (remove) a terminal.
    pub fn release(&self, terminal_id: &str) -> Result<()> {
        self.terminals
            .borrow_mut()
            .remove(terminal_id)
            .ok_or_else(|| Error::terminal(format!("unknown terminal: {terminal_id}")))?;
        Ok(())
    }
}
