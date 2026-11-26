//! Multi-client leader cluster: one shared leader, N pager clients, plus
//! inspection of the durable session log so reattach tests can assert on
//! what actually persisted.
//!
//! Every other leader test is single-client/single-leader; [`LeaderCluster`]
//! is the missing abstraction for "one leader, several pager clients sharing
//! its session". One [`ContentController`] gives one shared `$HOME` (hence one
//! elected leader) plus a fixed leader socket beneath its `ECHO_AGENT_HOME`; clients
//! spawn with the `--leader`/`--leader-socket` flags so they all attach to the
//! SAME leader. It also exposes the leader's durable `updates.jsonl` log so a
//! reattach test can assert on the persisted, replayable turn-completion
//! records — the genuine end-to-end signal behind durable turn completion.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::{ContentController, PtyHarness, pager_binary};

/// One shared mock-backed leader plus the pager clients attached to it.
pub struct LeaderCluster {
    content: ContentController,
    binary: PathBuf,
    socket: PathBuf,
    rows: u16,
    cols: u16,
}

impl LeaderCluster {
    /// Start the cluster: one [`ContentController`] (one shared `$HOME` =>
    /// one leader) and a fixed leader socket under its `ECHO_AGENT_HOME`.
    pub async fn start(rows: u16, cols: u16) -> Result<Self> {
        let content = ContentController::start()
            .await
            .context("start content controller")?;
        // One shared ECHO_AGENT_HOME => one leader; the socket lives beneath it so
        // every client (sharing the same env) elects/attaches to the same one.
        let echo_agent_home = content.home().join(".echo-agent");
        std::fs::create_dir_all(&echo_agent_home).context("create echoagent home")?;
        let socket = echo_agent_home.join("leader-e2e.sock");
        let binary = pager_binary().context("resolve pager binary")?;
        Ok(Self {
            content,
            binary,
            socket,
            rows,
            cols,
        })
    }

    /// Spawn the leader-electing client (`--leader --leader-socket <S>` plus
    /// `extra_args`); it starts a fresh session and brings up the leader.
    pub fn spawn_leader(&self, extra_args: &[&str]) -> Result<PtyHarness> {
        self.spawn_client(&[], extra_args)
    }

    /// Attach another client that resumes the shared session through the SAME
    /// leader (`--leader --leader-socket <S> --resume` plus `extra_args`).
    pub fn attach(&self, extra_args: &[&str]) -> Result<PtyHarness> {
        self.spawn_client(&["--resume"], extra_args)
    }

    /// Spawn a client wired to the shared leader socket. `mode_args` carries
    /// the per-role flag (`--resume` for attachers); `extra_args` is the
    /// caller's.
    fn spawn_client(&self, mode_args: &[&str], extra_args: &[&str]) -> Result<PtyHarness> {
        let socket = self.socket.to_str().context("socket path is utf-8")?;
        let mut args: Vec<&str> = vec!["--leader", "--leader-socket", socket];
        args.extend_from_slice(mode_args);
        args.extend_from_slice(extra_args);
        PtyHarness::spawn_with_content(&self.binary, self.rows, self.cols, &self.content, &args)
            .context("spawn pager client on shared leader")
    }

    /// The shared content controller (mock inference server + sandbox env).
    pub fn content(&self) -> &ContentController {
        &self.content
    }

    /// The cluster's sessions root: `ECHO_AGENT_HOME/sessions` (layout below is
    /// `sessions/<encoded-cwd>/<session-id>/updates.jsonl`).
    fn sessions_dir(&self) -> PathBuf {
        self.content.home().join(".echo-agent").join("sessions")
    }

    /// The session-update payload of every record across every `updates.jsonl`
    /// under the cluster's [`sessions_dir`](Self::sessions_dir) — i.e. the
    /// `params.update` object of each persisted envelope line, so a caller can
    /// match on its `sessionUpdate` tag directly. Scans ALL sessions under the
    /// cluster (fine for the single-session clusters these tests build).
    ///
    /// Infallible by design: a file that vanishes mid-walk, or whose appended
    /// tail tore across a multi-byte UTF-8 boundary (so `read_to_string`
    /// fails), is skipped for this call and picked up on the next one.
    pub fn session_updates(&self) -> Vec<Value> {
        let mut files = Vec::new();
        collect_updates_files(&self.sessions_dir(), &mut files);
        let mut out = Vec::new();
        for file in files {
            // Skip a vanished file or a torn multi-byte tail; the next poll retries.
            if let Ok(text) = std::fs::read_to_string(&file) {
                out.extend(parse_update_payloads(&text));
            }
        }
        out
    }

    /// Poll [`session_updates`](Self::session_updates) until a record with
    /// `sessionUpdate == "turn_completed"` appears, returning that (inner)
    /// update payload, or error on timeout. Scans ALL sessions under the
    /// cluster (fine for the single-session clusters these tests build).
    pub fn wait_for_turn_completed(&self, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let updates = self.session_updates();
            if let Some(rec) = updates.iter().find(|u| is_turn_completed(u)) {
                return Ok(rec.clone());
            }
            if Instant::now() >= deadline {
                // Surface what WAS persisted so "zero records / env problem" is
                // distinguishable from "records present but no turn_completed /
                // producer regression".
                let tags: std::collections::BTreeSet<&str> = updates
                    .iter()
                    .filter_map(|u| u.get("sessionUpdate").and_then(Value::as_str))
                    .collect();
                anyhow::bail!(
                    "timed out after {timeout:?} waiting for a turn_completed record under {}; \
                     saw {} update record(s) with sessionUpdate tags {tags:?}",
                    self.sessions_dir().display(),
                    updates.len(),
                );
            }
            // Sync FS poll mirrors the harness's blocking wait_for_text; a stat
            // every 150ms is cheap and fine on a multi_thread runtime worker.
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

/// Whether a session-update payload is a `turn_completed` terminal.
fn is_turn_completed(update: &Value) -> bool {
    update.get("sessionUpdate").and_then(Value::as_str) == Some("turn_completed")
}

/// Parse the `params.update` payload out of each non-blank line of an
/// `updates.jsonl` body, assuming the enveloped on-disk shape current sessions
/// always write (`{..,"params":{"update":{..}}}`). A line that is blank, fails
/// to parse (a torn trailing line that is still valid UTF-8), or carries no
/// `params.update` is skipped — never failing the batch. (A torn *multi-byte*
/// tail instead fails the file read upstream, skipping the whole file for that
/// poll; see [`LeaderCluster::session_updates`].)
fn parse_update_payloads(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let envelope: Value = serde_json::from_str(line).ok()?;
            envelope.get("params")?.get("update").cloned()
        })
        .collect()
}

/// Recursively collect every `updates.jsonl` beneath `dir` (a manual walk to
/// avoid a new crate dep). A missing/unreadable dir yields nothing — sessions
/// may not exist yet, and the walk is re-run on every poll.
fn collect_updates_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // No-follow file type: a symlinked directory has `is_dir() == false`,
        // so a symlink cycle can never recurse forever here.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_updates_files(&path, out);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("updates.jsonl") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrap a session update as the envelope stored in `updates.jsonl`.
    fn envelope(update_json: &str) -> String {
        format!(
            r#"{{"timestamp":1,"method":"_echo.agent/session/update","params":{{"sessionId":"s","update":{update_json}}}}}"#
        )
    }

    #[test]
    fn parse_update_payloads_unwraps_and_tolerates_torn_trailing_line() {
        let body = format!(
            "{}\n{}\n{{\"timestamp\":2,\"method\":\"_echo.agent/sess",
            envelope(
                r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}"#
            ),
            envelope(
                r#"{"sessionUpdate":"turn_completed","prompt_id":"p-1","stop_reason":"end_turn"}"#
            ),
        );
        let updates = parse_update_payloads(&body);
        // The two complete lines parse; the torn final line is dropped.
        assert_eq!(updates.len(), 2);
        let completed = updates
            .iter()
            .find(|u| is_turn_completed(u))
            .expect("turn_completed payload is unwrapped from params.update");
        assert_eq!(completed["stop_reason"], "end_turn");
        assert_eq!(completed["prompt_id"], "p-1");
    }

    #[test]
    fn parse_update_payloads_skips_blank_and_payloadless_lines() {
        let body = format!(
            "\n   \n{}\n{{\"timestamp\":3,\"method\":\"x\",\"params\":{{\"sessionId\":\"s\"}}}}\n",
            envelope(
                r#"{"sessionUpdate":"turn_completed","prompt_id":"p","stop_reason":"cancelled"}"#
            ),
        );
        let updates = parse_update_payloads(&body);
        // Only the one envelope carrying params.update survives.
        assert_eq!(updates.len(), 1);
        assert!(is_turn_completed(&updates[0]));
        assert_eq!(updates[0]["stop_reason"], "cancelled");
    }
}
