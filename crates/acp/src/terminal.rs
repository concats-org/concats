use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

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
    #[must_use]
    pub fn new() -> Self {
        Self {
            terminals: RefCell::new(HashMap::new()),
            next_id: Cell::new(1),
        }
    }

    /// Create a new terminal running the given command.
    ///
    /// # Errors
    ///
    /// Returns an error if the command cannot be spawned.
    pub fn create(&self, command: &str, args: &[String], cwd: Option<&str>) -> Result<String> {
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

        if let Some(stdout) = child.stdout.take() {
            spawn_output_drain(Rc::clone(&output), stdout);
        }

        if let Some(stderr) = child.stderr.take() {
            spawn_output_drain(Rc::clone(&output), stderr);
        }

        self.terminals
            .borrow_mut()
            .insert(terminal_id.clone(), TerminalState { child, output });

        Ok(terminal_id)
    }

    /// Read available output from a terminal.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal ID is unknown.
    pub fn read_all_output(&self, terminal_id: &str) -> Result<String> {
        let terminals = self.terminals.borrow();
        let state = terminals
            .get(terminal_id)
            .ok_or_else(|| Error::terminal(format!("unknown terminal: {terminal_id}")))?;
        Ok(state.output.borrow().clone())
    }

    /// Kill a terminal's process.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal ID is unknown or the process cannot be
    /// signaled.
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
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal ID is unknown or waiting on the
    /// process fails.
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
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal ID is unknown.
    pub fn release(&self, terminal_id: &str) -> Result<()> {
        self.terminals
            .borrow_mut()
            .remove(terminal_id)
            .ok_or_else(|| Error::terminal(format!("unknown terminal: {terminal_id}")))?;
        Ok(())
    }
}

fn spawn_output_drain<R>(output: Rc<RefCell<String>>, mut reader: R)
where
    R: AsyncRead + Unpin + 'static,
{
    tokio::task::spawn_local(async move {
        let mut buf = vec![0_u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buf[..n]);
                    output.borrow_mut().push_str(&text);
                }
            }
        }
    });
}
