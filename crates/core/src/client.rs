use std::{cell::RefCell, rc::Rc};

use agent_client_protocol::{
    CreateTerminalRequest, CreateTerminalResponse, ExtNotification, ExtRequest, ExtResponse,
    KillTerminalCommandRequest, KillTerminalCommandResponse, PermissionOptionKind,
    ReadTextFileRequest, ReadTextFileResponse, ReleaseTerminalRequest, ReleaseTerminalResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, TerminalExitStatus, TerminalOutputRequest,
    TerminalOutputResponse, WaitForTerminalExitRequest, WaitForTerminalExitResponse,
    WriteTextFileRequest, WriteTextFileResponse,
};
use serde_json::value::RawValue;

use crate::{
    checkpoint::CheckpointStore, fs::FileSystem, notification::NotificationSender,
    permission::PermissionHandler, terminal::TerminalManager,
};

/// Implements the ACP `Client` trait by delegating to owned tool modules.
///
/// Lives in the `!Send` `LocalSet` context of a session thread.
pub struct ClientHandler {
    fs: FileSystem,
    terminals: TerminalManager,
    permissions: PermissionHandler,
    notifications: NotificationSender,
    checkpoint: Rc<RefCell<Option<CheckpointStore>>>,
}

impl ClientHandler {
    pub fn new(
        fs: FileSystem,
        terminals: TerminalManager,
        permissions: PermissionHandler,
        notifications: NotificationSender,
        checkpoint: Rc<RefCell<Option<CheckpointStore>>>,
    ) -> Self {
        Self {
            fs,
            terminals,
            permissions,
            notifications,
            checkpoint,
        }
    }

    /// Amend the current checkpoint after a tool call that may have changed files.
    fn amend_checkpoint(&self) {
        if let Some(store) = self.checkpoint.borrow().as_ref()
            && let Err(e) = store.amend_checkpoint()
        {
            tracing::warn!("checkpoint amend failed: {e}");
        }
    }
}

/// Convert our internal errors to ACP protocol errors.
fn to_acp_error(err: crate::error::Error) -> agent_client_protocol::Error {
    agent_client_protocol::Error::new(-32603, err.to_string())
}

#[async_trait::async_trait(?Send)]
impl agent_client_protocol::Client for ClientHandler {
    async fn request_permission(
        &self,
        args: RequestPermissionRequest,
    ) -> agent_client_protocol::Result<RequestPermissionResponse> {
        tracing::debug!("permission request for session {}", args.session_id);

        // Auto-accept: find the first "allow" option.
        let _granted = self.permissions.check("auto");
        let option = args
            .options
            .iter()
            .find(|o| {
                matches!(
                    o.kind,
                    PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
                )
            })
            .ok_or_else(|| {
                agent_client_protocol::Error::new(-32603, "no allow option available")
            })?;

        Ok(RequestPermissionResponse::new(
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                option.option_id.clone(),
            )),
        ))
    }

    async fn session_notification(
        &self,
        args: SessionNotification,
    ) -> agent_client_protocol::Result<()> {
        self.notifications.send(args);
        Ok(())
    }

    async fn read_text_file(
        &self,
        args: ReadTextFileRequest,
    ) -> agent_client_protocol::Result<ReadTextFileResponse> {
        tracing::debug!("read_text_file: {:?}", args.path);
        let content = self
            .fs
            .read_text_file(&args.path)
            .await
            .map_err(to_acp_error)?;

        // Handle line/limit parameters.
        let content = match (args.line, args.limit) {
            (Some(start_line), Some(limit)) => {
                let start = start_line.saturating_sub(1) as usize;
                content
                    .lines()
                    .skip(start)
                    .take(limit as usize)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            (Some(start_line), None) => {
                let start = start_line.saturating_sub(1) as usize;
                content.lines().skip(start).collect::<Vec<_>>().join("\n")
            }
            (None, Some(limit)) => content
                .lines()
                .take(limit as usize)
                .collect::<Vec<_>>()
                .join("\n"),
            (None, None) => content,
        };

        Ok(ReadTextFileResponse::new(content))
    }

    async fn write_text_file(
        &self,
        args: WriteTextFileRequest,
    ) -> agent_client_protocol::Result<WriteTextFileResponse> {
        tracing::debug!("write_text_file: {:?}", args.path);
        self.fs
            .write_text_file(&args.path, &args.content)
            .await
            .map_err(to_acp_error)?;

        self.amend_checkpoint();

        Ok(WriteTextFileResponse::new())
    }

    async fn create_terminal(
        &self,
        args: CreateTerminalRequest,
    ) -> agent_client_protocol::Result<CreateTerminalResponse> {
        tracing::debug!("create_terminal: {} {:?}", args.command, args.args);
        let cwd: Option<String> = args.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
        let terminal_id = self
            .terminals
            .create(&args.command, &args.args, cwd.as_deref())
            .await
            .map_err(to_acp_error)?;
        Ok(CreateTerminalResponse::new(terminal_id))
    }

    async fn terminal_output(
        &self,
        args: TerminalOutputRequest,
    ) -> agent_client_protocol::Result<TerminalOutputResponse> {
        let id = args.terminal_id.0.as_ref();
        let output = self.terminals.read_output(id).await.map_err(to_acp_error)?;
        Ok(TerminalOutputResponse::new(output, false))
    }

    async fn release_terminal(
        &self,
        args: ReleaseTerminalRequest,
    ) -> agent_client_protocol::Result<ReleaseTerminalResponse> {
        let id = args.terminal_id.0.as_ref();
        self.terminals.release(id).map_err(to_acp_error)?;
        Ok(ReleaseTerminalResponse::new())
    }

    async fn wait_for_terminal_exit(
        &self,
        args: WaitForTerminalExitRequest,
    ) -> agent_client_protocol::Result<WaitForTerminalExitResponse> {
        let id = args.terminal_id.0.as_ref();
        let exit_code = self
            .terminals
            .wait_for_exit(id)
            .await
            .map_err(to_acp_error)?;
        let status = TerminalExitStatus::new().exit_code(exit_code as u32);

        self.amend_checkpoint();

        Ok(WaitForTerminalExitResponse::new(status))
    }

    async fn kill_terminal_command(
        &self,
        args: KillTerminalCommandRequest,
    ) -> agent_client_protocol::Result<KillTerminalCommandResponse> {
        let id = args.terminal_id.0.as_ref();
        self.terminals.kill(id).map_err(to_acp_error)?;
        Ok(KillTerminalCommandResponse::new())
    }

    async fn ext_method(&self, _args: ExtRequest) -> agent_client_protocol::Result<ExtResponse> {
        Ok(ExtResponse::new(RawValue::NULL.to_owned().into()))
    }

    async fn ext_notification(&self, _args: ExtNotification) -> agent_client_protocol::Result<()> {
        Ok(())
    }
}
