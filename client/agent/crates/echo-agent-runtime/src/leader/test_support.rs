//! In-crate fake leaders for exercising client-side handling of misbehaving
//! leaders (hung, half-framed, wrong-versioned) — wire shapes the real
//! `spawn_leader_server` can never produce.
//!
//! All stalls are cancellation-based (`cancel.cancelled().await`), never
//! timer-based, so `#[tokio::test(start_paused = true)]` auto-advance jumps
//! the client-side timeouts under test without waking the fake.
use super::protocol::{
    ClientMessage, LEADER_PROTOCOL_VERSION, LeaderCapabilities, ServerMessage, read_message,
    write_message,
};
use std::fs;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
/// Version metadata a fake leader reports in `Registered`.
pub(crate) struct FakeVersions {
    pub(crate) protocol_version: Option<u32>,
    pub(crate) binary_version: Option<String>,
}
impl FakeVersions {
    /// The versions a same-build real leader would report (`run_leader` stamps
    /// `echo_agent_version::VERSION` into its metadata).
    pub(crate) fn current() -> Self {
        Self {
            protocol_version: Some(LEADER_PROTOCOL_VERSION),
            binary_version: Some(echo_agent_version::VERSION.to_string()),
        }
    }
}
/// `LeaderCapabilities` has no `Default` (serde-only defaults), so fakes build
/// their capability shape through this helper.
pub(crate) fn fake_caps(control_v1: bool, relaunch_v1: bool) -> LeaderCapabilities {
    LeaderCapabilities {
        control_v1,
        runtime_cpu_profile: false,
        profile_formats: Vec::new(),
        workspace_exposure: false,
        relaunch_v1,
    }
}
/// Wire behavior of a [`spawn_fake_leader`] instance.
pub(crate) enum FakeLeaderBehavior {
    /// Well-formed: `Registered { ready: true }` with the given metadata, then
    /// idle until cancelled. Backs the discovery and adopt/evict tests; also
    /// the composition point for metadata skew (wrong protocol version, stale
    /// binary version) via an explicit [`FakeVersions`] — no sugar variants.
    Normal {
        versions: FakeVersions,
        caps: LeaderCapabilities,
    },
    /// Accepts the connection but never sends anything (hung pre-`Registered`).
    SilentAfterAccept,
    /// `Registered { ready: false }`, then never sends `LeaderReady`.
    ReadyFalseForever,
    /// Writes only `bytes` (< 4) of the 4-byte length prefix, then stalls.
    PartialFrame { bytes: usize },
    /// Valid length prefix followed by a non-JSON body.
    GarbageFrame,
    /// Well-formed `Registered { ready: true }`, then closes the connection.
    CloseAfterRegister,
}
/// Handle for a running fake leader; cancelling stops the accept loop and any
/// held-open connections, and removes the socket.
pub(crate) struct FakeLeaderHandle {
    cancel: CancellationToken,
}
impl FakeLeaderHandle {
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }
}
/// Bind a fake leader at `socket_path` behaving per `behavior`.
///
/// Returns once the listener is bound (readiness signalled via oneshot, no
/// fixed startup sleep), so callers can connect immediately. Serves clients
/// sequentially: the point of a fake is wire shape, not concurrency.
pub(crate) async fn spawn_fake_leader(
    socket_path: PathBuf,
    behavior: FakeLeaderBehavior,
) -> FakeLeaderHandle {
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = fs::remove_file(&socket_path);
        let listener = match super::transport::LeaderListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(_) => return,
        };
        let _ = ready_tx.send(());
        loop {
            tokio::select! {
                _ = cancel_clone.cancelled() => break,
                accept_result = listener.accept() => {
                    let Ok((stream, _)) = accept_result else {
                        break;
                    };
                    serve_client(stream, &behavior, &cancel_clone).await;
                }
            }
        }
        let _ = fs::remove_file(&socket_path);
    });
    let _ = ready_rx.await;
    FakeLeaderHandle { cancel }
}
async fn serve_client(
    stream: super::transport::LeaderStream,
    behavior: &FakeLeaderBehavior,
    cancel: &CancellationToken,
) {
    let (mut reader, mut writer) = tokio::io::split(stream);
    /// A `Registered` with `client_id: 1` and the given shape.
    fn registered(
        ready: bool,
        versions: &FakeVersions,
        caps: &LeaderCapabilities,
    ) -> ServerMessage {
        ServerMessage::Registered {
            client_id: 1,
            ready,
            leader_protocol_version: versions.protocol_version,
            leader_binary_version: versions.binary_version.clone(),
            leader_capabilities: Some(caps.clone()),
        }
    }
    match behavior {
        FakeLeaderBehavior::SilentAfterAccept => {
            cancel.cancelled().await;
        }
        FakeLeaderBehavior::PartialFrame { bytes } => {
            let prefix = 1024u32.to_be_bytes();
            let n = (*bytes).min(prefix.len());
            let _ = writer.write_all(&prefix[..n]).await;
            let _ = writer.flush().await;
            cancel.cancelled().await;
        }
        FakeLeaderBehavior::GarbageFrame => {
            let body = b"this is not json";
            let _ = writer.write_all(&(body.len() as u32).to_be_bytes()).await;
            let _ = writer.write_all(body).await;
            let _ = writer.flush().await;
            cancel.cancelled().await;
        }
        FakeLeaderBehavior::Normal { versions, caps } => {
            let register: Result<ClientMessage, _> = read_message(&mut reader).await;
            if register.is_err() {
                return;
            }
            let _ = write_message(&mut writer, &registered(true, versions, caps)).await;
            cancel.cancelled().await;
        }
        FakeLeaderBehavior::ReadyFalseForever => {
            let register: Result<ClientMessage, _> = read_message(&mut reader).await;
            if register.is_err() {
                return;
            }
            let _ = write_message(
                &mut writer,
                &registered(false, &FakeVersions::current(), &fake_caps(true, false)),
            )
            .await;
            cancel.cancelled().await;
        }
        FakeLeaderBehavior::CloseAfterRegister => {
            let register: Result<ClientMessage, _> = read_message(&mut reader).await;
            if register.is_err() {
                return;
            }
            let _ = write_message(
                &mut writer,
                &registered(true, &FakeVersions::current(), &fake_caps(true, false)),
            )
            .await;
        }
    }
}
