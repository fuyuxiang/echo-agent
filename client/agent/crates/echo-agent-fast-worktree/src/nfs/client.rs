//! Thin grove control-socket client implementing the §Fallback protocol.
//!
//! Decline / unreachable-before-send → copy fallback (no side effects).
//! Timeout / lost reply → poll `QueryWorktreeCreate`; copy fallback only when
//! the daemon reports `aborted` or is provably dead (socket gone + flock free)
//! *and* dest is not a mountpoint. `committed` after poll → adopt, never copy.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use super::mount_table::dest_is_known_unmounted;
use super::{NfsWorktreeOpts, ignored_wire, working_tree_wire};
use crate::worktree::plan::WorktreePlan;

const PROTOCOL_VERSION: u32 = 1;
const MAX_LINE_BYTES: u64 = 4 * 1024 * 1024;
const QUERY_PHASE_MIN_TIMEOUT: Duration = Duration::from_millis(250);
const REMOVE_RPC_TIMEOUT: Duration = Duration::from_secs(60);
/// Match grove `DETACH_RPC_TIMEOUT`: salvage/clean of a large upper can exceed 120s.
const DETACH_RPC_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug)]
pub enum NfsTryError {
    StorageFull,
    InFlight { phase: String },
    Other(anyhow::Error),
}

impl From<anyhow::Error> for NfsTryError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachReply {
    pub phase: String,
    pub same_device: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SalvageReply {
    pub virtual_remaining: Vec<String>,
    pub gitdir_copied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanArtifactsReply {
    pub purged_entries: u64,
    pub no_escapes: bool,
}

#[derive(Debug, Clone)]
pub struct NfsStatusView {
    pub hydration_percent: Option<f64>,
    pub raw: Option<serde_json::Value>,
    pub port: Option<u16>,
    pub mount_id: Option<String>,
    pub transport: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NfsAdopted {
    pub dest: PathBuf,
    pub mount_id: String,
    pub port: u16,
    pub transport: String,
}

#[derive(Debug)]
pub enum NfsCreateDecision {
    Adopted(NfsAdopted),
    /// Typed decline, ping-unreachable, abort complete, or provably-dead + unmounted.
    Fallback,
}

#[derive(Clone, Debug)]
pub struct NfsWorktreeClient {
    sock: PathBuf,
    runtime_dir: PathBuf,
    ping_timeout: Duration,
    create_timeout: Duration,
    query_timeout: Duration,
    query_interval: Duration,
}

impl NfsWorktreeClient {
    #[must_use]
    pub fn from_opts(opts: &NfsWorktreeOpts) -> Self {
        let sock = resolve_control_sock(opts);
        let runtime_dir = opts
            .runtime_dir
            .clone()
            .or_else(|| sock.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("/tmp/grove-missing-runtime"));
        Self {
            sock,
            runtime_dir,
            ping_timeout: opts.ping_timeout,
            create_timeout: opts.create_timeout,
            query_timeout: opts.query_timeout,
            query_interval: opts.query_interval,
        }
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.sock
    }

    /// Control-socket Ping with a hard timeout. `false` ⇒ unreachable before send.
    pub fn ping(&self) -> bool {
        match self.call(
            &Request::Ping {
                v: PROTOCOL_VERSION,
            },
            self.ping_timeout,
        ) {
            Ok(Response::Ok(body)) => body.pong || body.declined.is_none(),
            _ => false,
        }
    }

    pub(crate) fn create_worktree(
        &self,
        plan: &WorktreePlan,
    ) -> Result<NfsCreateDecision, NfsTryError> {
        if !self.ping() {
            // Same refuse-copy rule as lost-reply: require is_provably_dead
            // plus known-unmounted dest. A busy daemon (lock held, ping fails)
            // must not copy-fallback onto an in-flight NFS create that already
            // mkdir'd dest. deadline_decision also refuses a non-empty leftover.
            if self.is_provably_dead() && dest_is_known_unmounted(&plan.dest) {
                return self.deadline_decision(plan, "daemon-unreachable".into());
            }
            // is_provably_dead pings again. A recovered daemon must send
            // CreateWorktree, not InFlight (which blocks copy and fails hard).
            if self.ping() {
                // fall through to CreateWorktree
            } else {
                return Err(NfsTryError::InFlight {
                    phase: if dest_is_known_unmounted(&plan.dest) {
                        "daemon-unreachable".into()
                    } else {
                        "dest-mounted".into()
                    },
                });
            }
        }

        let req = Request::CreateWorktree {
            v: PROTOCOL_VERSION,
            source: plan.source.display().to_string(),
            dest: plan.dest.display().to_string(),
            git_ref: plan.git_ref.clone(),
            working_tree: working_tree_wire(&plan.working_tree).to_owned(),
            ignored: ignored_wire(&plan.ignored_files).to_owned(),
            worktree_id: plan.worktree_id.clone(),
        };

        match self.call(&req, self.create_timeout) {
            Ok(Response::Ok(body)) => self.interpret_create_ok(plan, body),
            Ok(Response::Err(e)) => self.interpret_create_err(plan, &e.error),
            Err(e) if is_timeout_io(&e) => self.poll_after_lost_reply(plan),
            Err(e) => {
                // Write may have landed; do not copy-fallback on a lost reply.
                tracing::warn!(error = %e, "nfs create transport error; polling journal");
                self.poll_after_lost_reply(plan)
            }
        }
    }

    pub fn query_phase(&self, worktree_id: &str) -> Result<QuerySnapshot, NfsTryError> {
        let req = Request::QueryWorktreeCreate {
            v: PROTOCOL_VERSION,
            worktree_id: worktree_id.to_owned(),
        };
        match self.call(&req, self.ping_timeout.max(QUERY_PHASE_MIN_TIMEOUT)) {
            Ok(Response::Ok(body)) => Ok(QuerySnapshot {
                phase: body.create_phase,
                declined: body.declined,
                storage_full: body.storage_full,
                unknown: false,
                error: None,
                mount: body.mount,
            }),
            Ok(Response::Err(e)) => Ok(QuerySnapshot {
                phase: None,
                declined: None,
                storage_full: false,
                unknown: e.error.contains("unknown worktree_id"),
                error: Some(e.error),
                mount: None,
            }
            .normalized()),
            Err(e) => Err(NfsTryError::Other(e)),
        }
    }

    pub fn remove_worktree(&self, dest: &Path, force: bool) -> Result<(), anyhow::Error> {
        if !self.ping() {
            anyhow::bail!("grove daemon unreachable");
        }
        let req = Request::RemoveWorktree {
            v: PROTOCOL_VERSION,
            dest: dest.display().to_string(),
            force,
        };
        match self.call(&req, REMOVE_RPC_TIMEOUT) {
            Ok(Response::Ok(_)) => Ok(()),
            Ok(Response::Err(e)) => Err(anyhow!(e.error)),
            Err(e) => Err(e),
        }
    }

    pub fn detach_worktree(
        &self,
        dest: &Path,
        allow_copy: bool,
    ) -> Result<DetachReply, anyhow::Error> {
        if !self.ping() {
            anyhow::bail!("grove daemon unreachable");
        }
        let req = Request::DetachWorktree {
            v: PROTOCOL_VERSION,
            dest: dest.display().to_string(),
            allow_copy,
        };
        match self.call(&req, DETACH_RPC_TIMEOUT) {
            Ok(Response::Ok(body)) => Ok(DetachReply {
                phase: body.detach_phase.or(body.create_phase).unwrap_or_default(),
                same_device: body.same_device.unwrap_or(true),
            }),
            Ok(Response::Err(e)) => Err(anyhow!(e.error)),
            Err(e) => Err(e),
        }
    }

    pub fn salvage_worktree(&self, dest: &Path, out: &Path) -> Result<SalvageReply, anyhow::Error> {
        if !self.ping() {
            anyhow::bail!("grove daemon unreachable");
        }
        let req = Request::SalvageWorktree {
            v: PROTOCOL_VERSION,
            dest: dest.display().to_string(),
            out: out.display().to_string(),
        };
        match self.call(&req, DETACH_RPC_TIMEOUT) {
            Ok(Response::Ok(body)) => Ok(SalvageReply {
                virtual_remaining: body.virtual_remaining.unwrap_or_default(),
                gitdir_copied: body.gitdir_copied,
            }),
            Ok(Response::Err(e)) => Err(anyhow!(e.error)),
            Err(e) => Err(e),
        }
    }

    pub fn clean_artifacts(&self, dest: &Path) -> Result<CleanArtifactsReply, anyhow::Error> {
        if !self.ping() {
            anyhow::bail!("grove daemon unreachable");
        }
        let req = Request::CleanArtifacts {
            v: PROTOCOL_VERSION,
            dest: dest.display().to_string(),
        };
        match self.call(&req, DETACH_RPC_TIMEOUT) {
            Ok(Response::Ok(body)) => Ok(CleanArtifactsReply {
                purged_entries: body.purged_entries.unwrap_or(0),
                no_escapes: body.no_escapes,
            }),
            Ok(Response::Err(e)) => Err(anyhow!(e.error)),
            Err(e) => Err(e),
        }
    }

    /// Live mount status for `echoagent worktree show`. `None` if unreachable.
    pub fn status_for_dir(&self, dest: &Path) -> Option<NfsStatusView> {
        let req = Request::Status {
            v: PROTOCOL_VERSION,
            dir: Some(dest.display().to_string()),
        };
        match self.call(&req, self.ping_timeout.max(Duration::from_millis(250))) {
            Ok(Response::Ok(body)) => Some(NfsStatusView {
                hydration_percent: body.hydration_percent,
                raw: body.status,
                port: body.mount.as_ref().map(|m| m.port),
                mount_id: body.mount.as_ref().map(|m| m.mount_id.clone()),
                transport: body.mount.as_ref().map(|m| m.transport.clone()),
            }),
            _ => None,
        }
    }

    fn interpret_create_ok(
        &self,
        plan: &WorktreePlan,
        body: OkBody,
    ) -> Result<NfsCreateDecision, NfsTryError> {
        if body.storage_full {
            return Err(NfsTryError::StorageFull);
        }
        if body.declined.is_some() {
            return Ok(NfsCreateDecision::Fallback);
        }
        match body.create_phase.as_deref() {
            Some("committed") | None => {
                if let Some(m) = body.mount {
                    return Ok(NfsCreateDecision::Adopted(NfsAdopted {
                        dest: plan.dest.clone(),
                        mount_id: m.mount_id,
                        port: m.port,
                        transport: m.transport,
                    }));
                }
                if body.create_phase.as_deref() == Some("committed") {
                    return Ok(NfsCreateDecision::Adopted(NfsAdopted {
                        dest: plan.dest.clone(),
                        mount_id: String::new(),
                        port: 0,
                        transport: super::default_grove_transport().into(),
                    }));
                }
                self.poll_after_lost_reply(plan)
            }
            Some("aborted") => Ok(NfsCreateDecision::Fallback),
            Some(phase) => {
                // Reply returned an in-flight phase (daemon still working). Poll.
                tracing::info!(phase, "nfs create returned in-flight phase; polling");
                self.poll_after_lost_reply(plan)
            }
        }
    }

    fn interpret_create_err(
        &self,
        plan: &WorktreePlan,
        error: &str,
    ) -> Result<NfsCreateDecision, NfsTryError> {
        let lower = error.to_ascii_lowercase();
        if lower.contains("unknown") && (lower.contains("op") || lower.contains("unknown variant"))
        {
            return Ok(NfsCreateDecision::Fallback);
        }
        if lower.contains("no space") || lower.contains("storage full") {
            return Err(NfsTryError::StorageFull);
        }
        if lower.contains("daemon.db") {
            // No journal: dest was never projected. Polling would wait out
            // the query timeout then InFlight because the socket still answers.
            return Ok(NfsCreateDecision::Fallback);
        }
        // Daemon may have journaled before failing; poll rather than copy.
        tracing::warn!(error, "nfs create ErrBody; polling journal");
        self.poll_after_lost_reply(plan)
    }

    fn poll_after_lost_reply(&self, plan: &WorktreePlan) -> Result<NfsCreateDecision, NfsTryError> {
        let deadline = Instant::now() + self.query_timeout;
        loop {
            match self.query_phase(&plan.worktree_id) {
                Ok(snap) if snap.storage_full => return Err(NfsTryError::StorageFull),
                Ok(snap) if snap.declined.is_some() => return Ok(NfsCreateDecision::Fallback),
                Ok(snap) if snap.phase.as_deref() == Some("aborted") => {
                    return Ok(NfsCreateDecision::Fallback);
                }
                Ok(snap) if snap.phase.as_deref() == Some("committed") => {
                    let (mount_id, port, transport) = match snap.mount {
                        Some(m) => (m.mount_id, m.port, m.transport),
                        None => (String::new(), 0, super::default_grove_transport().into()),
                    };
                    return Ok(NfsCreateDecision::Adopted(NfsAdopted {
                        dest: plan.dest.clone(),
                        mount_id,
                        port,
                        transport,
                    }));
                }
                Ok(snap) => {
                    // `unknown worktree_id` is not proof the create never started:
                    // dest is mkdir'd before the first journal persist. Keep
                    // polling; only deadline_decision (aborted is handled above;
                    // else provably-dead and not a mountpoint) may Fallback.
                    if Instant::now() >= deadline {
                        let phase = snap.phase.unwrap_or_else(|| {
                            if snap.unknown {
                                "unknown".into()
                            } else {
                                String::new()
                            }
                        });
                        return self.deadline_decision(plan, phase);
                    }
                }
                Err(_) => {
                    if Instant::now() >= deadline {
                        return self.deadline_decision(plan, String::new());
                    }
                }
            }
            if Instant::now() >= deadline {
                return self.deadline_decision(plan, String::new());
            }
            std::thread::sleep(self.query_interval);
        }
    }

    fn deadline_decision(
        &self,
        plan: &WorktreePlan,
        phase: String,
    ) -> Result<NfsCreateDecision, NfsTryError> {
        if self.is_provably_dead() && dest_is_known_unmounted(&plan.dest) {
            // git worktree add refuses an existing directory. A lost create
            // may have already mkdir'd dest; clear an empty leftover.
            if plan.dest.exists() {
                let empty = plan
                    .dest
                    .read_dir()
                    .map(|mut i| i.next().is_none())
                    .unwrap_or(false);
                if empty {
                    let _ = std::fs::remove_dir(&plan.dest);
                } else {
                    return Err(NfsTryError::InFlight {
                        phase: "dest-exists".into(),
                    });
                }
            }
            return Ok(NfsCreateDecision::Fallback);
        }
        Err(NfsTryError::InFlight {
            phase: if phase.is_empty() {
                "unknown".into()
            } else {
                phase
            },
        })
    }

    /// Socket gone (or unpingable) **and** daemon flock free.
    pub fn is_provably_dead(&self) -> bool {
        if self.sock.exists() && self.ping() {
            return false;
        }
        daemon_flock_free(&self.runtime_dir)
    }

    fn call(&self, req: &Request, timeout: Duration) -> Result<Response> {
        let mut stream = connect_unix(&self.sock, timeout)
            .with_context(|| format!("connect {}", self.sock.display()))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let mut bytes = serde_json::to_vec(req)?;
        bytes.push(b'\n');
        stream.write_all(&bytes)?;
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let mut reader = BufReader::new((&stream).take(MAX_LINE_BYTES));
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.is_empty() {
            return Err(anyhow!("empty response (timeout or closed)"));
        }
        Ok(serde_json::from_str(line.trim())?)
    }
}

#[derive(Debug, Default)]
pub struct QuerySnapshot {
    pub phase: Option<String>,
    pub declined: Option<String>,
    pub storage_full: bool,
    pub unknown: bool,
    pub error: Option<String>,
    pub mount: Option<MountInfo>,
}

impl QuerySnapshot {
    fn normalized(mut self) -> Self {
        if self
            .error
            .as_ref()
            .is_some_and(|e| e.contains("unknown worktree_id"))
        {
            self.unknown = true;
        }
        self
    }
}

/// `UnixStream::connect` has no deadline. Non-blocking connect + `poll` so a
/// socket that exists but is not accepting cannot hang ping/create/remove.
fn connect_unix(path: &Path, timeout: Duration) -> Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    let max_path = {
        // SAFETY: sockaddr_un is a C POD; zeroed is a valid empty address.
        let z: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        z.sun_path.len()
    };
    if bytes.len() >= max_path {
        anyhow::bail!("unix socket path too long: {}", path.display());
    }

    // SAFETY: AF_UNIX/SOCK_STREAM is a defined socket; the fd is owned below.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("socket");
    }
    // SAFETY: `fd` is a socket we just created and exclusively own.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let raw = fd.as_raw_fd();
    // SAFETY: `raw` is the live socket from `fd`; F_GETFD/F_SETFD/F_GETFL/F_SETFL
    // on our fd are defined.
    unsafe {
        let fd_flags = libc::fcntl(raw, libc::F_GETFD);
        if fd_flags >= 0 {
            libc::fcntl(raw, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC);
        }
        let fl = libc::fcntl(raw, libc::F_GETFL);
        if fl < 0 || libc::fcntl(raw, libc::F_SETFL, fl | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error()).context("fcntl O_NONBLOCK");
        }
    }

    // SAFETY: sockaddr_un is a C POD; zeroed then filled with a path we own.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (i, b) in bytes.iter().enumerate() {
        addr.sun_path[i] = *b as libc::c_char;
    }
    let addr_len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    // SAFETY: `addr` is a fully initialized sockaddr_un; `raw` is our socket.
    let rc = unsafe {
        libc::connect(
            raw,
            std::ptr::addr_of!(addr).cast::<libc::sockaddr>(),
            addr_len,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(err).context("connect");
        }
        let mut pfd = libc::pollfd {
            fd: raw,
            events: libc::POLLOUT,
            revents: 0,
        };
        let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: `pfd` is one pollfd we own for the duration of the call.
        let pr = unsafe { libc::poll(std::ptr::addr_of_mut!(pfd), 1, ms) };
        if pr == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "connect timed out",
            ))
            .context("connect");
        }
        if pr < 0 {
            return Err(std::io::Error::last_os_error()).context("poll");
        }
        let mut so_err: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: `so_err`/`len` are valid stack integers; `raw` is our socket.
        let gs = unsafe {
            libc::getsockopt(
                raw,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                std::ptr::addr_of_mut!(so_err).cast(),
                std::ptr::addr_of_mut!(len),
            )
        };
        if gs < 0 {
            return Err(std::io::Error::last_os_error()).context("getsockopt SO_ERROR");
        }
        if so_err != 0 {
            return Err(std::io::Error::from_raw_os_error(so_err)).context("connect");
        }
    }

    // SAFETY: `raw` is still the owned socket; clearing O_NONBLOCK is defined.
    unsafe {
        let fl = libc::fcntl(raw, libc::F_GETFL);
        if fl >= 0 {
            libc::fcntl(raw, libc::F_SETFL, fl & !libc::O_NONBLOCK);
        }
    }
    Ok(UnixStream::from(fd))
}

fn is_timeout_io(err: &anyhow::Error) -> bool {
    err.chain().any(|c| {
        if let Some(io) = c.downcast_ref::<std::io::Error>() {
            return matches!(
                io.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            );
        }
        let s = c.to_string();
        s.contains("timed out") || s.contains("Timeout") || s.contains("empty response")
    })
}

fn resolve_control_sock(opts: &NfsWorktreeOpts) -> PathBuf {
    if let Some(p) = &opts.control_sock {
        return p.clone();
    }
    if let Ok(p) = std::env::var("GROVE_CONTROL_SOCK") {
        return PathBuf::from(p);
    }
    if let Some(rt) = &opts.runtime_dir {
        return rt.join("control.sock");
    }
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(xdg).join("grove").join("control.sock");
    }
    PathBuf::from("/tmp/grove-missing/control.sock")
}

/// `LOCK_EX|LOCK_NB` on `<runtime>/daemon.lock`. Acquiring it means no daemon
/// holds the singleton; we drop immediately. WouldBlock ⇒ daemon alive.
fn daemon_flock_free(runtime_dir: &Path) -> bool {
    let path = runtime_dir.join("daemon.lock");
    if !path.exists() && !runtime_dir.exists() {
        return true;
    }
    let _ = std::fs::create_dir_all(runtime_dir);
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return false,
    };
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
    // SAFETY: `fd` is the live descriptor of `file` (open for the lock probe).
    // LOCK_EX|LOCK_NB is valid on that fd; we unlock the same fd if we took it.
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        // SAFETY: we hold LOCK_EX on `fd` from the call above.
        let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
        true
    } else {
        false
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Ping {
        v: u32,
    },
    CreateWorktree {
        v: u32,
        source: String,
        dest: String,
        git_ref: String,
        working_tree: String,
        ignored: String,
        worktree_id: String,
    },
    QueryWorktreeCreate {
        v: u32,
        worktree_id: String,
    },
    RemoveWorktree {
        v: u32,
        dest: String,
        #[serde(default)]
        force: bool,
    },
    DetachWorktree {
        v: u32,
        dest: String,
        #[serde(default)]
        allow_copy: bool,
    },
    QueryWorktreeDetach {
        v: u32,
        worktree_id: String,
    },
    SalvageWorktree {
        v: u32,
        dest: String,
        out: String,
    },
    CleanArtifacts {
        v: u32,
        dest: String,
    },
    Status {
        v: u32,
        #[serde(default)]
        dir: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
enum Response {
    Ok(OkBody),
    Err(ErrBody),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ErrBody {
    #[serde(default)]
    v: u32,
    error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OkBody {
    #[serde(default)]
    v: u32,
    #[serde(default)]
    pong: bool,
    #[serde(default)]
    mount: Option<MountInfo>,
    #[serde(default)]
    resolved_strategy: Option<String>,
    #[serde(default)]
    create_phase: Option<String>,
    #[serde(default)]
    declined: Option<String>,
    #[serde(default)]
    storage_full: bool,
    #[serde(default)]
    detach_phase: Option<String>,
    #[serde(default)]
    same_device: Option<bool>,
    #[serde(default)]
    virtual_remaining: Option<Vec<String>>,
    #[serde(default)]
    purged_entries: Option<u64>,
    #[serde(default)]
    no_escapes: bool,
    #[serde(default)]
    gitdir_copied: bool,
    #[serde(default)]
    status: Option<serde_json::Value>,
    #[serde(default)]
    hydration_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountInfo {
    pub port: u16,
    pub mount_id: String,
    pub transport: String,
}

#[cfg(test)]
mod tests {
    use super::super::mount_table::dest_is_mountpoint;
    use super::*;
    use crate::{CreationMode, IgnoredFilesMode, WorkingTreeMode};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    #[derive(Clone)]
    struct Script {
        ping_delay: Duration,
        create_hold: Duration,
        create_reply: Option<String>,
        query_replies: Arc<Mutex<Vec<String>>>,
        creates: Arc<AtomicUsize>,
        queries: Arc<AtomicUsize>,
        pings: Arc<AtomicUsize>,
        /// After the create hold, unlink the sock, drop flock, and stop accepting.
        die_after_create: bool,
        /// Hold `daemon.lock` for the server lifetime (released on die/exit).
        hold_lock_until_exit: bool,
    }

    impl Default for Script {
        fn default() -> Self {
            Self {
                ping_delay: Duration::ZERO,
                create_hold: Duration::ZERO,
                create_reply: None,
                query_replies: Arc::new(Mutex::new(Vec::new())),
                creates: Arc::new(AtomicUsize::new(0)),
                queries: Arc::new(AtomicUsize::new(0)),
                pings: Arc::new(AtomicUsize::new(0)),
                die_after_create: false,
                hold_lock_until_exit: false,
            }
        }
    }

    fn hold_daemon_lock(runtime_dir: &Path) -> std::fs::File {
        let lock_file = std::fs::File::create(runtime_dir.join("daemon.lock")).unwrap();
        let rc = unsafe {
            libc::flock(
                std::os::unix::io::AsRawFd::as_raw_fd(&lock_file),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        };
        assert_eq!(rc, 0, "test failed to acquire daemon.lock");
        lock_file
    }

    fn spawn_server(sock: PathBuf, script: Script) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(false).unwrap();
        thread::spawn(move || {
            let runtime = sock.parent().map(Path::to_path_buf);
            let mut lock_guard = if script.hold_lock_until_exit {
                runtime.as_deref().map(hold_daemon_lock)
            } else {
                None
            };
            for incoming in listener.incoming() {
                let Ok(mut stream) = incoming else { break };
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.is_empty() {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(line.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let op = v.get("op").and_then(|o| o.as_str()).unwrap_or("");
                let mut die = false;
                let reply = match op {
                    "ping" => {
                        script.pings.fetch_add(1, Ordering::SeqCst);
                        if !script.ping_delay.is_zero() {
                            thread::sleep(script.ping_delay);
                        }
                        Some(r#"{"status":"ok","data":{"v":1,"pong":true}}"#.to_owned())
                    }
                    "create_worktree" => {
                        script.creates.fetch_add(1, Ordering::SeqCst);
                        if !script.create_hold.is_zero() {
                            thread::sleep(script.create_hold);
                        }
                        if script.die_after_create {
                            drop(lock_guard.take());
                            let _ = std::fs::remove_file(&sock);
                            die = true;
                            None
                        } else {
                            script.create_reply.clone()
                        }
                    }
                    "query_worktree_create" => {
                        script.queries.fetch_add(1, Ordering::SeqCst);
                        let mut q = script.query_replies.lock().unwrap();
                        if q.is_empty() {
                            Some(
                                r#"{"status":"err","data":{"v":1,"error":"unknown worktree_id x"}}"#
                                    .to_owned(),
                            )
                        } else if q.len() == 1 {
                            Some(q[0].clone())
                        } else {
                            Some(q.remove(0))
                        }
                    }
                    _ => Some(r#"{"status":"err","data":{"v":1,"error":"unknown op"}}"#.to_owned()),
                };
                if let Some(r) = reply {
                    let _ = writeln!(stream, "{r}");
                }
                if die {
                    break;
                }
            }
        })
    }

    fn plan_at(tmp: &TempDir, dest_name: &str, nfs: NfsWorktreeOpts) -> WorktreePlan {
        let dest = tmp.path().join(dest_name);
        WorktreePlan {
            source: tmp.path().join("repo"),
            dest: dest.clone(),
            git_ref: "HEAD".into(),
            parallelism: 1,
            channel_buffer: 8,
            working_tree: WorkingTreeMode::PreserveWorkingTree,
            ignored_files: IgnoredFilesMode::Skip,
            ignored_parallelism: 1,
            creation_mode: CreationMode::Linked,
            cancellation_token: CancellationToken::new(),
            btrfs_delegate: None,
            worktree_id: crate::worktree::plan::worktree_id_from_path(&dest),
            nfs: Some(nfs),
        }
    }

    fn opts(sock: &Path, runtime: &Path) -> NfsWorktreeOpts {
        NfsWorktreeOpts {
            enabled: true,
            control_sock: Some(sock.to_path_buf()),
            data_dir: None,
            runtime_dir: Some(runtime.to_path_buf()),
            ping_timeout: Duration::from_millis(80),
            create_timeout: Duration::from_millis(80),
            query_timeout: Duration::from_millis(400),
            query_interval: Duration::from_millis(15),
        }
    }

    fn timeout_opts(sock: &Path, runtime: &Path) -> NfsWorktreeOpts {
        let mut o = opts(sock, runtime);
        o.query_timeout = Duration::from_millis(80);
        o
    }

    fn lost_create_script() -> Script {
        Script {
            create_hold: Duration::from_millis(300),
            ..Default::default()
        }
    }

    #[test]
    fn connect_missing_socket_fails_quickly() {
        let tmp = TempDir::new().unwrap();
        let start = Instant::now();
        let err =
            connect_unix(&tmp.path().join("missing.sock"), Duration::from_millis(80)).unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "missing socket must not hang: {err}"
        );
    }

    #[test]
    fn daemon_down_is_fallback() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("control.sock");
        let o = opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan).unwrap() {
            NfsCreateDecision::Fallback => {}
            other => panic!("expected fallback, got {other:?}"),
        }
    }

    #[test]
    fn ping_timeout_is_fallback_without_create() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let script = Script {
            ping_delay: Duration::from_millis(300),
            create_reply: Some(
                r#"{"status":"ok","data":{"v":1,"create_phase":"committed","mount":{"port":1,"mount_id":"1","transport":"nfs"}}}"#
                    .into(),
            ),
            ..Default::default()
        };
        let _h = spawn_server(sock.clone(), script.clone());
        thread::sleep(Duration::from_millis(20));
        let o = opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan).unwrap() {
            NfsCreateDecision::Fallback => {}
            other => panic!("unreachable ping must fallback, got {other:?}"),
        }
        assert_eq!(
            script.creates.load(Ordering::SeqCst),
            0,
            "ping failure must not send CreateWorktree"
        );
        assert!(script.pings.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn typed_decline_is_fallback() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let script = Script {
            create_reply: Some(r#"{"status":"ok","data":{"v":1,"declined":"jj-repo"}}"#.into()),
            ..Default::default()
        };
        let _h = spawn_server(sock.clone(), script.clone());
        thread::sleep(Duration::from_millis(20));
        let o = opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan).unwrap() {
            NfsCreateDecision::Fallback => {}
            other => panic!("{other:?}"),
        }
        assert!(script.creates.load(Ordering::SeqCst) >= 1);
        assert_eq!(
            script.queries.load(Ordering::SeqCst),
            0,
            "declined must not poll"
        );
    }

    #[test]
    fn daemon_db_unavailable_is_fallback_without_poll() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let script = Script {
            create_reply: Some(
                r#"{"status":"err","data":{"v":1,"error":"daemon.db unavailable"}}"#.into(),
            ),
            ..Default::default()
        };
        let _h = spawn_server(sock.clone(), script.clone());
        thread::sleep(Duration::from_millis(20));
        let o = opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan).unwrap() {
            NfsCreateDecision::Fallback => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(
            script.queries.load(Ordering::SeqCst),
            0,
            "missing daemon.db must not poll"
        );
    }

    #[test]
    fn timeout_then_committed_adopts_without_second_create() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let committed = r#"{"status":"ok","data":{"v":1,"create_phase":"committed","resolved_strategy":"nfs","mount":{"port":12345,"mount_id":"99","transport":"nfs"}}}"#;
        let script = Script {
            create_hold: Duration::from_millis(300),
            query_replies: Arc::new(Mutex::new(vec![
                r#"{"status":"ok","data":{"v":1,"create_phase":"index_ready"}}"#.into(),
                committed.into(),
            ])),
            ..Default::default()
        };
        let _h = spawn_server(sock.clone(), script.clone());
        thread::sleep(Duration::from_millis(20));
        let o = opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan).unwrap() {
            NfsCreateDecision::Adopted(a) => {
                assert_eq!(a.port, 12345);
                assert_eq!(a.mount_id, "99");
            }
            other => panic!("must adopt committed, got {other:?}"),
        }
        assert_eq!(
            script.creates.load(Ordering::SeqCst),
            1,
            "must not re-issue CreateWorktree after timeout"
        );
        assert!(script.queries.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn committed_without_mount_uses_os_default_transport() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let script = Script {
            create_reply: Some(
                r#"{"status":"ok","data":{"v":1,"create_phase":"committed"}}"#.into(),
            ),
            ..Default::default()
        };
        let _h = spawn_server(sock.clone(), script);
        thread::sleep(Duration::from_millis(20));
        let o = opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan).unwrap() {
            NfsCreateDecision::Adopted(a) => {
                assert_eq!(a.transport, super::super::default_grove_transport());
            }
            other => panic!("committed without mount must adopt, got {other:?}"),
        }
    }

    #[test]
    fn poll_committed_without_mount_uses_os_default_transport() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let script = Script {
            create_hold: Duration::from_millis(300),
            query_replies: Arc::new(Mutex::new(vec![
                r#"{"status":"ok","data":{"v":1,"create_phase":"committed"}}"#.into(),
            ])),
            ..Default::default()
        };
        let _h = spawn_server(sock.clone(), script);
        thread::sleep(Duration::from_millis(20));
        let o = opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan).unwrap() {
            NfsCreateDecision::Adopted(a) => {
                assert_eq!(a.transport, super::super::default_grove_transport());
            }
            other => panic!("polled committed without mount must adopt, got {other:?}"),
        }
    }

    #[test]
    fn timeout_then_aborted_falls_back_without_second_create() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let script = Script {
            create_hold: Duration::from_millis(300),
            query_replies: Arc::new(Mutex::new(vec![
                r#"{"status":"ok","data":{"v":1,"create_phase":"rolling_back"}}"#.into(),
                r#"{"status":"ok","data":{"v":1,"create_phase":"aborted"}}"#.into(),
            ])),
            ..Default::default()
        };
        let _h = spawn_server(sock.clone(), script.clone());
        thread::sleep(Duration::from_millis(20));
        let o = opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan).unwrap() {
            NfsCreateDecision::Fallback => {}
            other => panic!("aborted must fallback, got {other:?}"),
        }
        assert_eq!(script.creates.load(Ordering::SeqCst), 1);
        assert!(script.queries.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn storage_full_is_typed_error_not_fallback() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let script = Script {
            create_reply: Some(r#"{"status":"ok","data":{"v":1,"storage_full":true}}"#.into()),
            ..Default::default()
        };
        let _h = spawn_server(sock.clone(), script);
        thread::sleep(Duration::from_millis(20));
        let o = opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan) {
            Err(NfsTryError::StorageFull) => {}
            other => panic!("expected StorageFull, got {other:?}"),
        }
    }

    #[test]
    fn timeout_still_inflight_does_not_fallback() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let script = Script {
            create_hold: Duration::from_millis(300),
            query_replies: Arc::new(Mutex::new(vec![
                r#"{"status":"ok","data":{"v":1,"create_phase":"mounted"}}"#.into(),
            ])),
            ..Default::default()
        };
        let _h = spawn_server(sock.clone(), script.clone());
        thread::sleep(Duration::from_millis(20));
        let o = timeout_opts(&sock, tmp.path());
        let lock_file = hold_daemon_lock(tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan) {
            Err(NfsTryError::InFlight { phase }) => {
                assert!(
                    phase.contains("mounted") || phase == "unknown" || phase.contains("unknown")
                );
            }
            other => panic!("must not fallback while in-flight, got {other:?}"),
        }
        drop(lock_file);
    }

    #[test]
    fn timeout_unknown_id_while_create_running_does_not_fallback() {
        // Dest is mkdir'd before journaling. Lost create + Query unknown +
        // dest not a mountpoint is exactly a still-running create.
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let dest = tmp.path().join("d");
        std::fs::create_dir(&dest).unwrap();
        let script = lost_create_script();
        let _h = spawn_server(sock.clone(), script.clone());
        thread::sleep(Duration::from_millis(20));
        let o = timeout_opts(&sock, tmp.path());
        let lock_file = hold_daemon_lock(tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan) {
            Err(NfsTryError::InFlight { phase }) => {
                assert!(
                    phase.contains("unknown") || phase.is_empty(),
                    "expected unknown in-flight phase, got {phase:?}"
                );
            }
            other => panic!("live create must not copy-fallback, got {other:?}"),
        }
        assert_eq!(script.creates.load(Ordering::SeqCst), 1);
        assert!(
            script.queries.load(Ordering::SeqCst) >= 1,
            "must poll QueryWorktreeCreate after the lost create"
        );
        drop(lock_file);
    }

    #[test]
    fn timeout_dead_daemon_unmounted_dest_is_fallback_without_second_create() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let dest = tmp.path().join("d");
        std::fs::create_dir(&dest).unwrap();
        assert!(
            !dest_is_mountpoint(&dest),
            "plain temp dest must not be a mountpoint"
        );
        let script = Script {
            die_after_create: true,
            hold_lock_until_exit: true,
            ..lost_create_script()
        };
        let _h = spawn_server(sock.clone(), script.clone());
        thread::sleep(Duration::from_millis(20));
        let o = timeout_opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan).unwrap() {
            NfsCreateDecision::Fallback => {}
            other => panic!("dead daemon + unmounted dest must Fallback, got {other:?}"),
        }
        assert_eq!(script.creates.load(Ordering::SeqCst), 1);
        assert!(
            client.is_provably_dead(),
            "after the mock exits, flock must be free and sock gone"
        );
    }

    #[test]
    fn timeout_unknown_id_pingable_daemon_does_not_fallback() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        let script = lost_create_script();
        let _h = spawn_server(sock.clone(), script.clone());
        thread::sleep(Duration::from_millis(20));
        let o = timeout_opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan) {
            Err(NfsTryError::InFlight { .. }) => {}
            other => panic!("pingable daemon must stay InFlight, got {other:?}"),
        }
        assert_eq!(script.creates.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn timeout_unknown_id_flock_held_does_not_fallback() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        let script = Script {
            die_after_create: true,
            ..lost_create_script()
        };
        let _h = spawn_server(sock.clone(), script.clone());
        thread::sleep(Duration::from_millis(20));
        let o = timeout_opts(&sock, tmp.path());
        let lock_file = hold_daemon_lock(tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let plan = plan_at(&tmp, "d", o);
        match client.create_worktree(&plan) {
            Err(NfsTryError::InFlight { .. }) => {}
            other => panic!("held flock must stay InFlight, got {other:?}"),
        }
        assert_eq!(script.creates.load(Ordering::SeqCst), 1);
        drop(lock_file);
    }

    #[test]
    fn timeout_unknown_id_dest_is_mountpoint_does_not_fallback() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let dest = PathBuf::from("/");
        assert!(
            dest_is_mountpoint(&dest),
            "test needs a real kernel mountpoint; / is not in the mount table"
        );
        let script = Script {
            die_after_create: true,
            hold_lock_until_exit: true,
            ..lost_create_script()
        };
        let _h = spawn_server(sock.clone(), script.clone());
        thread::sleep(Duration::from_millis(20));
        let o = timeout_opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let mut plan = plan_at(&tmp, "d", o);
        plan.dest = dest;
        match client.create_worktree(&plan) {
            Err(NfsTryError::InFlight { .. }) => {}
            other => panic!("mountpoint dest must stay InFlight, got {other:?}"),
        }
        assert_eq!(script.creates.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ping_fail_dest_is_mountpoint_does_not_fallback() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("missing.sock");
        let dest = PathBuf::from("/");
        assert!(
            dest_is_mountpoint(&dest),
            "test needs a real kernel mountpoint; / is not in the mount table"
        );
        let o = timeout_opts(&sock, tmp.path());
        let client = NfsWorktreeClient::from_opts(&o);
        let mut plan = plan_at(&tmp, "d", o);
        plan.dest = dest;
        match client.create_worktree(&plan) {
            Err(NfsTryError::InFlight { phase }) => {
                assert_eq!(phase, "dest-mounted");
            }
            other => {
                panic!("unreachable daemon + mounted dest must not copy-fallback, got {other:?}")
            }
        }
    }
}
