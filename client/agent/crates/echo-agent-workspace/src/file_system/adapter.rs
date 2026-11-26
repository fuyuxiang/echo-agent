//! AcpFsAdapter: implements `echo-agent-tools::AsyncFileSystem` using ACP gateway calls.
//!
//! This adapter enables file tool execution over ACP (remote filesystem).
//! It translates echo-agent-tools' `AsyncFileSystem` trait into ACP protocol calls:
//!   `read_file()` → read_text_file
//!   `write_file()` → write_text_file
//!   `delete_file()` → not supported by ACP (returns error)
//!
//! Mirrors the pattern of `AcpTerminalAdapter` for terminal execution.

use std::path::Path;

use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;
use echo_agent_tools::computer::types::{AsyncFileSystem, ComputerError};

/// Wraps echo-agent-runtime's ACP gateway to satisfy echo-agent-tools' AsyncFileSystem.
///
/// When a client advertises `clientCapabilities.fs.readTextFile` and `writeTextFile`,
/// file operations from tools (read_file, search_replace, etc.) are routed through
/// the ACP gateway back to the client instead of hitting the local disk directly.
pub struct AcpFsAdapter {
    gateway: GatewaySender,
    session_id: acp::SessionId,
}

impl AcpFsAdapter {
    pub fn new(gateway: GatewaySender, session_id: acp::SessionId) -> Self {
        Self {
            gateway,
            session_id,
        }
    }
}

#[async_trait::async_trait]
impl AsyncFileSystem for AcpFsAdapter {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, ComputerError> {
        let read_req = acp::ReadTextFileRequest::new(self.session_id.clone(), path.to_path_buf());

        let response = self
            .gateway
            .send(read_req)
            .await
            .map_err(acp_error_to_computer_error)?;

        Ok(response.content.into_bytes())
    }

    async fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), ComputerError> {
        let content =
            String::from_utf8(data.to_vec()).map_err(|e| ComputerError::io(e.to_string()))?;

        let write_req =
            acp::WriteTextFileRequest::new(self.session_id.clone(), path.to_path_buf(), content);

        self.gateway
            .send(write_req)
            .await
            .map_err(acp_error_to_computer_error)?;

        Ok(())
    }

    async fn delete_file(&self, path: &Path) -> Result<(), ComputerError> {
        // ACP protocol doesn't support file deletion yet
        tracing::warn!(?path, "ACP filesystem does not support file deletion");
        Err(ComputerError::io("File deletion not supported via ACP"))
    }
}

fn acp_error_to_computer_error(err: acp::Error) -> ComputerError {
    match acp_error_to_io_kind(&err) {
        Some(kind) => ComputerError::io_with_kind(err.to_string(), kind),
        None => ComputerError::io(err.to_string()),
    }
}

fn acp_error_to_io_kind(err: &acp::Error) -> Option<std::io::ErrorKind> {
    let msg_lower = err.message.to_ascii_lowercase();

    if err.code == acp::ErrorCode::ResourceNotFound {
        Some(std::io::ErrorKind::NotFound)
    } else if msg_lower.contains("permission denied") {
        Some(std::io::ErrorKind::PermissionDenied)
    } else {
        None
    }
}
