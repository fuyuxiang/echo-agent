#[cfg(bundle_rg)]
use std::fs::{self, OpenOptions};
use std::io;
#[cfg(bundle_rg)]
use std::io::Write as _;
#[cfg(bundle_rg)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;

#[cfg(bundle_rg)]
const RG_BYTES: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/bundle-rg/rg-",
    env!("ECHO_AGENT_TOOLS_RG_VER"),
    "-",
    env!("ECHO_AGENT_TOOLS_RG_TARGET"),
    ".bin"
));

#[cfg(bundle_rg)]
fn embedded_rg_is_current(path: &Path) -> bool {
    use sha2::Digest as _;

    let Ok(existing) = fs::read(path) else {
        return false;
    };
    sha2::Sha256::digest(&existing) == sha2::Sha256::digest(RG_BYTES)
}

#[cfg(bundle_rg)]
fn resolve_bundled_rg() -> io::Result<PathBuf> {
    use fs2::FileExt as _;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let file_name = format!(
        "rg-{}-{}{}",
        env!("ECHO_AGENT_TOOLS_RG_VER"),
        env!("ECHO_AGENT_TOOLS_RG_TARGET"),
        if cfg!(windows) { ".exe" } else { "" },
    );
    let vendor_dir = crate::util::echo_agent_home().join("vendor");
    fs::create_dir_all(&vendor_dir)?;
    let p = vendor_dir.join(file_name);
    let lock_path = vendor_dir.join("rg-extract.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock_exclusive()?;

    if !embedded_rg_is_current(&p) {
        let tmp = p.with_extension(format!("tmp-{}", std::process::id()));
        let mut output = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        output.write_all(RG_BYTES)?;
        output.sync_all()?;
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&tmp)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&tmp, perms)?;
        }
        drop(output);
        if p.exists() {
            fs::remove_file(&p)?;
        }
        fs::rename(&tmp, &p)?;
    }
    Ok(p)
}

/// Get the path to the ripgrep executable.
///
/// In release builds with bundling enabled, this extracts the bundled ripgrep
/// binary to ~/.echo-agent/vendor/ and returns that path.
/// Otherwise, assumes `rg` is in PATH.
pub fn rg_path() -> io::Result<PathBuf> {
    static RG_EXEC: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    RG_EXEC
        .get_or_init(|| {
            // Runtime escape hatch for managed/offline environments. Resolve
            // it before the bundled branch so the recovery instruction also
            // works in packaged release clients.
            if let Ok(path) = std::env::var("RG_BIN_PATH")
                && !path.trim().is_empty()
            {
                return Ok(PathBuf::from(path));
            }
            #[cfg(bundle_rg)]
            {
                resolve_bundled_rg().map_err(|error| {
                    format!("failed to prepare the built-in ripgrep component: {error}")
                })
            }
            #[cfg(not(bundle_rg))]
            {
                // Some hermetic test runners set RUNFILES_DIR and ship rg as a
                // data dependency rather than on PATH. Scan for a directory
                // entry containing "ripgrep_hermetic" and prefer arch-scoped
                // paths when present.
                if let Ok(rf) = std::env::var("RUNFILES_DIR") {
                    let base = PathBuf::from(rf);
                    if let Ok(entries) = std::fs::read_dir(&base) {
                        for entry in entries.flatten() {
                            let name = entry.file_name();
                            if name.to_string_lossy().contains("ripgrep_hermetic") {
                                for sub in ["amd64/rg", "arm64/rg", "rg"] {
                                    let candidate = entry.path().join(sub);
                                    if candidate.exists() {
                                        return Ok(candidate);
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(PathBuf::from("rg"))
            }
        })
        .clone()
        .map_err(io::Error::other)
}

pub fn unavailable_message(error: &dyn std::fmt::Display) -> String {
    format!(
        "内置搜索组件（ripgrep）无法启动：{error}。请重启 EchoAgent；如果问题持续，请修复或重新安装客户端。\
         临时恢复可在启动客户端前将 RG_BIN_PATH 指向可用的 rg 可执行文件。"
    )
}

#[derive(Debug)]
pub struct BoundedRgOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BoundedRgError {
    #[error("{0}")]
    Spawn(#[source] io::Error),
    #[error("搜索超过 {0} 秒未完成")]
    Timeout(u64),
    #[error("读取搜索输出失败：{0}")]
    Io(#[source] io::Error),
}

impl BoundedRgError {
    pub fn into_tool_error(
        self,
        tool_id: echo_agent_tool_protocol::ToolId,
    ) -> echo_agent_tool_runtime::ToolError {
        match self {
            Self::Spawn(source) => echo_agent_tool_runtime::ToolError::service_unavailable(
                unavailable_message(&source),
            )
            .with_source(source),
            Self::Timeout(seconds) => echo_agent_tool_runtime::ToolError::timeout(
                tool_id,
                format!(
                    "搜索超过 {seconds} 秒未完成，已安全终止。请缩小搜索路径或使用更具体的匹配条件。"
                ),
            ),
            Self::Io(source) => echo_agent_tool_runtime::ToolError::execution(
                tool_id,
                format!("读取内置搜索组件输出失败：{source}"),
            )
            .with_source(source),
        }
    }
}

async fn read_limited<R>(reader: R, limit: usize) -> io::Result<(Vec<u8>, bool)>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    reader
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    Ok((bytes, truncated))
}

/// Execute a short-lived ripgrep command while draining both output pipes
/// concurrently. This prevents stderr backpressure from deadlocking stdout and
/// gives every non-streaming search implementation the same memory/time bounds.
pub async fn output_bounded(
    command: &mut Command,
    timeout: std::time::Duration,
    max_stdout: usize,
    max_stderr: usize,
) -> Result<BoundedRgOutput, BoundedRgError> {
    let mut child = command.spawn().map_err(BoundedRgError::Spawn)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| BoundedRgError::Io(io::Error::other("ripgrep stdout was not captured")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| BoundedRgError::Io(io::Error::other("ripgrep stderr was not captured")))?;
    let deadline = tokio::time::Instant::now() + timeout;
    let stdout_read = read_limited(stdout, max_stdout);
    let stderr_read = read_limited(stderr, max_stderr);
    tokio::pin!(stdout_read, stderr_read);
    let mut stdout_result = None;
    let mut stderr_result = None;
    let mut killed_for_limit = false;

    while stdout_result.is_none() || stderr_result.is_none() {
        tokio::select! {
            result = &mut stdout_read, if stdout_result.is_none() => {
                match result {
                    Ok(result) => {
                        if result.1 && !killed_for_limit {
                            let _ = child.start_kill();
                            killed_for_limit = true;
                        }
                        stdout_result = Some(result);
                    }
                    Err(error) => {
                        let _ = child.start_kill();
                        crate::util::reap_killed_search_child(&mut child).await;
                        return Err(BoundedRgError::Io(error));
                    }
                }
            }
            result = &mut stderr_read, if stderr_result.is_none() => {
                match result {
                    Ok(result) => {
                        if result.1 && !killed_for_limit {
                            let _ = child.start_kill();
                            killed_for_limit = true;
                        }
                        stderr_result = Some(result);
                    }
                    Err(error) => {
                        let _ = child.start_kill();
                        crate::util::reap_killed_search_child(&mut child).await;
                        return Err(BoundedRgError::Io(error));
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                let _ = child.start_kill();
                crate::util::reap_killed_search_child(&mut child).await;
                return Err(BoundedRgError::Timeout(timeout.as_secs()));
            }
        }
    }

    let (stdout, stdout_truncated) = stdout_result.expect("stdout reader completed");
    let (stderr, stderr_truncated) = stderr_result.expect("stderr reader completed");

    let exit_code = if stdout_truncated || stderr_truncated {
        if !killed_for_limit {
            let _ = child.start_kill();
        }
        crate::util::reap_killed_search_child(&mut child).await;
        if stderr_truncated { 2 } else { 0 }
    } else {
        match tokio::time::timeout_at(deadline, child.wait()).await {
            Ok(Ok(status)) => status.code().unwrap_or(-1),
            Ok(Err(error)) => return Err(BoundedRgError::Io(error)),
            Err(_) => {
                let _ = child.start_kill();
                crate::util::reap_killed_search_child(&mut child).await;
                return Err(BoundedRgError::Timeout(timeout.as_secs()));
            }
        }
    };

    Ok(BoundedRgOutput {
        stdout,
        stderr,
        exit_code,
        stdout_truncated,
        stderr_truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    #[tokio::test]
    async fn missing_binary_is_a_spawn_error_not_an_empty_success() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let mut command = Command::new(temp.path().join("definitely-missing-rg"));
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        let error = output_bounded(&mut command, std::time::Duration::from_secs(1), 1024, 1024)
            .await
            .expect_err("missing executable must fail");

        assert!(matches!(error, BoundedRgError::Spawn(_)));
        assert!(unavailable_message(&error).contains("RG_BIN_PATH"));
    }
}
