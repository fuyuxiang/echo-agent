//! Filesystem extension API layer.
//!
//! Routing: absolute paths work directly; relative paths require sessionId for lookup.
//! Business logic delegated to `session::file_system::*` pure functions.
use super::{Empty, ExtResult, parse_params, to_ext_response};
use crate::agent::MvpAgent;
use crate::session::ExtMethodResult;
use crate::session::file_system::{
    self as fs, FsListParams, FsReadFileData, check_file_size_limits,
};
use agent_client_protocol as acp;
use echo_agent_workspace::file_system::FsReadEncoding;
use serde::Deserialize;
use std::path::{Path, PathBuf};
fn default_depth() -> usize {
    1
}
fn default_limit() -> usize {
    1000
}
fn default_follow_symlinks() -> bool {
    true
}
fn default_respect_git_ignore() -> bool {
    true
}
fn default_max_bytes() -> usize {
    1_048_576
}
fn default_create_dirs() -> bool {
    true
}
fn default_include_hidden() -> bool {
    true
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FsListRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    pub path: String,
    #[serde(default = "default_depth")]
    pub depth: usize,
    #[serde(default = "default_include_hidden")]
    pub include_hidden: bool,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Pagination offset applied after the dirs-first sort (default 0).
    #[serde(default)]
    pub offset: u64,
    #[serde(default = "default_follow_symlinks")]
    pub follow_symlinks: bool,
    #[serde(default = "default_respect_git_ignore")]
    pub respect_git_ignore: bool,
    #[serde(default)]
    pub include_globs: Vec<String>,
    #[serde(default)]
    pub exclude_globs: Vec<String>,
}
impl FsListRequest {
    fn to_params(&self) -> FsListParams {
        FsListParams {
            path: self.path.clone(),
            depth: self.depth,
            limit: self.limit,
            offset: self.offset,
            follow_symlinks: self.follow_symlinks,
            respect_git_ignore: self.respect_git_ignore,
            include_hidden: self.include_hidden,
            include_globs: self.include_globs.clone(),
            exclude_globs: self.exclude_globs.clone(),
        }
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FsExistsRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    pub path: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FsReadFileRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    pub path: String,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: usize,
    #[serde(default)]
    pub max_lines: Option<usize>,
    /// Byte offset for a binary-safe ranged read. When `offset`/`length`
    /// is set (or `encoding` is `base64`) the read returns the chunk
    /// `[offset, offset + length)`; otherwise the whole file is read
    /// (legacy behavior).
    #[serde(default)]
    pub offset: Option<u64>,
    /// Bytes to read for a ranged read. Absent means "to EOF", but the
    /// effective read is always capped at `max_bytes` (default 1 MiB) and the
    /// server's hard limit, so an unset `length` still yields at most
    /// `max_bytes`. Detect "more data" by comparing the returned bytes (from
    /// `offset`) against the response `size`.
    #[serde(default)]
    pub length: Option<u64>,
    /// Transfer encoding for ranged reads (default `utf8`; non-UTF-8
    /// ranges fall back to base64 regardless).
    #[serde(default)]
    pub encoding: FsReadEncoding,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FsWriteFileRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    pub path: String,
    pub content: String,
    #[serde(default = "default_create_dirs")]
    pub create_dirs: bool,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FsDeleteFileRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    pub path: String,
}
/// Resolve path from explicit value or session lookup.
/// For absolute paths, use directly. For relative paths, resolve from session cwd.
fn resolve_path(
    agent: &MvpAgent,
    path: &str,
    session_id: Option<&acp::SessionId>,
) -> Result<PathBuf, acp::Error> {
    let p = Path::new(path);
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    if let Some(sid) = session_id {
        if let Some(cwd) = agent.get_session_cwd(sid) {
            return Ok(cwd.join(p));
        }
        return Err(acp::Error::invalid_params().data(format!("session not found: {}", sid.0)));
    }
    Err(acp::Error::invalid_params().data("sessionId is required for relative paths"))
}
/// Confine `path` to the workspace root, falling back to the session cwd for
/// worktree sessions (rooted outside it). Returns the resolved path and an
/// optional confining walk root (`None` when confinement is off — the default,
/// so the fallback and error paths only apply on a confining sandbox workspace).
async fn confine_local(
    agent: &MvpAgent,
    path: &Path,
    session_id: Option<&acp::SessionId>,
) -> Result<(PathBuf, Option<PathBuf>), acp::Error> {
    let ops = agent.resolve_workspace_ops()?;
    let handle = ops.workspace_handle().ok_or_else(|| {
        acp::Error::internal_error().data("no local workspace handle for fs confinement")
    })?;
    let workspace_err = match handle.confine_to_workspace_root(path).await {
        Ok(confined) => return Ok(confined),
        Err(e) => e,
    };
    if let Some(sid) = session_id
        && let Some(session_cwd) = agent.get_session_cwd(sid)
        && let Ok(confined) = handle.confine_to_root(path, &session_cwd).await
    {
        return Ok(confined);
    }
    Err(acp::Error::invalid_params().data(workspace_err.to_string()))
}
pub(crate) fn is_fs_method(method: &str) -> bool {
    method.starts_with("echo.agent/fs/")
}
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/fs/list" => {
            let req = parse_params::<FsListRequest>(args)?;
            let path = resolve_path(agent, &req.path, req.session_id.as_ref())?;
            let (path, confine_root) = confine_local(agent, &path, req.session_id.as_ref()).await?;
            let params = req.to_params();
            let result = fs::list(&path, &params, confine_root).await;
            to_ext_response(result)
        }
        "echo.agent/fs/exists" => {
            let req = parse_params::<FsExistsRequest>(args)?;
            let path = resolve_path(agent, &req.path, req.session_id.as_ref())?;
            let (path, _) = match confine_local(agent, &path, req.session_id.as_ref()).await {
                Ok(confined) => confined,
                Err(_) => return to_ext_response(Ok(fs::FsExistsData { exists: false })),
            };
            let result = fs::exists(&path).await;
            to_ext_response(result)
        }
        "echo.agent/fs/read_file" => {
            let req = parse_params::<FsReadFileRequest>(args)?;
            let max_lines = req.max_lines;
            let path_str = req.path.clone();
            let ranged = req.offset.is_some()
                || req.length.is_some()
                || req.encoding == FsReadEncoding::Base64;
            let path = resolve_path(agent, &req.path, req.session_id.as_ref())?;
            let (path, _) = confine_local(agent, &path, req.session_id.as_ref()).await?;
            let read_result: anyhow::Result<FsReadFileData> = if ranged {
                fs::read_file_ranged(
                    &path,
                    req.offset.unwrap_or(0),
                    req.length.unwrap_or(u64::MAX),
                    req.max_bytes as u64,
                    req.encoding,
                )
                .await
            } else {
                fs::read_file(&path).await
            };
            match read_result {
                Ok(data) => {
                    let size_check = if ranged {
                        Ok(())
                    } else {
                        check_file_size_limits(&data, &path_str, None, max_lines)
                    };
                    if let Err(err) = size_check {
                        let ext_result: ExtMethodResult<FsReadFileData> = err.into();
                        ext_result
                            .to_ext_response()
                            .map_err(|e| acp::Error::internal_error().data(e.to_string()))
                    } else {
                        to_ext_response(Ok(data))
                    }
                }
                Err(e) => to_ext_response(Err::<FsReadFileData, _>(e)),
            }
        }
        "echo.agent/fs/write_file" => {
            let req = parse_params::<FsWriteFileRequest>(args)?;
            let path = resolve_path(agent, &req.path, req.session_id.as_ref())?;
            let (path, _) = confine_local(agent, &path, req.session_id.as_ref()).await?;
            let result = fs::write_file(&path, &req.content, req.create_dirs)
                .await
                .map(|_| Empty {});
            to_ext_response(result)
        }
        "echo.agent/fs/delete_file" => {
            let req = parse_params::<FsDeleteFileRequest>(args)?;
            let path = resolve_path(agent, &req.path, req.session_id.as_ref())?;
            let (path, _) = confine_local(agent, &path, req.session_id.as_ref()).await?;
            let result = fs::delete_file(&path).await.map(|_| Empty {});
            to_ext_response(result)
        }
        _ => Err(acp::Error::method_not_found()),
    }
}
