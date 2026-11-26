use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

// Canonical in echo-agent-workspace-types; re-exported for existing paths.
pub use echo_agent_workspace_types::rpc::search::{
    ContentMatch, ContentMatchFile, ContentSearchData,
};

#[derive(Debug, Clone, Default)]
pub struct ContentSearchParams {
    pub pattern: String,
    pub case_insensitive: bool,
    pub literal: bool,
    pub globs: Vec<String>,
    pub max_files: Option<usize>,
    pub max_matches: Option<usize>,
    pub respect_gitignore: bool,
}

/// Batch of results sent during streaming search.
#[derive(Debug, Clone, Default)]
pub struct ContentSearchBatch {
    pub files: Vec<ContentMatchFile>,
    pub total_matches: usize,
    pub total_files: usize,
    pub done: bool,
    pub truncated: bool,
}

const BATCH_INTERVAL_MS: u64 = 50;
const DEFAULT_MAX_FILES: usize = 100;
const DEFAULT_MAX_MATCHES: usize = 1000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STDERR_BYTES: usize = 256_000;

fn build_ripgrep_command(root: &Path, params: &ContentSearchParams) -> anyhow::Result<Command> {
    let rg_path = crate::util::ripgrep::rg_path().map_err(|error| {
        anyhow::anyhow!(
            "{}",
            echo_agent_tools::implementations::echo_agent_build::grep::ripgrep::unavailable_message(
                &error
            )
        )
    })?;

    let mut cmd = Command::new(&rg_path);
    cmd.current_dir(root);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    echo_agent_tty_utils::detach_search_command(&mut cmd);

    cmd.arg("--json");
    cmd.arg("--line-number");

    const DEFAULT_EXCLUSIONS: &[&str] = &["!.git/**", "!submodules/**", "!vendor/**"];
    for glob in DEFAULT_EXCLUSIONS {
        cmd.arg("--glob").arg(glob);
    }

    cmd.arg("--max-filesize").arg("1M");
    cmd.arg("--max-count").arg("50");
    cmd.arg("--max-columns").arg("500");
    cmd.arg("--max-columns-preview");

    if params.case_insensitive {
        cmd.arg("--ignore-case");
    }
    if params.literal {
        cmd.arg("--fixed-strings");
    }
    if !params.respect_gitignore {
        cmd.arg("--no-ignore");
    }
    for glob in &params.globs {
        cmd.arg("--glob").arg(glob);
    }

    cmd.arg("-e").arg(&params.pattern);
    cmd.arg(".");

    Ok(cmd)
}

fn extract_match_positions(data: &serde_json::Value) -> (Option<usize>, Option<usize>) {
    data.get("submatches")
        .and_then(|s| s.as_array())
        .and_then(|arr| arr.first())
        .map(|first| {
            let start = first
                .get("start")
                .and_then(|s| s.as_u64())
                .map(|s| s as usize);
            let end = first
                .get("end")
                .and_then(|e| e.as_u64())
                .map(|e| e as usize);
            (start, end)
        })
        .unwrap_or((None, None))
}

fn parse_match_from_json(data: &serde_json::Value) -> Option<ContentMatch> {
    let line_number = data.get("line_number").and_then(|l| l.as_u64())? as usize;
    let content = data
        .get("lines")
        .and_then(|l| l.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim_end_matches('\n')
        .to_string();
    let (match_start, match_end) = extract_match_positions(data);

    Some(ContentMatch {
        line: line_number,
        content,
        match_start,
        match_end,
    })
}

fn parse_file_path_from_json(root: &Path, json: &serde_json::Value) -> Option<String> {
    let path = json
        .get("data")
        .and_then(|d| d.get("path"))
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())?;
    let normalized = path.strip_prefix("./").unwrap_or(path);
    if Path::new(normalized).is_absolute() {
        return Some(normalized.to_string());
    }
    Some(root.join(normalized).to_string_lossy().to_string())
}

/// Streaming content search with batched status notifications. Cancellation
/// is dropping the future: the spawn config kills rg on drop.
pub async fn content_search_streaming<F>(
    root: &Path,
    params: &ContentSearchParams,
    on_status: F,
) -> anyhow::Result<ContentSearchData>
where
    F: Fn(ContentSearchBatch) + Send + 'static,
{
    let max_files = params.max_files.unwrap_or(DEFAULT_MAX_FILES);
    let max_matches = params.max_matches.unwrap_or(DEFAULT_MAX_MATCHES);

    let mut cmd = build_ripgrep_command(root, params)?;
    #[allow(clippy::disallowed_methods)] // waited on below; killed on drop (cancellation)
    let mut child = cmd.spawn().map_err(|error| {
        anyhow::anyhow!(
            "{}",
            echo_agent_tools::implementations::echo_agent_build::grep::ripgrep::unavailable_message(
                &error
            )
        )
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture ripgrep stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture ripgrep stderr"))?;
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::with_capacity(MAX_STDERR_BYTES.min(64 * 1024));
        let mut stderr = stderr;
        let mut chunk = [0_u8; 16 * 1024];
        let mut truncated = false;
        loop {
            let read = stderr.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            let remaining = MAX_STDERR_BYTES.saturating_sub(bytes.len());
            if read > remaining {
                truncated = true;
            }
            if remaining > 0 {
                bytes.extend_from_slice(&chunk[..read.min(remaining)]);
            }
        }
        Ok::<_, std::io::Error>((bytes, truncated))
    });
    let deadline = tokio::time::Instant::now() + COMMAND_TIMEOUT;

    let mut reader = BufReader::new(stdout).lines();
    let mut files: Vec<ContentMatchFile> = Vec::new();
    let mut current_file: Option<ContentMatchFile> = None;
    let mut total_matches = 0usize;
    let mut pending_files: Vec<ContentMatchFile> = Vec::new();
    let mut last_notify = Instant::now();
    let mut hit_limit = false;

    loop {
        let line = match tokio::time::timeout_at(deadline, reader.next_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                let _ = child.start_kill();
                echo_agent_tools::util::reap_killed_search_child(&mut child).await;
                let _ = stderr_task.await;
                return Err(anyhow::anyhow!("读取搜索结果失败：{error}"));
            }
            Err(_) => {
                let _ = child.start_kill();
                echo_agent_tools::util::reap_killed_search_child(&mut child).await;
                let _ = stderr_task.await;
                return Err(anyhow::anyhow!(
                    "搜索超过 {} 秒未完成，已安全终止。请缩小搜索路径或使用更具体的匹配条件。",
                    COMMAND_TIMEOUT.as_secs()
                ));
            }
        };
        if line.is_empty() {
            continue;
        }

        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match json.get("type").and_then(|t| t.as_str()) {
            Some("begin") => {
                if let Some(file) = current_file.take()
                    && !file.matches.is_empty()
                {
                    pending_files.push(file.clone());
                    files.push(file);
                }
                if let Some(path) = parse_file_path_from_json(root, &json) {
                    current_file = Some(ContentMatchFile::new(path));
                }
            }
            Some("match") => {
                if let Some(ref mut file) = current_file
                    && let Some(data) = json.get("data")
                    && let Some(m) = parse_match_from_json(data)
                {
                    file.matches.push(m);
                    total_matches += 1;
                }
            }
            Some("end") => {
                if let Some(file) = current_file.take()
                    && !file.matches.is_empty()
                {
                    pending_files.push(file.clone());
                    files.push(file);
                }
            }
            _ => {}
        }

        if files.len() >= max_files || total_matches >= max_matches {
            hit_limit = true;
            break;
        }

        let should_notify = !pending_files.is_empty()
            && last_notify.elapsed().as_millis() >= BATCH_INTERVAL_MS as u128;

        if should_notify {
            on_status(ContentSearchBatch {
                files: std::mem::take(&mut pending_files),
                total_matches,
                total_files: files.len(),
                done: false,
                truncated: false,
            });
            tokio::task::yield_now().await;
            last_notify = Instant::now();
        }
    }

    let exit_code = if hit_limit {
        let _ = child.start_kill();
        // Bounded reap: a D-state rg must not stall this future forever.
        echo_agent_tools::util::reap_killed_search_child(&mut child).await;
        0
    } else {
        match tokio::time::timeout_at(deadline, child.wait()).await {
            Ok(Ok(status)) => status.code().unwrap_or(-1),
            Ok(Err(error)) => {
                let _ = stderr_task.await;
                return Err(anyhow::anyhow!("等待搜索进程结束失败：{error}"));
            }
            Err(_) => {
                let _ = child.start_kill();
                echo_agent_tools::util::reap_killed_search_child(&mut child).await;
                let _ = stderr_task.await;
                return Err(anyhow::anyhow!(
                    "搜索超过 {} 秒未完成，已安全终止。请缩小搜索路径或使用更具体的匹配条件。",
                    COMMAND_TIMEOUT.as_secs()
                ));
            }
        }
    };
    let (stderr, stderr_truncated) = stderr_task
        .await
        .map_err(|error| anyhow::anyhow!("收集搜索错误信息失败：{error}"))?
        .map_err(|error| anyhow::anyhow!("读取搜索错误信息失败：{error}"))?;
    if !hit_limit && (exit_code < 0 || exit_code > 1) {
        let stderr = String::from_utf8_lossy(&stderr);
        let suffix = if stderr_truncated {
            "…（已截断）"
        } else {
            ""
        };
        let detail = if stderr.trim().is_empty() {
            "未返回详细错误信息"
        } else {
            stderr.trim()
        };
        return Err(anyhow::anyhow!(
            "ripgrep 搜索失败（退出码 {exit_code}）：{detail}{suffix}"
        ));
    }

    if let Some(file) = current_file
        && !file.matches.is_empty()
        && files.len() < max_files
    {
        pending_files.push(file.clone());
        files.push(file);
    }

    let truncated = hit_limit;
    let total_files = files.len();

    on_status(ContentSearchBatch {
        files: pending_files,
        total_matches,
        total_files,
        done: true,
        truncated,
    });
    tokio::task::yield_now().await;

    Ok(ContentSearchData {
        files,
        total_matches,
        total_files,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cancellation is dropping the future; commands from
    /// `build_ripgrep_command` must kill rg on drop.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_spawned_search_child_kills_rg() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Overflow the stdout pipe (rg caps 50 matches/file, so use many files)
        // so rg blocks on write and stays alive until killed.
        let line = format!("needle {}\n", "x".repeat(120));
        for i in 0..200 {
            std::fs::write(tmp.path().join(format!("f{i}.txt")), line.repeat(50)).unwrap();
        }

        let params = ContentSearchParams {
            pattern: "needle".to_string(),
            ..Default::default()
        };
        let mut cmd = build_ripgrep_command(tmp.path(), &params).expect("build rg command");
        // rg is hermetic under Bazel and on PATH locally; spawn failure is a real bug.
        #[allow(clippy::disallowed_methods)] // test child, killed on drop below
        let mut child = cmd.spawn().expect("spawn rg");
        let pid = child.id().expect("child pid");

        // Hold the read end open (no EPIPE death) and drop the child mid-run.
        let stdout_pipe = child.stdout.take();
        drop(child);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !echo_agent_tty_utils::process_not_running(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "rg (pid {pid}) still running 5s after its Child was dropped — leaked"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        drop(stdout_pipe);
    }
}
