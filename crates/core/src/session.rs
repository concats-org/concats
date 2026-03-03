use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc};

use agent_client_protocol::{
    Agent, ClientCapabilities, ClientSideConnection, ContentBlock, FileSystemCapability,
    InitializeRequest, NewSessionRequest, PromptRequest, ProtocolVersion, SessionNotification,
    SessionUpdate, StopReason,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc,
    task::LocalSet,
};

use crate::{
    agent_process::AgentProcess,
    checkpoint::CheckpointStore,
    client::ClientHandler,
    error::{Error, Result},
    fs::FileSystem,
    git::Oid,
    notification::NotificationSender,
    permission::PermissionHandler,
    terminal::TerminalManager,
};

/// Configuration for starting a new agent session.
pub struct SessionConfig {
    pub agent_command: String,
    pub agent_args: Vec<String>,
    pub workspace_root: PathBuf,
    /// Extra environment variables to set for the agent process.
    #[allow(clippy::zero_sized_map_values)]
    pub env: HashMap<String, String>,
    /// When set, the new session forks from this commit OID instead of HEAD.
    pub fork_from: Option<git2::Oid>,
    /// When true, automatically push the session ref to the remote after each checkpoint.
    pub auto_push: bool,
    /// Git remote name to push to (e.g. "origin").
    pub push_remote: String,
}

/// Events emitted by a running session.
pub enum SessionEvent {
    /// Initial session config metadata from `session/new` response.
    SessionConfigured {
        mode: Option<String>,
        config_options: Vec<agent_client_protocol::SessionConfigOption>,
    },
    /// Agent streamed a notification (content chunk, tool call, etc.).
    Notification(Box<SessionNotification>),
    /// A prompt turn completed.
    TurnComplete {
        stop_reason: StopReason,
        commit_oid: Option<Oid>,
    },
    /// A line of stderr output from the agent process.
    Stderr(String),
    /// A push to the remote failed (non-fatal).
    PushFailed { ref_name: String, error: String },
    /// An error occurred.
    Error(Error),
}

/// Send-safe handle for interacting with a running session from the TUI thread.
pub struct SessionHandle {
    pub prompt_tx: mpsc::Sender<String>,
    pub event_rx: mpsc::UnboundedReceiver<SessionEvent>,
    pub cancel_tx: mpsc::Sender<()>,
}

/// Start a new agent session on a dedicated thread.
///
/// Returns a `SessionHandle` that can send prompts and receive events.
/// Each session gets its own thread with a single-threaded tokio runtime + `LocalSet`
/// to satisfy the `!Send` requirements of ACP's `ClientSideConnection`.
pub fn start_session(config: SessionConfig) -> Result<SessionHandle> {
    let (prompt_tx, prompt_rx) = mpsc::channel::<String>(16);
    let (event_tx, event_rx) = mpsc::unbounded_channel::<SessionEvent>();
    let (cancel_tx, cancel_rx) = mpsc::channel::<()>(1);

    let handle = SessionHandle {
        prompt_tx,
        event_rx,
        cancel_tx,
    };

    std::thread::Builder::new()
        .name("session".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build session runtime");

            let local = LocalSet::new();
            local.block_on(&rt, session_loop(config, prompt_rx, event_tx, cancel_rx));
        })
        .map_err(|e| Error::session(format!("failed to spawn session thread: {e}")))?;

    Ok(handle)
}

async fn session_loop(
    config: SessionConfig,
    mut prompt_rx: mpsc::Receiver<String>,
    event_tx: mpsc::UnboundedSender<SessionEvent>,
    mut cancel_rx: mpsc::Receiver<()>,
) {
    if let Err(e) = session_loop_inner(&config, &mut prompt_rx, &event_tx, &mut cancel_rx).await {
        let _ = event_tx.send(SessionEvent::Error(e));
    }
}

async fn session_loop_inner(
    config: &SessionConfig,
    prompt_rx: &mut mpsc::Receiver<String>,
    event_tx: &mpsc::UnboundedSender<SessionEvent>,
    _cancel_rx: &mut mpsc::Receiver<()>,
) -> Result<()> {
    // 1. Spawn agent process.
    let mut agent = AgentProcess::spawn(
        &config.agent_command,
        &config.agent_args,
        &config.workspace_root,
        &config.env,
    )?;
    let streams = agent.take_streams()?;

    // 2. Prepare checkpoint holder (populated after ACP session is created).
    let checkpoint: Rc<RefCell<Option<CheckpointStore>>> = Rc::new(RefCell::new(None));

    // 3. Build the ClientHandler (shares the checkpoint store via Rc<RefCell>).
    let (notification_tx, mut notification_rx) = mpsc::unbounded_channel::<SessionNotification>();
    let handler = ClientHandler::new(
        FileSystem::new(config.workspace_root.clone()),
        TerminalManager::new(),
        PermissionHandler::new(),
        NotificationSender::new(notification_tx),
        Rc::clone(&checkpoint),
    );

    // 4. Create the ACP connection.
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    let stdin_compat = streams.stdin.compat_write();
    let stdout_compat = streams.stdout.compat();

    let (conn, io_future) =
        ClientSideConnection::new(handler, stdin_compat, stdout_compat, |fut| {
            tokio::task::spawn_local(fut);
        });

    // 5. Spawn the IO driver as a local task.
    tokio::task::spawn_local(async move {
        if let Err(e) = io_future.await {
            tracing::error!("ACP IO error: {e}");
        }
    });

    // 5b. Forward agent stderr lines to the event channel.
    let stderr_event_tx = event_tx.clone();
    tokio::task::spawn_local(async move {
        let mut reader = BufReader::new(streams.stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if stderr_event_tx.send(SessionEvent::Stderr(line)).is_err() {
                break;
            }
        }
    });

    // 6. Forward notifications from the handler to the event channel,
    //    accumulating agent response text for checkpoint messages.
    let response_text: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let response_text_clone = Rc::clone(&response_text);
    let event_tx_clone = event_tx.clone();
    tokio::task::spawn_local(async move {
        while let Some(notification) = notification_rx.recv().await {
            // Accumulate agent text chunks for the checkpoint summary.
            if let SessionUpdate::AgentMessageChunk(ref chunk) = notification.update
                && let ContentBlock::Text(ref t) = chunk.content
            {
                response_text_clone.borrow_mut().push_str(&t.text);
            }
            let _ = event_tx_clone.send(SessionEvent::Notification(Box::new(notification)));
        }
    });

    // 7. Initialize the ACP connection.
    let _init_response = conn
        .initialize(
            InitializeRequest::new(ProtocolVersion::LATEST).client_capabilities(
                ClientCapabilities::new()
                    .fs(FileSystemCapability::new()
                        .read_text_file(true)
                        .write_text_file(true))
                    .terminal(true),
            ),
        )
        .await
        .map_err(|e| Error::protocol(format!("initialize failed: {e}")))?;

    // 8. Create a new ACP session and use its ID for checkpoints.
    let session_response = conn
        .new_session(NewSessionRequest::new(config.workspace_root.clone()))
        .await
        .map_err(|e| Error::protocol(format!("new_session failed: {e}")))?;
    let acp_session_id = session_response.session_id;
    let initial_mode = session_response
        .modes
        .as_ref()
        .map(|modes| modes.current_mode_id.to_string());
    let initial_config_options = session_response.config_options.unwrap_or_default();
    let _ = event_tx.send(SessionEvent::SessionConfigured {
        mode: initial_mode,
        config_options: initial_config_options,
    });

    // 9. Create the checkpoint store using the ACP session ID.
    {
        let checkpoint_store = if let Some(fork_oid) = config.fork_from {
            CheckpointStore::new_forked(
                config.workspace_root.clone(),
                acp_session_id.to_string(),
                fork_oid,
            )
        } else {
            CheckpointStore::new(config.workspace_root.clone(), acp_session_id.to_string())
        };
        *checkpoint.borrow_mut() = Some(checkpoint_store);
    }

    // 10. Prompt loop: receive prompts, send to agent, checkpoint on completion.
    while let Some(prompt_text) = prompt_rx.recv().await {
        // Clear the response accumulator for this turn.
        response_text.borrow_mut().clear();

        // Create initial checkpoint before sending the prompt.
        if let Some(store) = checkpoint.borrow().as_ref()
            && let Err(e) = store.create_checkpoint(&prompt_text)
        {
            tracing::warn!("checkpoint create failed: {e}");
        }

        let prompt_blocks = vec![ContentBlock::from(prompt_text.clone())];
        let request = PromptRequest::new(acp_session_id.clone(), prompt_blocks);

        match conn.prompt(request).await {
            Ok(response) => {
                // Finalize checkpoint with the accumulated response text.
                let stop_reason_str = format!("{:?}", response.stop_reason);
                let summary = response_text.borrow().clone();
                let (commit_oid, finalized_ref_name) =
                    match checkpoint.borrow_mut().as_mut().map(|store| {
                        let ref_name = store.ref_name().to_owned();
                        store
                            .finalize_checkpoint(&prompt_text, &summary, &stop_reason_str)
                            .map(|oid| (oid, ref_name))
                    }) {
                        Some(Ok((oid, ref_name))) => (Some(oid), Some(ref_name)),
                        Some(Err(e)) => {
                            tracing::warn!("checkpoint finalize failed: {e}");
                            (None, None)
                        }
                        None => (None, None),
                    };

                // Auto-push the session ref in a background thread if enabled.
                if config.auto_push {
                    if let Some(ref_name) = finalized_ref_name {
                        let repo_path = config.workspace_root.clone();
                        let remote = config.push_remote.clone();
                        let push_event_tx = event_tx.clone();
                        std::thread::Builder::new()
                            .name("push-ref".into())
                            .spawn(move || {
                                if let Err(e) = crate::git::push_ref(&repo_path, &remote, &ref_name)
                                {
                                    tracing::warn!("auto-push failed for {ref_name}: {e}");
                                    let _ = push_event_tx.send(SessionEvent::PushFailed {
                                        ref_name,
                                        error: e.to_string(),
                                    });
                                }
                            })
                            .ok();
                    }
                }

                let _ = event_tx.send(SessionEvent::TurnComplete {
                    stop_reason: response.stop_reason,
                    commit_oid,
                });
            }
            Err(e) => {
                let _ = event_tx.send(SessionEvent::Error(Error::protocol(format!(
                    "prompt failed: {e}"
                ))));
            }
        }
    }

    Ok(())
}
