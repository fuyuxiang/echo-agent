// todo: add support for signal handling
use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time;

use crate::runner::{AsyncTerminalRunner, TerminalError, TerminalRunRequest, TerminalRunResult};
use echo_agent_tty_utils::KILL_REAP_TIMEOUT;

pub struct LocalTerminalRunner;

/// A pipe reader whose buffer is shared with the spawner, so bytes read
/// before an abort are not lost with the task.
struct PipeReader {
    task: tokio::task::JoinHandle<()>,
    buffer: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl PipeReader {
    fn spawn(mut stream: impl AsyncReadExt + Unpin + Send + 'static) -> Self {
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let task_buffer = std::sync::Arc::clone(&buffer);
        let task = tokio::spawn(async move {
            let mut chunk = [0u8; 8192];
            loop {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => Self::lock_buffer(&task_buffer).extend_from_slice(&chunk[..n]),
                }
            }
        });
        Self { task, buffer }
    }

    /// Never panics: a poisoned lock just means a holder panicked mid-append —
    /// the buffer is plain bytes and stays consistent, so recover it.
    fn lock_buffer(buffer: &std::sync::Mutex<Vec<u8>>) -> std::sync::MutexGuard<'_, Vec<u8>> {
        buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn take_buffer(self) -> Vec<u8> {
        std::mem::take(&mut Self::lock_buffer(&self.buffer))
    }

    /// Wait for EOF (pipe closed by all writers) at most [`KILL_REAP_TIMEOUT`],
    /// then return everything read so far.
    ///
    /// Callers only join after the child was reaped or killed, so EOF is
    /// normally immediate; the bound is a backstop for writers that outlive
    /// the command and keep the pipe open — a background descendant
    /// (`printf hi; sleep 30 &`) or a process wedged in uninterruptible
    /// kernel I/O (D-state), which not even SIGKILL moves.
    async fn join_bounded(mut self) -> Vec<u8> {
        if time::timeout(KILL_REAP_TIMEOUT, &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
        }
        self.take_buffer()
    }
}

/// Truncate buffer to keep only the last `limit` bytes (drops oldest bytes).
/// Returns true if truncation occurred.
///
/// This function ensures we don't split UTF-8 characters when truncating
/// by using char_indices to find a valid character boundary.
fn truncate_buffer(buf: &mut Vec<u8>, limit: usize) -> bool {
    if buf.len() > limit {
        // Convert to string to work with character boundaries
        let s = String::from_utf8_lossy(buf);
        let excess = buf.len().saturating_sub(limit);

        // Find the first char boundary at or after `excess` bytes
        let start_idx = s
            .char_indices()
            .find(|(i, _)| *i >= excess)
            .map(|(i, _)| i)
            .unwrap_or(s.len());

        // Slice from that boundary and update buffer
        *buf = s[start_idx..].as_bytes().to_vec();

        true
    } else {
        false
    }
}

#[async_trait::async_trait]
impl AsyncTerminalRunner for LocalTerminalRunner {
    async fn run(&self, request: TerminalRunRequest) -> Result<TerminalRunResult, TerminalError> {
        // Build and spawn the command via the platform shell.
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new(crate::default_shell_path());
            c.arg("-lc").arg(&request.command);
            c
        };
        #[cfg(not(unix))]
        let mut cmd = {
            let inv = echo_agent_config::shell::shell_command_argv(&request.command);
            let mut c = Command::new(inv.program);
            c.args(&inv.args).envs(inv.env);
            c
        };
        cmd.current_dir(&request.cwd)
            .envs(&request.env)
            .envs(crate::pager_env())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Detach from the controlling terminal so child processes
        // (e.g. GPG pinentry) cannot open /dev/tty and corrupt the TUI.
        // (This also makes the child its own session/group leader, which the
        // ProcessGroup attach below relies on.)
        echo_agent_tools::util::detach_command(&mut cmd);

        #[allow(clippy::disallowed_methods)]
        // process-group-killed on the request timeout; an unreapable (D-state)
        // child is abandoned to the runtime's orphan reaper
        let mut child = cmd
            .spawn()
            .map_err(|e| TerminalError::Other(format!("Failed to start shell: {e}")))?;

        // Own the whole tree, not just the direct shell: on timeout the group
        // is killed so grandchildren can't keep running (and can't hold the
        // output pipes open past the kill). Same pattern as
        // `gateway_bridge::local_workspace_supervisor`. Group teardown is an
        // enhancement over the direct-kill floor, so both enrollment failures
        // degrade softly instead of dropping an already-running shell.
        let process_group = match echo_agent_tty_utils::ProcessGroup::new() {
            Ok(mut group) => {
                if let Err(e) = group.attach(&child) {
                    // e.g. the child already exited.
                    tracing::debug!("ProcessGroup::attach failed: {e}");
                }
                Some(group)
            }
            Err(e) => {
                tracing::warn!("Failed to create process group: {e}");
                None
            }
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TerminalError::Other("Failed to capture stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| TerminalError::Other("Failed to capture stderr".into()))?;

        let stdout_reader = PipeReader::spawn(stdout);
        let stderr_reader = PipeReader::spawn(stderr);

        let mut timed_out = false;

        let wait_result = time::timeout(request.timeout, child.wait()).await;
        let exit_code = match wait_result {
            Ok(status_res) => status_res
                .map_err(|e| TerminalError::Other(format!("Failed to wait for process: {e}")))?
                .code(),
            Err(_) => {
                timed_out = true;
                // Kill the direct child AND the whole group (grandchildren),
                // then return the synthetic timeout result without waiting
                // for the corpse: `timed_out: true` already tells the caller
                // everything, and a D-state child would never become reapable
                // anyway. Both kills are unconditional — `kill()` on a group
                // whose attach failed is a silent no-op, so the direct
                // `start_kill` is the guaranteed floor (same as
                // `util/subprocess.rs`). The kills close the pipes, so the
                // bounded joins below return immediately in the normal case;
                // the abandoned child goes to tokio's orphan reaper.
                if let Err(e) = child.start_kill() {
                    tracing::warn!("Failed to kill timed-out process: {e}");
                }
                if let Some(group) = &process_group
                    && let Err(e) = group.kill()
                {
                    tracing::warn!("Failed to kill timed-out process group: {e}");
                }
                None
            }
        };

        // Join concurrently (worst case one reap bound, not one per pipe).
        // Bounded on the natural-exit arm too: a background descendant that
        // outlives the shell (`printf hi; sleep 30 &`) holds the pipes open
        // past the child's exit, and previously stalled this wait unboundedly.
        let (stdout_result, stderr_result) =
            tokio::join!(stdout_reader.join_bounded(), stderr_reader.join_bounded());

        // Combine stdout and stderr, then truncate if needed
        let mut combined = stdout_result;
        combined.extend(stderr_result);
        let truncated = truncate_buffer(&mut combined, request.output_byte_limit);

        let combined_output = String::from_utf8_lossy(&combined).into_owned();

        Ok(TerminalRunResult {
            combined_output,
            exit_code,
            truncated,
            signal: None,
            timed_out,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_OUTPUT_BYTE_LIMIT;
    use crate::runner::TerminalRunRequest;
    use echo_agent_paths::AbsPathBuf;
    use std::collections::HashMap;

    fn make_request(command: &str) -> TerminalRunRequest {
        TerminalRunRequest {
            tool_call_id: agent_client_protocol::ToolCallId::new("test"),
            command: command.to_string(),
            cwd: AbsPathBuf::new(std::env::current_dir().unwrap()).unwrap(),
            env: HashMap::new(),
            timeout: std::time::Duration::from_secs(10),
            output_byte_limit: DEFAULT_OUTPUT_BYTE_LIMIT,
            stream: false,
            output_file: None,
        }
    }

    /// Verify that `detach_from_tty` prevents child processes from opening
    /// `/dev/tty`. After setsid(), the child has no controlling terminal.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_child_cannot_open_dev_tty() {
        // Skip in CI / environments without a controlling terminal.
        if std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .is_err()
        {
            eprintln!("skipping: no controlling terminal");
            return;
        }

        let result = LocalTerminalRunner
            .run(make_request(
                "(exec 3>/dev/tty && echo ATTACHED || echo DETACHED) 2>/dev/null",
            ))
            .await
            .unwrap();

        assert_eq!(
            result.combined_output.trim(),
            "DETACHED",
            "child process should not be able to open /dev/tty after detach_from_tty()"
        );
    }

    /// Basic regression: commands still produce output and exit normally.
    #[tokio::test]
    async fn test_basic_command_output() {
        let result = LocalTerminalRunner
            .run(make_request("echo hello"))
            .await
            .unwrap();

        assert_eq!(result.combined_output.trim(), "hello");
        assert_eq!(result.exit_code, Some(0));
    }

    /// Timing out a command whose grandchild inherited the output pipes must
    /// (a) return promptly — before the group kill, the joins waited for the
    /// grandchild's EOF (the full sleep here; forever for a writer wedged in
    /// uninterruptible kernel I/O, the production incident this guards
    /// against) — and (b) actually kill the grandchild via the group kill,
    /// not merely stop waiting for it.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_timeout_kills_grandchildren_and_returns_promptly() {
        let mut request = make_request("sleep 5 & echo bgpid=$!; sleep 5");
        request.timeout = std::time::Duration::from_millis(300);

        let started = std::time::Instant::now();
        let result = LocalTerminalRunner.run(request).await.unwrap();

        assert!(result.timed_out, "run should report the timeout");
        // Prompt return: request timeout (0.3s) + pipe EOF from the group
        // kill (normally instant; KILL_REAP_TIMEOUT-bounded worst case) —
        // anything near the 5s sleeps means we waited for the grandchild.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(4),
            "timeout path must not wait for the killed tree (took {:?})",
            started.elapsed()
        );

        // Outcome: the background grandchild must actually be dead, proving
        // the group kill reached it. Poll briefly — the SIGKILL is delivered
        // synchronously but init may reap the orphan a beat later.
        let bg_pid = result
            .combined_output
            .lines()
            .find_map(|l| l.trim().strip_prefix("bgpid="))
            .and_then(|p| p.trim().parse::<u32>().ok())
            .expect("background pid should have been echoed before the kill");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let alive = std::process::Command::new("kill")
                .args(["-0", &bg_pid.to_string()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !alive {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background grandchild {bg_pid} survived the group kill"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}
