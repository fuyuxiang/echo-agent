use crate::file_system::{AsyncFileSystem, FsError};
use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;
use std::path::{Path, PathBuf};

pub struct AcpSessionFs {
    root: PathBuf,
    gateway: GatewaySender,
    session_id: acp::SessionId,
    /// When set, any path under `display_cwd` is rewritten to `root` before
    /// being sent to the extension.  This is the defense-in-depth guard for
    /// AB overlay isolation: if a tool accidentally passes the display path
    /// (e.g., `/testbed/project/foo.rs`) instead of the overlay path
    /// (`~/.echo-agent/worktrees/.../b-overlay/foo.rs`), the adapter rewrites it
    /// so the extension reads/writes to the correct overlay location.
    display_cwd: Option<PathBuf>,
}

impl AcpSessionFs {
    pub fn new(root: PathBuf, session_id: acp::SessionId, gateway: GatewaySender) -> Self {
        Self {
            root,
            session_id,
            gateway,
            display_cwd: None,
        }
    }

    /// Set the display CWD for path rewriting.
    ///
    /// When AB FS isolation is active, the model sees `display_cwd`
    /// (e.g., `/testbed/project`) but writes should go to `root`
    /// (the overlay path).  Any path under `display_cwd` is rewritten
    /// to the equivalent path under `root`.
    pub fn with_display_cwd(mut self, display_cwd: PathBuf) -> Self {
        self.display_cwd = Some(display_cwd);
        self
    }

    /// Rewrite a display path to the overlay path if needed.
    fn resolve_path(&self, path: &Path) -> PathBuf {
        if let Some(ref display) = self.display_cwd
            && let Ok(suffix) = path.strip_prefix(display)
        {
            let resolved = self.root.join(suffix);
            tracing::debug!(
                display_path = %path.display(),
                resolved_path = %resolved.display(),
                "AcpSessionFs: rewrote display path to overlay path"
            );
            return resolved;
        }
        path.to_path_buf()
    }
}

#[async_trait::async_trait]
impl AsyncFileSystem for AcpSessionFs {
    fn root(&self) -> &Path {
        &self.root
    }

    async fn exists(&self, path: &Path) -> Result<bool, FsError> {
        let resolved = self.resolve_path(path);
        let read_req = acp::ReadTextFileRequest::new(self.session_id.clone(), resolved).limit(0);
        match self.gateway.send(read_req).await {
            Ok(_) => Ok(true),
            Err(e) if e.code == acp::ErrorCode::ResourceNotFound => Ok(false),
            Err(e) => Err(FsError::Other(e.to_string())),
        }
    }

    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        let resolved = self.resolve_path(path);
        let read_req = acp::ReadTextFileRequest::new(self.session_id.clone(), resolved);
        let response = self
            .gateway
            .send(read_req)
            .await
            .map_err(|e| FsError::Other(e.to_string()))?;
        Ok(response.content.into_bytes())
    }

    async fn try_read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, FsError> {
        let resolved = self.resolve_path(path);
        let read_req = acp::ReadTextFileRequest::new(self.session_id.clone(), resolved);
        match self.gateway.send(read_req).await {
            Ok(response) => Ok(Some(response.content.into_bytes())),
            Err(e) if e.code == acp::ErrorCode::ResourceNotFound => Ok(None),
            Err(e) => Err(FsError::Other(e.to_string())),
        }
    }

    async fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), FsError> {
        let resolved = self.resolve_path(path);
        let write_req = acp::WriteTextFileRequest::new(
            self.session_id.clone(),
            resolved,
            String::from_utf8(data.to_vec()).map_err(|e| FsError::Other(e.to_string()))?,
        );
        self.gateway
            .send(write_req)
            .await
            .map_err(|e| FsError::Other(e.to_string()))?;
        Ok(())
    }

    async fn delete_file(&self, path: &Path) -> Result<(), FsError> {
        // ACP protocol doesn't support file deletion yet
        // For now, we'll log a warning and return Ok (no-op)
        tracing::warn!(?path, "ACP filesystem does not support file deletion");
        Err(FsError::Other(
            "File deletion not supported via ACP".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // resolve_path only uses self.root and self.display_cwd — extract
    // the logic into a standalone test helper that doesn't need a gateway.
    fn test_resolve(root: &str, display_cwd: Option<&str>, input: &str) -> PathBuf {
        let root = PathBuf::from(root);
        let display = display_cwd.map(PathBuf::from);
        // Inline the same logic as resolve_path
        if let Some(ref display) = display
            && let Ok(suffix) = Path::new(input).strip_prefix(display)
        {
            return root.join(suffix);
        }
        PathBuf::from(input)
    }

    #[test]
    fn resolve_path_rewrites_display_to_overlay() {
        let result = test_resolve(
            "/root/.echo-agent/worktrees/proj/ab-123-b-overlay",
            Some("/testbed/proj"),
            "/testbed/proj/src/main.rs",
        );
        assert_eq!(
            result,
            PathBuf::from("/root/.echo-agent/worktrees/proj/ab-123-b-overlay/src/main.rs")
        );
    }

    #[test]
    fn resolve_path_passes_through_overlay_path() {
        let overlay_path = "/root/.echo-agent/worktrees/proj/ab-123-b-overlay/src/main.rs";
        let result = test_resolve(
            "/root/.echo-agent/worktrees/proj/ab-123-b-overlay",
            Some("/testbed/proj"),
            overlay_path,
        );
        assert_eq!(result, PathBuf::from(overlay_path));
    }

    #[test]
    fn resolve_path_no_display_cwd_passthrough() {
        let result = test_resolve(
            "/root/.echo-agent/worktrees/proj/ab-123-b-overlay",
            None,
            "/testbed/proj/src/main.rs",
        );
        assert_eq!(result, PathBuf::from("/testbed/proj/src/main.rs"));
    }

    #[test]
    fn resolve_path_relative_path_passthrough() {
        let result = test_resolve(
            "/root/.echo-agent/worktrees/proj/ab-123-b-overlay",
            Some("/testbed/proj"),
            "src/main.rs",
        );
        assert_eq!(result, PathBuf::from("src/main.rs"));
    }
}
