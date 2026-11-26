//! Filesystem extension ops (`workspace.fs_*`) — the server-proxied backing
//! for the shell's `echo.agent/fs/*` ACP extension methods.
//!
//! These mirror the pure functions that previously lived only in the
//! shell (`echo-agent-runtime/src/session/file_system.rs`) so that, in proxy
//! mode, a `echo.agent/fs/*` request executes on the *remote* workspace server
//! instead of the agent host. Each request type implements
//! [`WorkspaceOp`], so it runs in-process for local sessions and routes
//! over the server `workspace_rpc` tool for proxy sessions — identical wire
//! output either way.
//!
//! Path resolution: an absolute `path` is used directly; a relative
//! `path` is joined onto `cwd` (the per-session cwd the shell resolves
//! and sends) or, when absent, the workspace root.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine;
use chrono::Utc;

use crate::error::{WorkspaceError, WorkspaceResult};
use crate::handle::WorkspaceHandle;
use crate::workspace_ops::WorkspaceOp;

// Canonical in echo-agent-workspace-types; re-exported for existing paths.
use echo_agent_workspace_types::rpc::fs::FsReadEncoding;
pub use echo_agent_workspace_types::rpc::fs::{
    FsDeleteFileReq, FsExistsData, FsExistsReq, FsListData, FsListNode, FsListReq, FsReadFileData,
    FsReadFileReq, FsWriteFileReq,
};

/// Resolve a request `path` to an absolute path. Absolute paths are used
/// directly; relative paths join `cwd` (the shell-resolved per-session
/// cwd) or, when absent, the workspace root.
fn resolve_abs(
    path: &str,
    cwd: &Option<PathBuf>,
    ws: &WorkspaceHandle,
) -> WorkspaceResult<PathBuf> {
    let p = Path::new(path);
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    let base = match cwd {
        Some(c) => c.clone(),
        None => ws.root_cwd()?,
    };
    Ok(base.join(p))
}

#[async_trait]
impl WorkspaceOp for FsListReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let abs_unconfined = resolve_abs(&self.path, &self.cwd, ws)?;
        let (abs, confine_root) = ws.confine_to_workspace_root(&abs_unconfined).await?;
        // Off-executor: `list` does synchronous walk + metadata syscalls.
        let req = self.clone();
        tokio::task::spawn_blocking(move || list(&abs, &req, confine_root))
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))?
    }
}

#[async_trait]
impl WorkspaceOp for FsExistsReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let abs_unconfined = resolve_abs(&self.path, &self.cwd, ws)?;
        let (abs, _) = ws.confine_to_workspace_root(&abs_unconfined).await?;
        let exists = tokio::fs::try_exists(&abs).await.unwrap_or(false);
        Ok(FsExistsData { exists })
    }
}

#[async_trait]
impl WorkspaceOp for FsReadFileReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let abs_unconfined = resolve_abs(&self.path, &self.cwd, ws)?;
        let (abs, _) = ws.confine_to_workspace_root(&abs_unconfined).await?;

        // Legacy full-file read path: preserves the pre-range wire output
        // (auto utf8/base64 detect, MIME `type`, `lineCount`).
        let ranged = self.offset.is_some()
            || self.length.is_some()
            || self.encoding == FsReadEncoding::Base64;
        if !ranged {
            let bytes = tokio::fs::read(&abs)
                .await
                .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
            return Ok(build_file_entry(&bytes));
        }

        // Binary-safe ranged read: `size` is the full file size, the
        // chunk is `[offset, offset + min(length, max_bytes, cap))`.
        let md = tokio::fs::metadata(&abs)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        if md.is_dir() {
            return Err(WorkspaceError::HubError(format!(
                "not a file: {}",
                self.path
            )));
        }
        // Best-effort snapshot: a concurrent truncate/grow between here and
        // read_range can make `size` inconsistent with the returned chunk.
        let size = md.len();
        let offset = self.offset.unwrap_or(0);
        let length = super::walk::clamp_read_length(self.length, self.max_bytes);
        let chunk = super::walk::read_range(&abs, offset, length)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        Ok(build_ranged_entry(chunk, size, self.encoding))
    }
}

#[async_trait]
impl WorkspaceOp for FsWriteFileReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let abs_unconfined = resolve_abs(&self.path, &self.cwd, ws)?;
        let (abs, _) = ws.confine_to_workspace_root(&abs_unconfined).await?;
        let content = self.content.clone();
        let create_dirs = self.create_dirs;
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            if create_dirs && let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&abs, content.as_bytes())
        })
        .await
        .map_err(|e| WorkspaceError::HubError(e.to_string()))?
        .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl WorkspaceOp for FsDeleteFileReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let abs_unconfined = resolve_abs(&self.path, &self.cwd, ws)?;
        let (abs, _) = ws.confine_to_workspace_root(&abs_unconfined).await?;
        tokio::fs::remove_file(&abs)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        Ok(())
    }
}

// =========================================================================
// Pure helpers — ported verbatim from the shell so output is identical.
// =========================================================================

fn list(
    abs_path: &Path,
    req: &FsListReq,
    confine_to_canonical_root: Option<PathBuf>,
) -> WorkspaceResult<FsListData> {
    // Confined to the canonical root when set (escaping symlinks not enumerated);
    // `None` (the default) walks unconfined.
    let page = super::walk::list_directory_paged(
        abs_path,
        super::walk::ListOptions {
            depth: req.depth,
            follow_symlinks: req.follow_symlinks,
            respect_git_ignore: req.respect_git_ignore,
            include_hidden: req.include_hidden,
            include_globs: &req.include_globs,
            exclude_globs: &req.exclude_globs,
            offset: req.offset,
            limit: req.limit,
            confine_to_canonical_root,
        },
        super::walk::MAX_LIST_COLLECT,
    );

    let nodes: Vec<FsListNode> = page
        .entries
        .into_iter()
        .map(|e| FsListNode {
            node_type: if e.is_dir { "directory" } else { "file" }.to_string(),
            size: e.size,
            modified_at: e.modified.map(|st| {
                let dt: chrono::DateTime<Utc> = st.into();
                dt.to_rfc3339()
            }),
            is_symlink: e.is_symlink.then_some(true),
            path: e.abs_path.to_string_lossy().into_owned(),
            name: e.name,
        })
        .collect();

    Ok(FsListData {
        nodes,
        truncated: page.truncated,
    })
}

/// Map a binary-safe ranged chunk to the shell-facing `FsReadFileData`.
/// `size` is the full file size; `lineCount` is omitted for ranged reads
/// and the MIME `type` is a coarse text/binary tag (mid-file chunks make
/// magic-byte sniffing meaningless).
fn build_ranged_entry(chunk: Vec<u8>, size: u64, encoding: FsReadEncoding) -> FsReadFileData {
    let (payload, is_text) = super::walk::encode_chunk(chunk, encoding);
    let (content, content_base64) = match payload {
        super::walk::ChunkPayload::Text(t) => (t, None),
        super::walk::ChunkPayload::Base64(b) => (String::new(), Some(b)),
    };
    FsReadFileData {
        content,
        content_base64,
        size,
        line_count: None,
        content_type: if is_text {
            "text/plain".to_string()
        } else {
            "application/octet-stream".to_string()
        },
    }
}

fn build_file_entry(bytes: &[u8]) -> FsReadFileData {
    let size = bytes.len() as u64;
    let inferred = infer::get(bytes).map(|t| t.mime_type().to_string());
    match String::from_utf8(bytes.to_vec()) {
        Ok(text) => FsReadFileData {
            line_count: Some(text.lines().count() as u64),
            content: text,
            content_base64: None,
            size,
            content_type: inferred.unwrap_or_else(|| "text/plain".to_string()),
        },
        Err(_) => FsReadFileData {
            content: String::new(),
            content_base64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            size,
            line_count: None,
            content_type: inferred.unwrap_or_else(|| "application/octet-stream".to_string()),
        },
    }
}

// =========================================================================
// Tests for the pure helpers (no `WorkspaceHandle` required).
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an `FsListReq` for a temp dir test. `path`/`cwd` are unused by
    /// `list` (it takes the resolved abs path directly); `respect_git_ignore`
    /// is off so the temp dir's location can't filter out our fixtures.
    fn list_req(limit: usize) -> FsListReq {
        FsListReq {
            path: String::new(),
            cwd: None,
            depth: 1,
            limit,
            offset: 0,
            include_hidden: true,
            follow_symlinks: true,
            respect_git_ignore: false,
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
        }
    }

    #[test]
    fn build_file_entry_utf8_sets_content_and_line_count() {
        let bytes = b"line one\nline two\n";
        let entry = build_file_entry(bytes);
        assert_eq!(entry.content, "line one\nline two\n");
        assert!(entry.content_base64.is_none());
        assert_eq!(entry.line_count, Some(2));
        assert_eq!(entry.size, bytes.len() as u64);
    }

    #[test]
    fn build_file_entry_invalid_utf8_uses_base64() {
        let bytes: &[u8] = &[0xff, 0xfe, 0x00];
        let entry = build_file_entry(bytes);
        assert!(entry.content.is_empty());
        assert_eq!(
            entry.content_base64,
            Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
        );
        assert!(entry.line_count.is_none());
        assert_eq!(entry.size, bytes.len() as u64);
    }

    #[test]
    fn list_enumerates_nodes_without_truncation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("a.txt"), b"a").expect("write a");
        std::fs::write(root.join("b.txt"), b"bb").expect("write b");
        std::fs::create_dir(root.join("sub")).expect("mkdir sub");

        let data = list(root, &list_req(1000), Some(root.to_path_buf())).expect("list");

        let names: Vec<&str> = data.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(data.nodes.len(), 3, "names: {names:?}");
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
        assert!(names.contains(&"sub"));
        assert!(!data.truncated);
        // Directories sort ahead of files.
        assert_eq!(data.nodes[0].name, "sub");
        assert_eq!(data.nodes[0].node_type, "directory");
    }

    #[test]
    fn list_marks_truncated_when_limit_reached() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("a.txt"), b"a").expect("write a");
        std::fs::write(root.join("b.txt"), b"bb").expect("write b");
        std::fs::create_dir(root.join("sub")).expect("mkdir sub");

        let data = list(root, &list_req(1), Some(root.to_path_buf())).expect("list");
        assert_eq!(data.nodes.len(), 1);
        assert!(data.truncated);
    }

    #[test]
    fn list_paginates_with_offset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for n in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            std::fs::write(root.join(n), b"x").expect("write");
        }
        let names = |d: FsListData| d.nodes.into_iter().map(|n| n.name).collect::<Vec<_>>();

        let mut req = list_req(2);
        let p0 = list(root, &req, Some(root.to_path_buf())).expect("list");
        assert!(p0.truncated);
        assert_eq!(names(p0), vec!["a.txt", "b.txt"]);

        req.offset = 2;
        let p1 = list(root, &req, Some(root.to_path_buf())).expect("list");
        assert!(!p1.truncated, "last page is not truncated");
        assert_eq!(names(p1), vec!["c.txt", "d.txt"]);

        // Offset past the end yields an empty, non-truncated page.
        req.offset = 10;
        let p2 = list(root, &req, Some(root.to_path_buf())).expect("list");
        assert!(!p2.truncated);
        assert!(p2.nodes.is_empty());
    }

    #[test]
    fn build_ranged_entry_encodes_utf8_and_binary() {
        // UTF-8 chunk under the default encoding → `content`, text/plain,
        // full `size` echoed, no line count.
        let e = build_ranged_entry(b"hello".to_vec(), 100, FsReadEncoding::Utf8);
        assert_eq!(e.content, "hello");
        assert!(e.content_base64.is_none());
        assert_eq!(e.size, 100);
        assert!(e.line_count.is_none());
        assert_eq!(e.content_type, "text/plain");

        // Explicit base64 of valid UTF-8 stays text/plain but travels in
        // `contentBase64`.
        let e = build_ranged_entry(b"hi".to_vec(), 2, FsReadEncoding::Base64);
        assert!(e.content.is_empty());
        assert_eq!(
            e.content_base64,
            Some(base64::engine::general_purpose::STANDARD.encode(b"hi")),
        );
        assert_eq!(e.content_type, "text/plain");

        // Non-UTF-8 bytes fall back to base64 + octet-stream.
        let raw = vec![0xff_u8, 0x00, 0xfe];
        let e = build_ranged_entry(raw.clone(), 3, FsReadEncoding::Utf8);
        assert!(e.content.is_empty());
        assert_eq!(
            e.content_base64,
            Some(base64::engine::general_purpose::STANDARD.encode(&raw)),
        );
        assert_eq!(e.content_type, "application/octet-stream");
    }

    // Confinement (WorkspaceOp::execute) — covers both local and proxy dispatch.

    #[tokio::test]
    async fn read_write_within_root_ok() {
        let ws = crate::handle::tests::make_confining_handle();
        let root = ws.root_cwd().unwrap();
        FsWriteFileReq {
            path: "sub/data.txt".into(),
            cwd: Some(root.clone()),
            content: "hello".into(),
            create_dirs: true,
        }
        .execute(&ws, None)
        .await
        .expect("in-root write must succeed");
        let data = FsReadFileReq {
            path: "sub/data.txt".into(),
            cwd: Some(root.clone()),
            offset: None,
            length: None,
            max_bytes: 1 << 20,
            encoding: FsReadEncoding::Utf8,
        }
        .execute(&ws, None)
        .await
        .expect("in-root read must succeed");
        assert_eq!(data.content, "hello");
    }

    #[tokio::test]
    async fn read_file_rejects_absolute_escape() {
        let ws = crate::handle::tests::make_confining_handle();
        let err = FsReadFileReq {
            path: "/etc/passwd".into(),
            cwd: None,
            offset: None,
            length: None,
            max_bytes: 1 << 20,
            encoding: FsReadEncoding::Utf8,
        }
        .execute(&ws, None)
        .await
        .expect_err("absolute escape must be rejected");
        assert!(
            err.to_string().contains("workspace root"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn read_file_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let ws = crate::handle::tests::make_confining_handle();
        let root = ws.root_cwd().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
        symlink(outside.path(), root.join("escape_link")).unwrap();

        let err = FsReadFileReq {
            path: "escape_link/secret.txt".into(),
            cwd: Some(root.clone()),
            offset: None,
            length: None,
            max_bytes: 1 << 20,
            encoding: FsReadEncoding::Utf8,
        }
        .execute(&ws, None)
        .await
        .expect_err("symlink escape must be rejected");
        assert!(
            err.to_string().contains("workspace root"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_file_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let ws = crate::handle::tests::make_confining_handle();
        let root = ws.root_cwd().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.join("escape_link")).unwrap();

        let err = FsWriteFileReq {
            path: "escape_link/injected.txt".into(),
            cwd: Some(root.clone()),
            content: "x".into(),
            create_dirs: true,
        }
        .execute(&ws, None)
        .await
        .expect_err("symlink escape write must be rejected");
        assert!(
            err.to_string().contains("workspace root"),
            "unexpected error: {err}"
        );
        assert!(
            !outside.path().join("injected.txt").exists(),
            "write must not land outside the workspace root"
        );
    }

    // A *dangling* in-root symlink (target outside root, not yet created) must
    // not let a write escape via `open(O_CREAT)` following the link.
    #[tokio::test]
    #[cfg(unix)]
    async fn write_file_rejects_dangling_symlink_escape() {
        use std::os::unix::fs::symlink;
        let ws = crate::handle::tests::make_confining_handle();
        let root = ws.root_cwd().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("new.txt");
        symlink(&target, root.join("lnk")).unwrap();

        let err = FsWriteFileReq {
            path: "lnk".into(),
            cwd: Some(root.clone()),
            content: "x".into(),
            create_dirs: true,
        }
        .execute(&ws, None)
        .await
        .expect_err("dangling symlink escape write must be rejected");
        assert!(
            err.to_string().contains("workspace root"),
            "unexpected error: {err}"
        );
        assert!(
            !target.exists(),
            "write must not create the file outside root"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn list_excludes_symlink_escape() {
        use std::os::unix::fs::symlink;
        let ws = crate::handle::tests::make_confining_handle();
        let root = ws.root_cwd().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), b"x").unwrap();
        symlink(outside.path(), root.join("escape_link")).unwrap();
        std::fs::write(root.join("inside.txt"), b"y").unwrap();

        let mut req = list_req(1000);
        req.path = ".".into();
        req.cwd = Some(root.clone());
        req.depth = 2;
        let data = req.execute(&ws, None).await.expect("list must succeed");
        let names: Vec<&str> = data.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"inside.txt"), "in-root file: {names:?}");
        assert!(
            !data.nodes.iter().any(|n| n.path.contains("secret.txt")),
            "escaping symlink target must not be enumerated: {names:?}"
        );
    }
}
