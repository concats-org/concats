use std::{collections::HashMap, path::PathBuf};

use agent_client_protocol::{
    Agent, CancelNotification, ClientCapabilities, ClientSideConnection, ContentBlock,
    FileSystemCapability, InitializeRequest, NewSessionRequest, PromptRequest, ProtocolVersion,
    SessionId, SessionNotification, StopReason,
};
use concats_core::{
    Oid, current_head_oid,
    session::{self, Session},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc,
    task::LocalSet,
};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::{
    agent::{AgentProcess, AgentStreams},
    checkpoint_recorder::CheckpointRecorder,
    client::ClientHandler,
    error::{Error, Result},
    fs::FileSystem,
    notification::NotificationSender,
    terminal::TerminalManager,
};

struct ActiveSession {
    acp_session_id: SessionId,
    conn: ClientSideConnection,
    agent: AgentProcess,
    recorder: CheckpointRecorder,
    session_ref: Session,
}

enum PromptOutcome {
    Completed(agent_client_protocol::Result<agent_client_protocol::PromptResponse>),
    Cancelled,
}

/// Configuration for starting a new agent session.
pub struct SessionConfig {
    pub agent_command: String,
    pub agent_args: Vec<String>,
    pub workspace_root: PathBuf,
    #[allow(clippy::zero_sized_map_values)]
    pub env: HashMap<String, String>,
    pub fork_from: Option<Oid>,
    pub auto_push: bool,
    pub push_remote: String,
}

/// Events emitted by a running session.
pub enum SessionEvent {
    SessionConfigured {
        mode: Option<String>,
        config_options: Vec<agent_client_protocol::SessionConfigOption>,
    },
    Notification(Box<SessionNotification>),
    TurnComplete {
        stop_reason: StopReason,
        commit_oid: Option<Oid>,
    },
    Stderr(String),
    PushFailed {
        ref_name: String,
        error: String,
    },
    Error(Error),
}

/// Send-safe handle for interacting with a running session from the TUI thread.
pub struct SessionHandle {
    pub prompt_tx: mpsc::Sender<String>,
    event_rx: Option<mpsc::Receiver<SessionEvent>>,
    pub cancel_tx: mpsc::Sender<()>,
}

impl SessionHandle {
    pub fn take_event_rx(&mut self) -> Option<mpsc::Receiver<SessionEvent>> {
        self.event_rx.take()
    }
}

/// Start a background ACP session thread and return channels for interacting
/// with it.
///
/// # Errors
///
/// Returns an error if the session thread cannot be spawned.
pub fn start_session(config: SessionConfig) -> Result<SessionHandle> {
    let (prompt_tx, prompt_rx) = mpsc::channel::<String>(16);
    let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(1024);
    let (cancel_tx, cancel_rx) = mpsc::channel::<()>(1);

    let handle = SessionHandle {
        prompt_tx,
        event_rx: Some(event_rx),
        cancel_tx,
    };

    std::thread::Builder::new()
        .name("session".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = event_tx.blocking_send(SessionEvent::Error(Error::session(format!(
                        "failed to build session runtime: {error}"
                    ))));
                    return;
                }
            };
            let local = LocalSet::new();
            local.block_on(
                &runtime,
                session_loop(config, prompt_rx, event_tx, cancel_rx),
            );
        })
        .map_err(|e| Error::session(format!("failed to spawn session thread: {e}")))?;

    Ok(handle)
}

async fn session_loop(
    config: SessionConfig,
    mut prompt_rx: mpsc::Receiver<String>,
    event_tx: mpsc::Sender<SessionEvent>,
    mut cancel_rx: mpsc::Receiver<()>,
) {
    if let Err(error) = session_loop_inner(&config, &mut prompt_rx, &event_tx, &mut cancel_rx).await
    {
        let _ = event_tx.send(SessionEvent::Error(error)).await;
    }
}

async fn session_loop_inner(
    config: &SessionConfig,
    prompt_rx: &mut mpsc::Receiver<String>,
    event_tx: &mpsc::Sender<SessionEvent>,
    cancel_rx: &mut mpsc::Receiver<()>,
) -> Result<()> {
    let mut active = connect_session(config, event_tx).await?;

    loop {
        tokio::select! {
            _ = cancel_rx.recv() => {
                shutdown_active_session(&mut active).await;
                return Ok(());
            }
            maybe_prompt = prompt_rx.recv() => {
                let Some(prompt_text) = maybe_prompt else {
                    shutdown_active_session(&mut active).await;
                    return Ok(());
                };

                active.recorder.start_prompt(&active.session_ref, &prompt_text);
                if run_prompt_turn(config, event_tx, &mut active, prompt_text, cancel_rx).await? {
                    return Ok(());
                }
            }
        }
    }
}

async fn connect_session(
    config: &SessionConfig,
    event_tx: &mpsc::Sender<SessionEvent>,
) -> Result<ActiveSession> {
    let mut agent = AgentProcess::spawn(
        &config.agent_command,
        &config.agent_args,
        &config.workspace_root,
        &config.env,
    )?;
    let AgentStreams {
        stdin,
        stdout,
        stderr,
    } = agent.take_streams()?;

    let recorder = CheckpointRecorder::new();
    let (notification_tx, notification_rx) = mpsc::channel::<SessionNotification>(1024);
    let handler = ClientHandler::new(
        FileSystem::new(config.workspace_root.clone()),
        TerminalManager::new(),
        NotificationSender::new(notification_tx),
        recorder.clone(),
    );

    let (conn, io_future) =
        ClientSideConnection::new(handler, stdin.compat_write(), stdout.compat(), |future| {
            tokio::task::spawn_local(future);
        });

    tokio::task::spawn_local(async move {
        if let Err(error) = io_future.await {
            tracing::error!("ACP IO error: {error}");
        }
    });

    spawn_stderr_forwarder(event_tx.clone(), stderr);
    spawn_notification_forwarder(recorder.clone(), event_tx.clone(), notification_rx);

    let (acp_session_id, session_id) = initialize_connection(config, event_tx, &conn).await?;
    let session_ref = session::create(&config.workspace_root, &session_id, resolve_base(config)?)?;

    Ok(ActiveSession {
        acp_session_id,
        conn,
        agent,
        recorder,
        session_ref,
    })
}

fn spawn_stderr_forwarder(
    event_tx: mpsc::Sender<SessionEvent>,
    stderr: tokio::process::ChildStderr,
) {
    tokio::task::spawn_local(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if event_tx.send(SessionEvent::Stderr(line)).await.is_err() {
                break;
            }
        }
    });
}

fn spawn_notification_forwarder(
    recorder: CheckpointRecorder,
    event_tx: mpsc::Sender<SessionEvent>,
    mut notification_rx: mpsc::Receiver<SessionNotification>,
) {
    tokio::task::spawn_local(async move {
        while let Some(notification) = notification_rx.recv().await {
            recorder.handle_notification(&notification);
            let _ = event_tx
                .send(SessionEvent::Notification(Box::new(notification)))
                .await;
        }
    });
}

async fn initialize_connection(
    config: &SessionConfig,
    event_tx: &mpsc::Sender<SessionEvent>,
    conn: &ClientSideConnection,
) -> Result<(SessionId, String)> {
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
        .map_err(|error| Error::protocol(format!("initialize failed: {error}")))?;

    let session_response = conn
        .new_session(NewSessionRequest::new(config.workspace_root.clone()))
        .await
        .map_err(|error| Error::protocol(format!("new_session failed: {error}")))?;
    let acp_session_id = session_response.session_id;
    let session_id = acp_session_id.to_string();
    let initial_mode = session_response
        .modes
        .as_ref()
        .map(|modes| modes.current_mode_id.to_string());
    let initial_config_options = session_response.config_options.unwrap_or_default();
    let _ = event_tx
        .send(SessionEvent::SessionConfigured {
            mode: initial_mode,
            config_options: initial_config_options,
        })
        .await;
    Ok((acp_session_id, session_id))
}

fn resolve_base(config: &SessionConfig) -> Result<Oid> {
    Ok(config
        .fork_from
        .map_or_else(|| current_head_oid(&config.workspace_root), Ok)?)
}

async fn run_prompt_turn(
    config: &SessionConfig,
    event_tx: &mpsc::Sender<SessionEvent>,
    active: &mut ActiveSession,
    prompt_text: String,
    cancel_rx: &mut mpsc::Receiver<()>,
) -> Result<bool> {
    let request = PromptRequest::new(
        active.acp_session_id.clone(),
        vec![ContentBlock::from(prompt_text)],
    );
    let outcome = execute_prompt_request(active, request, cancel_rx).await;

    match outcome {
        PromptOutcome::Completed(Ok(response)) => {
            active.recorder.finish_response();
            let commit_oid = active.recorder.current_oid();
            maybe_spawn_auto_push(config, event_tx, active.session_ref.clone());
            let _ = event_tx
                .send(SessionEvent::TurnComplete {
                    stop_reason: response.stop_reason,
                    commit_oid,
                })
                .await;
        }
        PromptOutcome::Completed(Err(error)) => {
            let _ = event_tx
                .send(SessionEvent::Error(Error::protocol(format!(
                    "prompt failed: {error}"
                ))))
                .await;
        }
        PromptOutcome::Cancelled => {
            shutdown_active_session(active).await;
            return Ok(true);
        }
    }

    Ok(false)
}

async fn execute_prompt_request(
    active: &ActiveSession,
    request: PromptRequest,
    cancel_rx: &mut mpsc::Receiver<()>,
) -> PromptOutcome {
    let prompt = active.conn.prompt(request);
    tokio::pin!(prompt);

    tokio::select! {
        response = &mut prompt => PromptOutcome::Completed(response),
        _ = cancel_rx.recv() => {
            if let Err(error) = active
                .conn
                .cancel(CancelNotification::new(active.acp_session_id.clone()))
                .await
            {
                tracing::warn!("failed to cancel ACP prompt: {error}");
            }
            PromptOutcome::Cancelled
        }
    }
}

fn maybe_spawn_auto_push(
    config: &SessionConfig,
    event_tx: &mpsc::Sender<SessionEvent>,
    session_ref: Session,
) {
    if !config.auto_push {
        return;
    }

    let push_remote = config.push_remote.clone();
    let push_event_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("push-ref".into())
        .spawn(move || {
            if let Err(error) = session::push(&session_ref, &push_remote) {
                let error: concats_core::error::Error = error;
                let ref_name = format!("refs/agent/sessions/{}", session_ref.id);
                tracing::warn!("auto-push failed for {ref_name}: {error}");
                let _ = push_event_tx.try_send(SessionEvent::PushFailed {
                    ref_name,
                    error: error.to_string(),
                });
            }
        })
        .ok();
}

async fn shutdown_active_session(active: &mut ActiveSession) {
    active.recorder.finish_response();

    if let Err(error) = active.agent.kill().await {
        tracing::debug!("agent kill during shutdown failed: {error}");
    }
    if let Err(error) = active.agent.wait().await {
        tracing::debug!("agent wait during shutdown failed: {error}");
    }
}
