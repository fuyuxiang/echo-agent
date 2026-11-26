//! Agent-scoped interactive PTY manager. PTYs are keyed by `terminalId`,
//! outlive sessions, and multiplex I/O over the existing ACP WebSocket.
//! Shells enroll in the process-global scope as terminal owners, so teardown
//! hangs a live shell up and it forwards that to its jobs. A shell that exits
//! on its own leaves them running, as a terminal does: their process groups are
//! their own and nothing here holds a handle to them.

// A panic here loses a shell: teardown paths run inside `Drop`, where an
// unwind during another unwind aborts the process. Tests panic freely.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::{Arc, LazyLock};

use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{Mutex, mpsc};

use echo_agent_workspace::file_system::TargetClientId;

use crate::{TerminalExtError, TerminalInfo, TerminalStatus};

/// Inject `targetClientId` into `_meta` and fire-and-forget an ext notification.
/// Mirrors `echo_agent_runtime::extensions::routing::send_routed_notification` so
/// this crate does not depend on the shell.
fn send_routed_notification(
    gateway: &GatewaySender,
    method: &str,
    mut params: serde_json::Value,
    target_client_id: &TargetClientId,
) {
    if !target_client_id.is_none() {
        let meta = params.as_object_mut().and_then(|obj| {
            obj.entry("_meta")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
        });
        if let Some(meta) = meta
            && let Ok(val) = serde_json::to_value(target_client_id)
        {
            meta.insert("targetClientId".to_string(), val);
        }
    }
    if let Ok(raw) = serde_json::value::to_raw_value(&params) {
        gateway.forward_fire_and_forget(agent_client_protocol::ExtNotification::new(
            method,
            raw.into(),
        ));
    }
}

const NOTIFICATION_METHOD: &str = "echo.agent/terminal/pty/notification";
const OUTPUT_RING_BUFFER_SIZE: usize = 256 * 1024;
const OUTPUT_BATCH_INTERVAL_MS: u64 = 16;
const BUSY_POLL_INTERVAL_MS: u64 = 500;
const INPUT_CHANNEL_CAPACITY: usize = 256;
const EXIT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
/// Collecting a killed shell, not waiting on a live one.
const REAP_GRACE: std::time::Duration = std::time::Duration::from_millis(100);

pub struct PtySession {
    master: Option<Box<dyn MasterPty + Send>>,
    /// Taken at teardown so the writer loop ends and releases its master dup.
    input_tx: Option<mpsc::Sender<Vec<u8>>>,
    output_offset: u64,
    output_ring: VecDeque<u8>,
    shell: Shell,
    cwd: Option<String>,
    name: Option<String>,
    created_at: u64,
    rows: u16,
    cols: u16,
    target_client_id: TargetClientId,
    busy: bool,
    gateway: GatewaySender,
}

/// A shell and the group that can signal it.
///
/// Reaping releases the pid, and a released pid can be recycled, so a signal
/// after the reap may reach a stranger. `Reaped` carries no group, which makes
/// that mistake unrepresentable rather than a rule to remember.
enum Shell {
    Running {
        child: Box<dyn portable_pty::Child + Send + Sync>,
        group: Option<Arc<echo_agent_tty_utils::ProcessGroup>>,
    },
    Reaped(Option<portable_pty::ExitStatus>),
}

impl Shell {
    fn pid(&self) -> Option<u32> {
        match self {
            Shell::Running { child, .. } => child.process_id(),
            Shell::Reaped(_) => None,
        }
    }

    /// Reports whether a hangup was sent.
    fn hangup(&self) -> bool {
        match self {
            Shell::Running {
                group: Some(group), ..
            } if group.wants_hangup() => {
                let _ = group.hangup();
                true
            }
            _ => false,
        }
    }

    fn attach_group(&mut self, enrolled: Arc<echo_agent_tty_utils::ProcessGroup>) {
        if let Shell::Running { group, .. } = self {
            *group = Some(enrolled);
        }
    }

    /// Reap a shell that never reached the registry, where teardown would
    /// otherwise never find it. Same order as [`reap`], and it has to wait:
    /// `killpg` returns before the kernel has zombified the leader, so dropping
    /// straight after would leak it and retire the group with it.
    fn reap_now(&mut self) {
        if self.hangup() && self.wait_exit(echo_agent_tty_utils::HANGUP_GRACE) {
            return;
        }
        self.kill();
        self.wait_exit(REAP_GRACE);
    }

    /// Poll to a deadline. [`wait_for_exit`] is the same wait for a session that
    /// reached the registry, where each turn has to retake the lock.
    fn wait_exit(&mut self, budget: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + budget;
        loop {
            if self.poll_exit() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(EXIT_POLL_INTERVAL);
        }
    }

    /// The shell leads its group, so one non-blocking `killpg` reaches the tree.
    fn kill(&mut self) {
        match self {
            Shell::Running {
                group: Some(group), ..
            } => {
                let _ = group.kill();
            }
            // No pid to enroll, so the child handle is all there is.
            Shell::Running { child, .. } => {
                let _ = child.kill();
            }
            Shell::Reaped(_) => {}
        }
    }

    /// Whether the shell is gone. An error counts as gone: the pid is not ours
    /// to signal either way, and leaving the group attached would let teardown
    /// signal a recycled one.
    fn poll_exit(&mut self) -> bool {
        if let Shell::Running { child, .. } = self {
            match child.try_wait() {
                Ok(None) => return false,
                Ok(Some(status)) => *self = Shell::Reaped(Some(status)),
                Err(_) => *self = Shell::Reaped(None),
            }
        }
        true
    }

    fn exit_code(&self) -> Option<i32> {
        match self {
            Shell::Reaped(Some(status)) => Some(status.exit_code() as i32),
            _ => None,
        }
    }
}

/// A shell not yet in the registry, where teardown would never find it.
/// Dropping reaps it; [`Self::into_registered`] hands it to the registry
/// instead.
///
/// Disarming leaves [`Shell::Reaped`] behind rather than an empty slot, so the
/// guard has no state in which its own field is missing.
struct UnregisteredShell(Shell);

impl UnregisteredShell {
    fn new(child: Box<dyn portable_pty::Child + Send + Sync>) -> Self {
        Self(Shell::Running { child, group: None })
    }

    fn pid(&self) -> Option<u32> {
        self.0.pid()
    }

    fn attach_group(&mut self, enrolled: Arc<echo_agent_tty_utils::ProcessGroup>) {
        self.0.attach_group(enrolled);
    }

    /// Disarms the guard. The caller must reach the registry without awaiting,
    /// or it reopens the window this type closes.
    fn into_registered(mut self) -> Shell {
        self.disarm()
    }

    fn disarm(&mut self) -> Shell {
        std::mem::replace(&mut self.0, Shell::Reaped(None))
    }
}

impl Drop for UnregisteredShell {
    fn drop(&mut self) {
        let mut shell = self.disarm();
        // `reap_now` blocks through its grace waits, so keep it off an async
        // thread. Not the runtime's blocking pool though: a task still queued
        // there at shutdown is dropped unrun, taking the group with it and
        // leaving the scope holding a dead `Weak`.
        if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::spawn(move || shell.reap_now());
        } else {
            shell.reap_now();
        }
    }
}

type PtyMap = HashMap<String, Arc<Mutex<PtySession>>>;

static PTY_REGISTRY: LazyLock<Mutex<PtyMap>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub async fn get_pty(pty_id: &str) -> Option<Arc<Mutex<PtySession>>> {
    PTY_REGISTRY.lock().await.get(pty_id).cloned()
}

pub(crate) async fn require_pty(
    terminal_id: &str,
) -> Result<Arc<Mutex<PtySession>>, TerminalExtError> {
    get_pty(terminal_id)
        .await
        .ok_or_else(|| TerminalExtError::NotInteractive {
            terminal_id: terminal_id.into(),
        })
}

pub async fn create_pty(
    shell: Option<&str>,
    cwd: Option<&str>,
    env: HashMap<String, String>,
    rows: u16,
    cols: u16,
    name: Option<&str>,
    gateway: GatewaySender,
    target_client_id: TargetClientId,
) -> Result<String, TerminalExtError> {
    let pty_id = uuid::Uuid::now_v7().to_string();

    let pty_system = native_pty_system();
    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pair = pty_system
        .openpty(size)
        .map_err(|e| TerminalExtError::Internal(format!("failed to open pty: {e}")))?;

    let (shell_path, shell_args) = resolve_pty_shell(shell);

    let mut cmd = CommandBuilder::new(&shell_path);
    for arg in &shell_args {
        cmd.arg(arg);
    }

    if let Some(dir) = cwd {
        cmd.cwd(dir);
    } else if let Ok(dir) = std::env::current_dir() {
        cmd.cwd(dir);
    }

    for (k, v) in &env {
        cmd.env(k, v);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("LANG", "en_US.UTF-8");
    cmd.env("LC_ALL", "en_US.UTF-8");

    // Enrolled below, and reaped by the guard until it reaches the registry.
    #[allow(clippy::disallowed_methods)]
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| TerminalExtError::Internal(format!("failed to spawn shell: {e}")))?;

    let mut shell = UnregisteredShell::new(child);
    if let Some(pid) = shell.pid() {
        // `enroll_terminal_pid` reaps with a grace wait if it loses the close
        // race, so run it off-task.
        let enrolled = tokio::task::spawn_blocking(move || {
            echo_agent_tty_utils::global_process_scope().enroll_terminal_pid(pid)
        })
        .await
        .map_err(|e| TerminalExtError::Internal(format!("enroll task failed: {e}")))?
        .map_err(|e| TerminalExtError::Internal(format!("failed to enroll shell: {e}")))?;
        shell.attach_group(enrolled);
    }

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| TerminalExtError::Internal(format!("failed to clone pty reader: {e}")))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| TerminalExtError::Internal(format!("failed to take pty writer: {e}")))?;

    let (input_tx, input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
    spawn_pty_input_loop(writer, input_rx);

    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let resolved_cwd = cwd.map(|s| s.to_string()).or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    });

    let resolved_name = name.map(|s| s.to_string()).or_else(|| {
        // Default to cwd basename, fall back to shell basename.
        resolved_cwd
            .as_deref()
            .and_then(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .or_else(|| {
                std::path::Path::new(&shell_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
    });

    // Taking the lock first means nothing can await between disarming the guard
    // and the insert that gives teardown another way to reach the shell.
    let mut registry = PTY_REGISTRY.lock().await;

    // The scope can close during the setup above, and teardown has already run
    // by then: publishing here would advertise a shell it just killed.
    if echo_agent_tty_utils::global_process_scope().is_closed() {
        return Err(TerminalExtError::Internal(
            "process scope closed while the shell was starting".to_string(),
        ));
    }

    let entry = Arc::new(Mutex::new(PtySession {
        master: Some(pair.master),
        input_tx: Some(input_tx),
        output_offset: 0,
        output_ring: VecDeque::with_capacity(OUTPUT_RING_BUFFER_SIZE),
        shell: shell.into_registered(),
        cwd: resolved_cwd,
        name: resolved_name,
        created_at,
        rows,
        cols,
        target_client_id,
        busy: false,
        gateway: gateway.clone(),
    }));
    registry.insert(pty_id.clone(), entry.clone());
    drop(registry);

    let pty_id_clone = pty_id.clone();
    tokio::task::spawn_local(run_pty_output_loop(reader, entry, pty_id_clone, gateway));

    Ok(pty_id)
}

fn spawn_pty_input_loop(mut writer: Box<dyn Write + Send>, mut input_rx: mpsc::Receiver<Vec<u8>>) {
    tokio::task::spawn_blocking(move || {
        while let Some(mut chunk) = input_rx.blocking_recv() {
            while let Ok(more) = input_rx.try_recv() {
                chunk.extend_from_slice(&more);
            }
            if writer.write_all(&chunk).is_err() {
                break;
            }
            if writer.flush().is_err() {
                break;
            }
        }
    });
}

/// Reads PTY output, batches on a 16ms tick, and sends notifications.
/// Also samples the foreground process group on a slower tick and pushes
/// `process_started`/`process_ended` on idle↔busy transitions — time-based
/// rather than output-driven because a busy process can be silent.
async fn run_pty_output_loop(
    reader: Box<dyn Read + Send>,
    pty: Arc<Mutex<PtySession>>,
    pty_id: String,
    gateway: GatewaySender,
) {
    use tokio::sync::mpsc;
    use tokio::time::{Duration, interval};

    let (data_tx, mut data_rx) = mpsc::channel::<Vec<u8>>(64);

    tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if data_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut pending = Vec::new();
    let mut tick = interval(Duration::from_millis(OUTPUT_BATCH_INTERVAL_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut busy_tick = interval(Duration::from_millis(BUSY_POLL_INTERVAL_MS));
    busy_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            chunk = data_rx.recv() => {
                match chunk {
                    Some(data) => {
                        {
                            let mut session = pty.lock().await;
                            session.output_ring.extend(data.iter().copied());
                            if session.output_ring.len() > OUTPUT_RING_BUFFER_SIZE {
                                let excess = session.output_ring.len() - OUTPUT_RING_BUFFER_SIZE;
                                session.output_ring.drain(..excess);
                            }
                            session.output_offset += data.len() as u64;
                        }
                        pending.extend_from_slice(&data);
                    }
                    None => {
                        if !pending.is_empty() {
                            flush_output(&pty, &pty_id, &mut pending, &gateway).await;
                        }
                        break;
                    }
                }
            }

            _ = tick.tick() => {
                if !pending.is_empty() {
                    flush_output(&pty, &pty_id, &mut pending, &gateway).await;
                }
            }

            _ = busy_tick.tick() => {
                sample_busy_transition(&pty, &pty_id).await;
            }
        }
    }

    // The reader ending does not prove the shell did, so poll rather than
    // blocking on `wait`: teardown needs this lock to hang the shell up.
    let (exit_code, signal, target_client_id, was_busy) = tokio::task::spawn_blocking({
        let pty = pty.clone();
        move || {
            loop {
                {
                    let mut session = pty.blocking_lock();
                    let target_client_id = session.target_client_id.clone();
                    let was_busy = session.busy;
                    if session.shell.poll_exit() {
                        return (
                            session.shell.exit_code(),
                            None::<String>,
                            target_client_id,
                            was_busy,
                        );
                    }
                }
                std::thread::sleep(EXIT_POLL_INTERVAL);
            }
        }
    })
    .await
    .unwrap_or_default();

    if was_busy {
        send_busy_notification(&pty_id, false, &target_client_id, &gateway);
    }

    send_routed_notification(
        &gateway,
        NOTIFICATION_METHOD,
        serde_json::json!({
            "terminalId": pty_id,
            "type": "exit",
            "exitCode": exit_code,
            "signal": signal,
        }),
        &target_client_id,
    );
}

/// Whether a command is running, rather than the shell sitting at its prompt.
///
/// A shell that `exec`s a program in place keeps its pid and pgid, so that
/// program reads as idle. Separating it from a real prompt needs per-OS
/// process inspection.
#[cfg(unix)]
fn session_has_foreground_process(session: &PtySession) -> bool {
    let Some(foreground_pgid) = session
        .master
        .as_ref()
        .and_then(|m| m.process_group_leader())
    else {
        return false;
    };
    match session.shell.pid() {
        Some(shell_pid) => i64::from(foreground_pgid) != i64::from(shell_pid),
        None => false,
    }
}

/// `tcgetpgrp` has no ConPTY equivalent (`process_group_leader` is
/// unix-only in portable-pty), so non-unix PTYs never report a foreground
/// process and clients close terminals without confirmation.
#[cfg(not(unix))]
fn session_has_foreground_process(_session: &PtySession) -> bool {
    false
}

/// Emits `process_started` / `process_ended` only on idle↔busy transitions so
/// a steady state never repeats notifications.
async fn sample_busy_transition(pty: &Arc<Mutex<PtySession>>, pty_id: &str) {
    let mut session = pty.lock().await;
    let now_busy = session_has_foreground_process(&session);
    if now_busy == session.busy {
        return;
    }
    session.busy = now_busy;
    send_busy_notification(
        pty_id,
        now_busy,
        &session.target_client_id,
        &session.gateway,
    );
}

fn send_busy_notification(
    pty_id: &str,
    busy: bool,
    target_client_id: &TargetClientId,
    gateway: &GatewaySender,
) {
    send_routed_notification(
        gateway,
        NOTIFICATION_METHOD,
        serde_json::json!({
            "terminalId": pty_id,
            "type": if busy { "process_started" } else { "process_ended" },
        }),
        target_client_id,
    );
}

async fn flush_output(
    pty: &Arc<Mutex<PtySession>>,
    pty_id: &str,
    pending: &mut Vec<u8>,
    gateway: &GatewaySender,
) {
    use base64::Engine as _;

    let (output_offset, target_client_id) = {
        let session = pty.lock().await;
        (session.output_offset, session.target_client_id.clone())
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&*pending);
    pending.clear();

    send_routed_notification(
        gateway,
        NOTIFICATION_METHOD,
        serde_json::json!({
            "terminalId": pty_id,
            "type": "output",
            "data": b64,
            "outputOffset": output_offset,
        }),
        &target_client_id,
    );
}

pub async fn write_pty_input(pty_id: &str, data: &[u8]) -> Result<(), TerminalExtError> {
    let entry = require_pty(pty_id).await?;
    let input_tx = entry.lock().await.input_tx.clone();
    input_tx
        .ok_or_else(|| TerminalExtError::InputClosed {
            terminal_id: pty_id.into(),
        })?
        .send(data.to_vec())
        .await
        .map_err(|_| TerminalExtError::InputClosed {
            terminal_id: pty_id.into(),
        })?;
    sample_busy_transition(&entry, pty_id).await;
    Ok(())
}

pub async fn resize_pty(pty_id: &str, rows: u16, cols: u16) -> Result<(), TerminalExtError> {
    let entry = require_pty(pty_id).await?;
    let mut session = entry.lock().await;
    let Some(master) = session.master.as_ref() else {
        return Ok(());
    };
    master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| TerminalExtError::Internal(format!("failed to resize pty: {e}")))?;
    session.rows = rows;
    session.cols = cols;
    Ok(())
}

pub async fn is_exited(pty_id: &str) -> bool {
    match get_pty(pty_id).await {
        Some(entry) => entry.lock().await.shell.poll_exit(),
        None => true,
    }
}

pub async fn close_pty(pty_id: &str) -> Result<(), String> {
    let entry = { PTY_REGISTRY.lock().await.remove(pty_id) };
    if let Some(entry) = entry {
        tokio::task::spawn_blocking(move || reap(&entry))
            .await
            .map_err(|e| format!("close task failed: {e}"))?;
    }
    Ok(())
}

/// Dropping the master would not hang the shell up: the reader and writer hold
/// their own dups of it, and SIGHUP needs the last one closed.
fn reap(entry: &Arc<Mutex<PtySession>>) {
    let hung_up = {
        let mut session = entry.blocking_lock();
        session.master.take();
        // Ends the writer loop, which holds the other dup of the master.
        session.input_tx.take();
        session.shell.hangup()
    };
    if hung_up && wait_for_exit(entry, echo_agent_tty_utils::HANGUP_GRACE) {
        return;
    }
    entry.blocking_lock().shell.kill();
    wait_for_exit(entry, REAP_GRACE);
}

/// Polls rather than blocking on `wait`, re-locking each turn: a shell that
/// ignores its hangup must not wedge teardown or stall the pty's own I/O.
fn wait_for_exit(entry: &Arc<Mutex<PtySession>>, budget: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if entry.blocking_lock().shell.poll_exit() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(EXIT_POLL_INTERVAL);
    }
}

/// Called on agent disconnect to clean up all PTYs.
pub async fn close_all() {
    let entries: Vec<Arc<Mutex<PtySession>>> = {
        let mut reg = PTY_REGISTRY.lock().await;
        reg.drain().map(|(_, v)| v).collect()
    };
    for entry in entries {
        let _ = tokio::task::spawn_blocking(move || reap(&entry)).await;
    }
}

/// Explicit `shell` param, then `$SHELL`, then the platform default. Windows
/// has no `$SHELL`, so it uses the `detect_windows_shell` cascade.
fn resolve_pty_shell(shell: Option<&str>) -> (String, Vec<String>) {
    if let Some(s) = shell {
        return (s.to_string(), vec![]);
    }

    #[cfg(unix)]
    {
        let path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        (path, vec!["-l".to_string()])
    }

    #[cfg(not(unix))]
    {
        use echo_agent_config::shell::{WindowsShell, detect_windows_shell};
        match detect_windows_shell() {
            WindowsShell::GitBash(path) => (path.clone(), vec!["-l".to_string()]),
            WindowsShell::Pwsh => ("pwsh".to_string(), vec!["-NoLogo".to_string()]),
            WindowsShell::PowerShell => ("powershell.exe".to_string(), vec!["-NoLogo".to_string()]),
            WindowsShell::Cmd => ("cmd.exe".to_string(), vec![]),
        }
    }
}

pub(crate) async fn list_ptys() -> Vec<TerminalInfo> {
    let entries: Vec<(String, Arc<Mutex<PtySession>>)> = {
        let reg = PTY_REGISTRY.lock().await;
        reg.iter()
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect()
    };

    let mut result = Vec::with_capacity(entries.len());
    for (id, entry) in entries {
        let mut session = entry.lock().await;
        let (status, exit_code) = if session.shell.poll_exit() {
            (TerminalStatus::Exited, session.shell.exit_code())
        } else {
            (TerminalStatus::Connected, None)
        };
        result.push(TerminalInfo {
            terminal_id: id,
            status,
            interactive: true,
            name: session.name.clone(),
            exit_code,
            cwd: session.cwd.clone(),
            output_offset: session.output_offset,
            created_at: session.created_at,
        });
    }
    result
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyLoadResult {
    pub terminal_id: String,
    pub rows: u16,
    pub cols: u16,
    pub exited: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// Reconnect to a PTY, replaying the ring buffer so the client can reset its
/// VTE emulator and feed all bytes from scratch. An exited PTY still loads, so
/// its final output stays readable.
pub async fn load(
    pty_id: &str,
    gateway: &GatewaySender,
    target_client_id: TargetClientId,
) -> Result<PtyLoadResult, TerminalExtError> {
    let entry = require_pty(pty_id).await?;
    let (replay, output_offset, exited, exit_code, rows, cols, busy) = {
        let mut session = entry.lock().await;
        session.target_client_id = target_client_id.clone();
        // Two facts, not one: a shell whose wait failed is gone with no code.
        let exited = session.shell.poll_exit();
        let exit_code = session.shell.exit_code();
        let busy = session_has_foreground_process(&session);
        session.busy = busy;
        (
            session.output_ring.iter().copied().collect::<Vec<u8>>(),
            session.output_offset,
            exited,
            exit_code,
            session.rows,
            session.cols,
            busy,
        )
    };

    if !replay.is_empty() {
        use base64::Engine as _;
        send_routed_notification(
            gateway,
            NOTIFICATION_METHOD,
            serde_json::json!({
                "terminalId": pty_id,
                "type": "output",
                "data": base64::engine::general_purpose::STANDARD.encode(&replay),
                "outputOffset": output_offset,
                "isReplay": true,
            }),
            &target_client_id,
        );
    }

    if exited {
        send_routed_notification(
            gateway,
            NOTIFICATION_METHOD,
            serde_json::json!({
                "terminalId": pty_id,
                "type": "exit",
                "exitCode": exit_code,
                "isReplay": true,
            }),
            &target_client_id,
        );
    } else {
        send_busy_notification(pty_id, busy, &target_client_id, gateway);
    }

    Ok(PtyLoadResult {
        terminal_id: pty_id.to_string(),
        rows,
        cols,
        exited,
        exit_code,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use agent_client_protocol as acp;
    use echo_agent_acp::acp_gateway;

    type RecordedNotifications = Rc<RefCell<Vec<(String, serde_json::Value)>>>;

    struct RecordingClient {
        notifications: RecordedNotifications,
    }

    #[async_trait::async_trait(?Send)]
    impl acp::Client for RecordingClient {
        async fn request_permission(
            &self,
            _: acp::RequestPermissionRequest,
        ) -> acp::Result<acp::RequestPermissionResponse> {
            unimplemented!()
        }

        async fn session_notification(&self, _: acp::SessionNotification) -> acp::Result<()> {
            Ok(())
        }

        async fn ext_notification(&self, args: acp::ExtNotification) -> acp::Result<()> {
            let params: serde_json::Value =
                serde_json::from_str(args.params.get()).unwrap_or_default();
            self.notifications
                .borrow_mut()
                .push((args.method.to_string(), params));
            Ok(())
        }
    }

    fn recording_gateway() -> (GatewaySender, RecordedNotifications) {
        let notifications: RecordedNotifications = Rc::new(RefCell::new(Vec::new()));
        let (sender, receiver) = acp_gateway::<acp::AgentSide, _>(RecordingClient {
            notifications: notifications.clone(),
        });
        tokio::task::spawn_local(receiver.run());
        (sender, notifications)
    }

    async fn create_test_pty(gateway: GatewaySender) -> String {
        let env = HashMap::from([("ENV".to_string(), String::new())]);
        create_pty(
            Some("/bin/bash"),
            None,
            env,
            24,
            80,
            None,
            gateway,
            TargetClientId::None,
        )
        .await
        .expect("create test pty")
    }

    fn busy_event_types(notifications: &RecordedNotifications, pty_id: &str) -> Vec<String> {
        notifications
            .borrow()
            .iter()
            .filter(|(method, params)| {
                method == NOTIFICATION_METHOD && params["terminalId"] == pty_id
            })
            .filter_map(|(_, params)| match params["type"].as_str() {
                Some(t @ ("process_started" | "process_ended")) => Some(t.to_string()),
                _ => None,
            })
            .collect()
    }

    async fn wait_for_busy_events(
        notifications: &RecordedNotifications,
        pty_id: &str,
        expected: &[&str],
        deadline: Duration,
    ) -> Vec<String> {
        let started = tokio::time::Instant::now();
        loop {
            let events = busy_event_types(notifications, pty_id);
            if events == expected {
                return events;
            }
            if started.elapsed() > deadline {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn long_running_command_emits_balanced_started_then_ended_pair() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (gateway, notifications) = recording_gateway();
                let pty_id = create_test_pty(gateway).await;

                write_pty_input(&pty_id, b"sleep 2\n")
                    .await
                    .expect("write command");

                let after_start = wait_for_busy_events(
                    &notifications,
                    &pty_id,
                    &["process_started"],
                    Duration::from_secs(10),
                )
                .await;
                assert_eq!(after_start, vec!["process_started"]);

                let after_end = wait_for_busy_events(
                    &notifications,
                    &pty_id,
                    &["process_started", "process_ended"],
                    Duration::from_secs(10),
                )
                .await;
                assert_eq!(after_end, vec!["process_started", "process_ended"]);

                close_pty(&pty_id).await.expect("close pty");
            })
            .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn close_pty_kills_a_background_grandchild() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (gateway, _) = recording_gateway();
                let pty_id = create_test_pty(gateway).await;

                write_pty_input(&pty_id, b"sleep 300 & echo pid=$!\n")
                    .await
                    .expect("write command");
                let grandchild = wait_for_reported_pid(&pty_id).await;

                // Without job control the job shares the shell's group and the
                // group kill alone would pass this test.
                let shell = require_pty(&pty_id)
                    .await
                    .expect("pty")
                    .lock()
                    .await
                    .shell
                    .pid()
                    .expect("shell pid") as i32;
                assert_ne!(
                    unsafe { libc::getpgid(grandchild) },
                    unsafe { libc::getpgid(shell) },
                    "background job did not get its own process group"
                );

                close_pty(&pty_id).await.expect("close pty");

                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while unsafe { libc::kill(grandchild, 0) } == 0 {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "grandchild {grandchild} survived the pty close"
                    );
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await;
    }

    /// A local scope, so the process-global one is not latched closed for the
    /// tests that follow.
    #[cfg(unix)]
    #[tokio::test]
    async fn scope_teardown_kills_a_background_grandchild() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (gateway, _) = recording_gateway();
                let pty_id = create_test_pty(gateway).await;

                write_pty_input(&pty_id, b"sleep 300 & echo pid=$!\n")
                    .await
                    .expect("write command");
                let grandchild = wait_for_reported_pid(&pty_id).await;

                let shell = require_pty(&pty_id)
                    .await
                    .expect("pty")
                    .lock()
                    .await
                    .shell
                    .pid()
                    .expect("shell pid");
                assert_ne!(
                    unsafe { libc::getpgid(grandchild) },
                    unsafe { libc::getpgid(shell as i32) },
                    "background job did not get its own process group"
                );

                let scope = echo_agent_tty_utils::ProcessScope::new();
                let _group = scope.enroll_terminal_pid(shell).expect("enroll");
                scope.kill_all();

                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while unsafe { libc::kill(grandchild, 0) } == 0 {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "grandchild {grandchild} survived scope teardown"
                    );
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }

                close_pty(&pty_id).await.expect("close pty");
            })
            .await;
    }

    /// Covers the failure paths in [`create_pty`], which reap by returning.
    #[tokio::test]
    async fn dropping_an_unregistered_shell_reaps_it() {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("sleep 300");
        #[allow(clippy::disallowed_methods)]
        let child = pair.slave.spawn_command(cmd).expect("spawn shell");
        let pid = child.process_id().expect("shell pid") as i32;

        drop(UnregisteredShell::new(child));

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "shell {pid} survived the guard drop"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[cfg(unix)]
    async fn wait_for_reported_pid(pty_id: &str) -> i32 {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let entry = require_pty(pty_id).await.expect("pty");
            let out: Vec<u8> = entry.lock().await.output_ring.iter().copied().collect();
            let text = String::from_utf8_lossy(&out);
            // Every occurrence, since the shell echoes the command's own `pid=$!`.
            if let Some(pid) = text
                .split("pid=")
                .skip(1)
                .filter_map(|rest| rest.split_whitespace().next())
                .find_map(|digits| digits.trim().parse::<i32>().ok())
            {
                return pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "shell never reported a background pid: {text}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn idle_shell_emits_no_busy_notifications() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (gateway, notifications) = recording_gateway();
                let pty_id = create_test_pty(gateway).await;

                tokio::time::sleep(Duration::from_millis(BUSY_POLL_INTERVAL_MS * 3)).await;

                assert_eq!(
                    busy_event_types(&notifications, &pty_id),
                    Vec::<String>::new()
                );

                close_pty(&pty_id).await.expect("close pty");
            })
            .await;
    }
}
