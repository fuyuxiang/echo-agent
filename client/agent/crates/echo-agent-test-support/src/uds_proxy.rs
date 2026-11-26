//! Frame-aware fault-injection proxy for unix-domain-socket IPC.
//!
//! Sits between a client and a real listener (`proxy.sock` → `real.sock`),
//! parsing the leader IPC framing (4-byte big-endian length prefix + body) so
//! faults land on exact frame boundaries: drop exactly the Nth frame, sever
//! after a half-written length prefix, delay or duplicate one frame. Everything
//! is path-addressed, so no production changes are needed — point
//! `LeaderClient::connect` / `ECHO_AGENT_LEADER_SOCKET` at the proxy path.
//!
//! Frame numbering is 1-based and **per proxied connection, per direction**;
//! reconnects restart the count. Unix-only (the leader transport on Windows is
//! a named pipe, which cannot be interposed this way); gated in `lib.rs`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

/// Which pump direction a [`FaultPlan`] applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FaultDirection {
    #[default]
    ClientToLeader,
    LeaderToClient,
}

/// Frame-indexed fault schedule (1-based, per connection, per direction).
///
/// The default plan is a transparent pass-through.
#[derive(Debug, Clone, Default)]
pub struct FaultPlan {
    /// Direction the frame-indexed faults below apply to; the other direction
    /// always passes through untouched.
    pub direction: FaultDirection,
    /// Silently drop the Nth frame (never forwarded).
    pub drop_frame: Option<u64>,
    /// On the Nth frame, forward only 2 bytes of its 4-byte length prefix,
    /// then hard-close both sides of the connection.
    pub sever_mid_frame: Option<u64>,
    /// Hold the Nth frame for the given duration before forwarding it.
    pub delay: Option<(u64, Duration)>,
    /// Forward the Nth frame twice.
    pub duplicate_frame: Option<u64>,
}

#[derive(Default)]
struct FaultState {
    /// Current sever scope: cancelled + swapped for a fresh token on every
    /// [`FaultHandle::sever_now`], so only connections active at sever time die.
    sever_now: std::sync::Mutex<CancellationToken>,
    /// Frames fully forwarded client→leader across all connections.
    forwarded_c2l: AtomicU64,
    /// Frames fully forwarded leader→client across all connections.
    forwarded_l2c: AtomicU64,
}

/// Runtime control over a running [`UdsProxy`].
#[derive(Clone, Default)]
pub struct FaultHandle {
    state: Arc<FaultState>,
}

impl FaultHandle {
    /// Hard-close every active proxied connection immediately (mid-stream
    /// sever, independent of the frame-indexed plan). Later connections
    /// through the same proxy are unaffected.
    pub fn sever_now(&self) {
        let mut guard = self.state.sever_now.lock().unwrap();
        guard.cancel();
        *guard = CancellationToken::new();
    }

    fn connection_scope(&self) -> CancellationToken {
        self.state.sever_now.lock().unwrap().child_token()
    }

    /// Frames fully forwarded so far in the given direction. Relaxed:
    /// independent counters, no cross-variable ordering to protect.
    pub fn forwarded(&self, direction: FaultDirection) -> u64 {
        match direction {
            FaultDirection::ClientToLeader => self.state.forwarded_c2l.load(Ordering::Relaxed),
            FaultDirection::LeaderToClient => self.state.forwarded_l2c.load(Ordering::Relaxed),
        }
    }
}

/// A running proxy: listener on [`Self::proxy_path`], forwarding to the
/// upstream path it was spawned with. Dropping the struct stops the listener
/// and severs active connections.
pub struct UdsProxy {
    pub proxy_path: PathBuf,
    handle: FaultHandle,
    cancel: CancellationToken,
}

impl UdsProxy {
    /// Bind `proxy_path` and forward each accepted connection to
    /// `upstream_path`, applying `plan` per connection.
    pub async fn spawn(
        proxy_path: impl Into<PathBuf>,
        upstream_path: impl AsRef<Path>,
        plan: FaultPlan,
    ) -> io::Result<Self> {
        let proxy_path = proxy_path.into();
        let upstream_path = upstream_path.as_ref().to_path_buf();
        let _ = std::fs::remove_file(&proxy_path);
        let listener = UnixListener::bind(&proxy_path)?;

        let handle = FaultHandle::default();
        let cancel = CancellationToken::new();

        let accept_handle = handle.clone();
        let accept_cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = accept_cancel.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((client, _)) = accepted else { break };
                        let Ok(upstream) = UnixStream::connect(&upstream_path).await else {
                            // Upstream gone: dropping `client` models a refused
                            // connection; the caller's retry logic takes over.
                            continue;
                        };
                        spawn_connection(client, upstream, plan.clone(), accept_handle.clone());
                    }
                }
            }
        });

        Ok(Self {
            proxy_path,
            handle,
            cancel,
        })
    }

    pub fn handle(&self) -> FaultHandle {
        self.handle.clone()
    }

    /// Stop accepting and sever active connections.
    pub fn shutdown(&self) {
        self.handle.sever_now();
        self.cancel.cancel();
    }
}

impl Drop for UdsProxy {
    fn drop(&mut self) {
        self.shutdown();
        let _ = std::fs::remove_file(&self.proxy_path);
    }
}

fn spawn_connection(
    client: UnixStream,
    upstream: UnixStream,
    plan: FaultPlan,
    handle: FaultHandle,
) {
    let (client_read, client_write) = tokio::io::split(client);
    let (upstream_read, upstream_write) = tokio::io::split(upstream);

    // One sever scope per connection: a mid-frame sever (or `sever_now`)
    // cancels BOTH pumps so the two half-connections drop together.
    let conn_cancel = handle.connection_scope();

    let c2l_plan = (plan.direction == FaultDirection::ClientToLeader).then(|| plan.clone());
    let l2c_plan = (plan.direction == FaultDirection::LeaderToClient).then_some(plan);

    let c2l_counter = handle.clone();
    let c2l_cancel = conn_cancel.clone();
    tokio::spawn(async move {
        pump_frames(
            client_read,
            upstream_write,
            c2l_plan,
            FaultDirection::ClientToLeader,
            c2l_counter,
            c2l_cancel,
        )
        .await;
    });

    let l2c_counter = handle;
    tokio::spawn(async move {
        pump_frames(
            upstream_read,
            client_write,
            l2c_plan,
            FaultDirection::LeaderToClient,
            l2c_counter,
            conn_cancel,
        )
        .await;
    });
}

/// Pump length-prefixed frames from `reader` to `writer`, applying `plan`
/// (when `Some`) to this direction. Ends on EOF, IO error, or sever.
async fn pump_frames(
    mut reader: ReadHalf<UnixStream>,
    mut writer: WriteHalf<UnixStream>,
    plan: Option<FaultPlan>,
    direction: FaultDirection,
    handle: FaultHandle,
    cancel: CancellationToken,
) {
    let mut frame_index: u64 = 0;
    loop {
        let frame = tokio::select! {
            _ = cancel.cancelled() => break,
            frame = read_frame(&mut reader) => frame,
        };
        let Ok((len_prefix, body)) = frame else {
            break;
        };
        frame_index += 1;

        if let Some(ref plan) = plan {
            if plan.drop_frame == Some(frame_index) {
                continue;
            }
            if plan.sever_mid_frame == Some(frame_index) {
                // Half a length prefix, then a hard close of the whole
                // connection: the reader sees a short read, never a body.
                let _ = writer.write_all(&len_prefix[..2]).await;
                let _ = writer.flush().await;
                cancel.cancel();
                break;
            }
            if let Some((nth, duration)) = plan.delay
                && nth == frame_index
            {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(duration) => {}
                }
            }
            let copies = if plan.duplicate_frame == Some(frame_index) {
                2
            } else {
                1
            };
            for _ in 0..copies {
                if write_frame(&mut writer, &len_prefix, &body).await.is_err() {
                    return;
                }
                bump_forwarded(&handle, direction);
            }
            continue;
        }

        if write_frame(&mut writer, &len_prefix, &body).await.is_err() {
            return;
        }
        bump_forwarded(&handle, direction);
    }
}

fn bump_forwarded(handle: &FaultHandle, direction: FaultDirection) {
    match direction {
        FaultDirection::ClientToLeader => {
            handle.state.forwarded_c2l.fetch_add(1, Ordering::Relaxed);
        }
        FaultDirection::LeaderToClient => {
            handle.state.forwarded_l2c.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Max frame body the proxy will buffer — mirrors the leader transport's own
/// 64 MiB `MAX_MESSAGE_SIZE`, so a corrupt/mis-framed length surfaces as a
/// readable pump error instead of a multi-GiB allocation.
const MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;

async fn read_frame(reader: &mut ReadHalf<UnixStream>) -> io::Result<([u8; 4], Vec<u8>)> {
    let mut len_prefix = [0u8; 4];
    reader.read_exact(&mut len_prefix).await?;
    let len = u32::from_be_bytes(len_prefix) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds MAX_FRAME_SIZE ({MAX_FRAME_SIZE})"),
        ));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    Ok((len_prefix, body))
}

async fn write_frame(
    writer: &mut WriteHalf<UnixStream>,
    len_prefix: &[u8; 4],
    body: &[u8],
) -> io::Result<()> {
    writer.write_all(len_prefix).await?;
    writer.write_all(body).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn client_write_frame(stream: &mut UnixStream, body: &[u8]) {
        stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .await
            .unwrap();
        stream.write_all(body).await.unwrap();
        stream.flush().await.unwrap();
    }

    async fn client_read_frame(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
        let mut len = [0u8; 4];
        stream.read_exact(&mut len).await?;
        let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
        stream.read_exact(&mut body).await?;
        Ok(body)
    }

    /// Upstream that echoes every frame back to the sender.
    fn spawn_echo_upstream(path: PathBuf) {
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    loop {
                        let mut len = [0u8; 4];
                        if stream.read_exact(&mut len).await.is_err() {
                            break;
                        }
                        let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
                        if stream.read_exact(&mut body).await.is_err() {
                            break;
                        }
                        if stream.write_all(&len).await.is_err()
                            || stream.write_all(&body).await.is_err()
                        {
                            break;
                        }
                        let _ = stream.flush().await;
                    }
                });
            }
        });
    }

    #[tokio::test]
    async fn passes_frames_through_untouched() {
        let temp = TempDir::new().unwrap();
        let upstream_path = temp.path().join("real.sock");
        spawn_echo_upstream(upstream_path.clone());
        let proxy = UdsProxy::spawn(
            temp.path().join("proxy.sock"),
            &upstream_path,
            FaultPlan::default(),
        )
        .await
        .unwrap();

        let mut client = UnixStream::connect(&proxy.proxy_path).await.unwrap();
        for payload in [b"one".as_slice(), b"two", b"three"] {
            client_write_frame(&mut client, payload).await;
            assert_eq!(client_read_frame(&mut client).await.unwrap(), payload);
        }
        assert_eq!(proxy.handle().forwarded(FaultDirection::ClientToLeader), 3);
        assert_eq!(proxy.handle().forwarded(FaultDirection::LeaderToClient), 3);
    }

    #[tokio::test]
    async fn drops_exactly_the_nth_frame() {
        let temp = TempDir::new().unwrap();
        let upstream_path = temp.path().join("real.sock");
        spawn_echo_upstream(upstream_path.clone());
        let proxy = UdsProxy::spawn(
            temp.path().join("proxy.sock"),
            &upstream_path,
            FaultPlan {
                drop_frame: Some(2),
                ..FaultPlan::default()
            },
        )
        .await
        .unwrap();

        let mut client = UnixStream::connect(&proxy.proxy_path).await.unwrap();
        client_write_frame(&mut client, b"first").await;
        client_write_frame(&mut client, b"second").await;
        client_write_frame(&mut client, b"third").await;

        // The echo of "second" never arrives; "third" comes straight after "first".
        assert_eq!(client_read_frame(&mut client).await.unwrap(), b"first");
        assert_eq!(client_read_frame(&mut client).await.unwrap(), b"third");
    }

    #[tokio::test]
    async fn duplicates_exactly_the_nth_frame() {
        let temp = TempDir::new().unwrap();
        let upstream_path = temp.path().join("real.sock");
        spawn_echo_upstream(upstream_path.clone());
        let proxy = UdsProxy::spawn(
            temp.path().join("proxy.sock"),
            &upstream_path,
            FaultPlan {
                duplicate_frame: Some(1),
                ..FaultPlan::default()
            },
        )
        .await
        .unwrap();

        let mut client = UnixStream::connect(&proxy.proxy_path).await.unwrap();
        client_write_frame(&mut client, b"once").await;
        assert_eq!(client_read_frame(&mut client).await.unwrap(), b"once");
        assert_eq!(client_read_frame(&mut client).await.unwrap(), b"once");
    }

    #[tokio::test]
    async fn severs_mid_frame_and_closes_both_sides() {
        let temp = TempDir::new().unwrap();
        let upstream_path = temp.path().join("real.sock");
        spawn_echo_upstream(upstream_path.clone());
        let proxy = UdsProxy::spawn(
            temp.path().join("proxy.sock"),
            &upstream_path,
            FaultPlan {
                sever_mid_frame: Some(1),
                ..FaultPlan::default()
            },
        )
        .await
        .unwrap();

        let mut client = UnixStream::connect(&proxy.proxy_path).await.unwrap();
        client_write_frame(&mut client, b"never-delivered").await;

        // The upstream got 2 bytes of a length prefix and then a close, so it
        // echoes nothing; the client's next read observes the sever.
        let read = client_read_frame(&mut client).await;
        assert!(
            read.is_err(),
            "sever must close the client side, got {read:?}"
        );
    }

    #[tokio::test]
    async fn delays_exactly_the_nth_frame() {
        let temp = TempDir::new().unwrap();
        let upstream_path = temp.path().join("real.sock");
        spawn_echo_upstream(upstream_path.clone());
        let delay = Duration::from_millis(300);
        let proxy = UdsProxy::spawn(
            temp.path().join("proxy.sock"),
            &upstream_path,
            FaultPlan {
                delay: Some((1, delay)),
                ..FaultPlan::default()
            },
        )
        .await
        .unwrap();

        let mut client = UnixStream::connect(&proxy.proxy_path).await.unwrap();
        let started = std::time::Instant::now();
        client_write_frame(&mut client, b"held").await;
        assert_eq!(client_read_frame(&mut client).await.unwrap(), b"held");
        assert!(
            started.elapsed() >= delay,
            "frame must be held for the configured delay"
        );

        // Only the Nth frame is delayed; the next one is immediate.
        let started = std::time::Instant::now();
        client_write_frame(&mut client, b"quick").await;
        assert_eq!(client_read_frame(&mut client).await.unwrap(), b"quick");
        assert!(started.elapsed() < delay);
    }

    #[tokio::test]
    async fn sever_now_drops_active_connections() {
        let temp = TempDir::new().unwrap();
        let upstream_path = temp.path().join("real.sock");
        spawn_echo_upstream(upstream_path.clone());
        let proxy = UdsProxy::spawn(
            temp.path().join("proxy.sock"),
            &upstream_path,
            FaultPlan::default(),
        )
        .await
        .unwrap();

        let mut client = UnixStream::connect(&proxy.proxy_path).await.unwrap();
        client_write_frame(&mut client, b"alive").await;
        assert_eq!(client_read_frame(&mut client).await.unwrap(), b"alive");

        proxy.handle().sever_now();
        let read = client_read_frame(&mut client).await;
        assert!(
            read.is_err(),
            "sever_now must close the proxied connection, got {read:?}"
        );
    }
}
