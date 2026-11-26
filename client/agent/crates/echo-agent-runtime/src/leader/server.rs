use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
/// The binary version of the currently running leader process.
///
/// Compared against each registering client's `ClientCapabilities::client_version`
/// to detect mismatches early and surface a structured ACP notification.
/// In development builds where `VERSION_WITH_COMMIT` is not set, this is
/// `"unknown"` and version-mismatch detection is disabled (no notification sent).
const LEADER_VERSION: &str = match option_env!("VERSION_WITH_COMMIT") {
    Some(v) => v,
    None => "unknown",
};
use super::protocol::{
    ClientCapabilities, ClientId, ClientMessage, ClientMode, ControlCommand, ControlPayload,
    InternalMethod, LEADER_PROTOCOL_VERSION, LeaderCapabilities, ProtocolError, ServerMessage,
    internal_notification, read_message, write_message,
};
use super::transport::{LeaderListener, LeaderStream};
use crate::agent::activity::AgentActivity;
use crate::auth::AuthManager;
use crate::cpu_profile::{
    ControlError, ControlErrorCode, CpuProfileManager, CpuProfileStartOptions, CpuProfileStatus,
    ShutdownStopDisposition,
};
use agent_client_protocol::AGENT_METHOD_NAMES;
use echo_agent_computer_hub_sdk::{AuthCredential, AuthIdentity, AuthProvider};
use echo_agent_workspace::WorkspaceHandle;
use kanal::{AsyncReceiver, AsyncSender};
use parking_lot::Mutex;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(30);
/// Separator for namespacing request IDs. Using pipe character which is:
/// - Valid in JSON strings (no escaping needed)
/// - Unlikely to appear in typical JSON-RPC IDs (usually numbers or UUIDs)
const ID_NAMESPACE_SEP: char = '|';
/// Cap on live notifications buffered per in-flight `session/load` (see
/// `load_live_buffer`). A normal load resolves in well under a second, so the
/// buffer is tiny; this bound just prevents unbounded growth if a load stalls.
/// On overflow we stop buffering and forward live normally (correctness of the
/// transcript is preserved by the client's eventId dedup; only the ordering
/// nicety is lost in this degenerate case).
const MAX_BUFFERED_LIVE_PER_LOAD: usize = 4096;
enum ServerEvent {
    Disconnected(ClientId),
    Registered(ClientId, ClientMode, ClientCapabilities, String),
    Message(ClientId, ClientMessage),
}
enum LeaderServerPoll {
    Cancelled,
    Accept(std::io::Result<LeaderStream>),
    Event(ServerEvent),
    Response(String),
}
/// A live notification buffered during an in-flight `session/load`: the
/// shared payload plus its `event_seq` (computed at buffer time, when the
/// message is already parsed, so the post-load flush never re-parses).
type BufferedLive = (Arc<str>, Option<u64>);
/// Message queued to a client handler task.
///
/// ACP payloads are by far the hot path (every chunk of every session fans out
/// to every subscriber), so they ride as a shared `Arc<str>`: the routing loop
/// pays one refcount bump per recipient instead of a full `String` clone, and
/// the live-load buffer / interaction cache share the same allocation. The
/// handler serializes the wire envelope via [`ServerMessageRef`] without ever
/// materializing an owned `ServerMessage::Acp`.
#[derive(Debug, Clone)]
enum ClientOutbound {
    /// An ACP payload, shared (refcounted) across fan-out targets.
    Acp(Arc<str>),
    /// Everything else (registration, control results, ping, shutdown, errors).
    Message(ServerMessage),
}
impl From<ServerMessage> for ClientOutbound {
    fn from(msg: ServerMessage) -> Self {
        Self::Message(msg)
    }
}
/// Serialize-only mirror of [`ServerMessage`]'s `Acp` variant that borrows the
/// payload, so the per-client writer can frame a shared `Arc<str>` without
/// copying it into an owned `ServerMessage`. Must stay wire-identical to
/// `ServerMessage::Acp` — asserted by the
/// `server_message_ref_is_wire_identical` test.
#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessageRef<'a> {
    Acp { payload: &'a str },
}
/// Write one [`ClientOutbound`] to a client connection.
async fn write_outbound<W>(writer: &mut W, msg: &ClientOutbound) -> Result<(), ProtocolError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg {
        ClientOutbound::Acp(payload) => {
            write_message(writer, &ServerMessageRef::Acp { payload }).await
        }
        ClientOutbound::Message(m) => write_message(writer, m).await,
    }
}
struct ClientState {
    tx: AsyncSender<ClientOutbound>,
    mode: ClientMode,
    capabilities: ClientCapabilities,
    /// The client type string from IPC registration (e.g., "echo-agent-tui", "echo-agent-code-extension").
    /// Injected into `initialize` requests as `clientIdentifier` so the agent knows the real
    /// client type even when multiple clients share one leader process.
    client_type: String,
    /// Set to `true` once the client's `initialize` request has been seen and had
    /// `clientIdentifier` injected. Until `initialize` is observed (regardless of how many
    /// earlier messages arrived), each ACP message is checked so we never miss a late
    /// `initialize`. After it is seen once, we skip the per-message parse as an optimisation.
    initialize_seen: bool,
    /// Patch the next response's `modelState.currentModelId` to match `default_model`.
    /// Set on outbound `initialize`, cleared after patching the response.
    patch_initialize_model: bool,
    /// Whether this client has completed IPC registration. Used to keep `client_count`
    /// accurate — only registered clients are counted, so pre-registration connections
    /// (which may time out) don't inflate the count and block auto-updates.
    registered: bool,
}
#[derive(Debug, Clone)]
pub struct LeaderServerMetadata {
    pub pid: u32,
    pub socket_path: PathBuf,
    pub lock_path: PathBuf,
    pub ws_url_suffix: String,
    pub leader_binary_version: String,
}
#[derive(Debug, Clone)]
pub struct LeaderServerControlState {
    pub metadata: LeaderServerMetadata,
    pub cpu_profile: Arc<Mutex<CpuProfileManager>>,
    pub workspace: Arc<WorkspaceControl>,
}
impl LeaderServerControlState {
    pub fn new(metadata: LeaderServerMetadata) -> Self {
        Self {
            metadata,
            cpu_profile: Arc::new(Mutex::new(CpuProfileManager::new())),
            workspace: Arc::new(WorkspaceControl::new(None)),
        }
    }
    pub(crate) fn with_default_hub_url(mut self, default_hub_url: Option<String>) -> Self {
        self.workspace = Arc::new(WorkspaceControl::new(default_hub_url));
        self
    }
    fn leader_capabilities(&self) -> LeaderCapabilities {
        let manager = self.cpu_profile.lock();
        LeaderCapabilities {
            control_v1: true,
            runtime_cpu_profile: manager.runtime_cpu_profile(),
            profile_formats: manager.profile_formats().to_vec(),
            workspace_exposure: true,
            relaunch_v1: true,
        }
    }
}
pub struct WorkspaceControl {
    default_hub_url: Option<String>,
    /// Hub credential, wired to the leader's `AuthManager` once auth is ready.
    /// A `watch` so a starting leader (socket up, auth pending) can be awaited
    /// instead of failing the command.
    auth: tokio::sync::watch::Sender<Option<Arc<dyn AuthProvider>>>,
    /// Serializes mutating commands (start/pause/resume/stop) so their long
    /// awaits (drain, reconnect) never interleave.
    lock: tokio::sync::Mutex<()>,
    /// Current exposure, published for lock-free reads so `status` never
    /// blocks behind an in-flight drain/reconnect.
    exposure: arc_swap::ArcSwapOption<WorkspaceExposure>,
}
impl WorkspaceControl {
    fn new(default_hub_url: Option<String>) -> Self {
        Self {
            default_hub_url,
            auth: tokio::sync::watch::channel(None).0,
            lock: tokio::sync::Mutex::new(()),
            exposure: arc_swap::ArcSwapOption::empty(),
        }
    }
    /// Wire the hub credential to the leader's shared `AuthManager` (sole
    /// owner of refresh + persistence).
    pub(crate) fn set_auth_manager(&self, auth_manager: Arc<AuthManager>) {
        self.auth.send_replace(Some(Arc::new(LeaderAuthProvider {
            auth_manager,
            refresh_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })));
    }
}
impl std::fmt::Debug for WorkspaceControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceControl")
            .field("default_hub_url", &self.default_hub_url)
            .finish_non_exhaustive()
    }
}
/// Hub [`AuthProvider`] backed by the leader's `AuthManager`: returns the
/// current token at each connect/reconnect; never writes auth.json.
struct LeaderAuthProvider {
    auth_manager: Arc<AuthManager>,
    /// One background refresh at a time. `current()` is called by a reconnect
    /// loop that can spin fast while offline; `refresh_lock` would serialize
    /// those tasks but not collapse them, so each queued one would still issue
    /// its own IdP call once the previous released.
    refresh_in_flight: Arc<std::sync::atomic::AtomicBool>,
}
impl std::fmt::Debug for LeaderAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaderAuthProvider").finish_non_exhaustive()
    }
}
impl AuthProvider for LeaderAuthProvider {
    fn current(&self) -> AuthCredential {
        use std::sync::atomic::Ordering;
        let cached = self.auth_manager.current();
        if cached.is_none()
            && self.auth_manager.is_expired()
            && self
                .refresh_in_flight
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            struct ClaimGuard(Arc<std::sync::atomic::AtomicBool>);
            impl Drop for ClaimGuard {
                fn drop(&mut self) {
                    self.0.store(false, std::sync::atomic::Ordering::Release);
                }
            }
            let guard = ClaimGuard(Arc::clone(&self.refresh_in_flight));
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let am = Arc::clone(&self.auth_manager);
                handle.spawn(async move {
                    let _guard = guard;
                    if let Err(e) = am.auth().await {
                        tracing::debug!(error = %e, "leader hub auth: background refresh failed");
                    }
                });
            }
        }
        let token = cached
            .or_else(|| self.auth_manager.current_or_expired())
            .map(|a| a.key)
            .unwrap_or_default();
        AuthCredential::bearer(token)
    }
    /// Owner identity from the leader's `AuthManager`, so the workspace derives
    /// `WorkspaceIdentity` from this provider instead of a separate auth.json
    /// read. Mirrors the in-process path (`mvp_agent`): prefer `EchoAgentAuth.team_id`
    /// (what shell telemetry/snapshot use) mapped onto a `"Team"` principal so
    /// team attribution is derived; otherwise pass principal fields through.
    /// `None` when no credential is available (identity resolution never blocks).
    fn identity(&self) -> Option<AuthIdentity> {
        let a = self.auth_manager.current_or_expired()?;
        Some(match a.team_id.filter(|t| !t.is_empty()) {
            Some(team) => AuthIdentity {
                user_id: a.user_id,
                principal_type: Some("Team".to_string()),
                principal_id: Some(team),
            },
            None => AuthIdentity {
                user_id: a.user_id,
                principal_type: a.principal_type,
                principal_id: a.principal_id,
            },
        })
    }
}
struct WorkspaceExposure {
    handle: WorkspaceHandle,
    hub_url: String,
    cwd: PathBuf,
    started_at: Instant,
    paused: std::sync::atomic::AtomicBool,
}
/// Rewrite JSON-RPC request ID **in place** by prefixing with client ID to
/// avoid collisions.
///
/// Uses a pipe separator which is valid in JSON but unlikely to appear in
/// typical JSON-RPC IDs (which are usually numbers or UUIDs).
///
/// Only rewrites IDs for **requests** (messages with a "method" field).
/// Responses (messages with "result" or "error" but no "method") are left
/// untouched so the agent can match them to its pending requests.
///
/// Returns `Some((namespaced_id, original_id))` when the message is a request
/// carrying an ID (and `json` was mutated); `None` otherwise (no mutation).
/// The returned `namespaced_id` lets the caller key per-request state (e.g.
/// `pending_load_by_req`) without re-parsing the rewritten payload.
fn rewrite_request_id(
    json: &mut serde_json::Value,
    client_id: ClientId,
) -> Option<(String, serde_json::Value)> {
    json.get("method")?;
    let original_id = json.get("id").cloned()?;
    let original_json = serde_json::to_string(&original_id).unwrap_or_default();
    let namespaced_id = format!("{}{}{}", client_id.0, ID_NAMESPACE_SEP, original_json);
    json["id"] = serde_json::json!(namespaced_id);
    Some((namespaced_id, original_id))
}
/// Parse a namespaced response ID to find the target client, restoring the
/// original ID **in place**.
///
/// Expects format: "client_id|original_id_json" where original_id_json is the
/// JSON-serialized form of the original ID (preserving type information).
///
/// Returns `Some((client_id, namespaced_id))` if successful (`json` now
/// carries the restored original ID). The returned `namespaced_id` is the raw
/// pre-restore ID, so the caller can match per-request state (e.g.
/// `pending_load_by_req`) without re-parsing the original payload. On `None`,
/// `json` is untouched.
fn parse_response_id(json: &mut serde_json::Value) -> Option<(ClientId, String)> {
    let id = json.get("id")?;
    let id_str = id.as_str()?;
    let (client_part, original_json) = id_str.split_once(ID_NAMESPACE_SEP)?;
    let client_id: u64 = client_part.parse().ok()?;
    let original_id: serde_json::Value = serde_json::from_str(original_json).ok()?;
    let namespaced_id = id_str.to_string();
    json["id"] = original_id;
    Some((ClientId(client_id), namespaced_id))
}
/// Extract session_id from a message's params (for session-based routing).
fn extract_session_id(json: &serde_json::Value) -> Option<String> {
    let params = json.get("params")?;
    params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            params
                .get("params")
                .and_then(|inner| inner.get("sessionId").or_else(|| inner.get("session_id")))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
}
/// Whether a payload attaches an existing session (`session/load` or
/// `session/resume`): both need live-broadcast buffering until the response
/// (see `load_live_buffer`) and the pending-modal replay keyed on it.
fn is_session_attach_request(json: &serde_json::Value) -> bool {
    json.get("method")
        .and_then(|m| m.as_str())
        .is_some_and(|m| m == "session/load" || m == "session/resume")
}
/// Extract the leader unicast target `ClientId` from a notification's
/// `params._meta["echo.agent/leaderClientId"]`.
///
/// The agent stamps this onto every `session/load` replay notification (echoing
/// the id the leader injected into the load request) so the replay can be routed
/// back to ONLY the loading client instead of broadcasting to all subscribers.
/// Live (non-replay) turn deltas are never tagged, so they keep broadcasting.
fn extract_target_client_id(json: &serde_json::Value) -> Option<ClientId> {
    let params = json.get("params")?;
    params
        .get("_meta")
        .and_then(|m| m.get("echo.agent/leaderClientId"))
        .or_else(|| {
            params
                .get("params")
                .and_then(|inner| inner.get("_meta"))
                .and_then(|m| m.get("echo.agent/leaderClientId"))
        })
        .and_then(|v| v.as_u64())
        .map(ClientId)
}
/// Extract the monotonic `event_seq` counter from a notification's
/// `_meta.eventId` (format `"{sessionId}-{counter}"`). Mirrors the `_meta`
/// lookup in [`extract_target_client_id`] (also checks the ExtNotification
/// `params.params` nesting) and the suffix parse used by
/// `session::storage` and the client's `acp::meta`. Returns `None` for
/// notifications without an `eventId` (EchoAgent one-shots / older shell).
fn event_seq_of(json: &serde_json::Value) -> Option<u64> {
    let params = json.get("params")?;
    let event_id = params
        .get("_meta")
        .and_then(|m| m.get("eventId"))
        .or_else(|| {
            params
                .get("params")
                .and_then(|inner| inner.get("_meta"))
                .and_then(|m| m.get("eventId"))
        })
        .and_then(|v| v.as_str())?;
    event_id.rsplit_once('-')?.1.parse::<u64>().ok()
}
/// Whether a payload is a machine-wide notification (no `sessionId`) that
/// must be **broadcast to every client** instead of falling through to the
/// last-active-client fallback:
///
/// - `echo.agent/sessions/changed` — roster delta; every open dashboard must stay
///   in sync.
/// - `echo.agent/models/update` — the model catalog changed (config.toml
///   `[model.*]`/`[models]` hot-reload, `models_cache.json` external write,
///   auth change, response-header etag refresh). Every connected client's
///   model picker must refresh, not just the most recently active one.
/// - `echo.agent/mcp/servers_updated` — the MCP catalog resolved/changed (managed
///   connectors fetched in the background after `initialize`). Deliberately
///   session-agnostic on the wire (no `sessionId`, see
///   `extensions::mcp::notify_servers_updated`); the push fires seconds after
///   `initialize` returns, so last-active-client fallback routinely delivered
///   it to the wrong client (or dropped it) in multi-client leaders — managed
///   connectors then "disappeared" from every other client's `/mcp` view.
///   Broadcast is safe: the pager handler only debounce-refetches `mcp/list`
///   for agents with an open extensions modal.
/// - `echo.agent/announcements/update` — the announcements list changed (startup
///   one-shot or the periodic settings refresh). Session-agnostic; every
///   client renders its own banner, so last-active-client fallback would
///   leave every other client's banner stale. Broadcast is safe: the pager
///   handler is idempotent and drops stale generations via its `gen` gate.
///   (`echo.agent/settings/update` stays non-broadcast — it carries auth/gate state.)
///
/// Matched via [`method_of`], NOT the raw top-level `method`: agent ext
/// notifications arrive `_`-prefixed on the wire (`_echo.agent/sessions/changed`),
/// so a raw compare would miss the production form.
fn is_machine_wide_broadcast_notification(json: &serde_json::Value) -> bool {
    matches!(
        method_of(json),
        Some(
            "echo.agent/sessions/changed"
                | "echo.agent/models/update"
                | "echo.agent/mcp/servers_updated"
                | "echo.agent/announcements/update"
        )
    )
}
/// Whether a payload is the `echo.agent/scheduled_task_inject_prompt` notification.
///
/// This notification tells the receiving client to enqueue AND drive a
/// scheduled (`/loop`) cron prompt. Unlike ordinary `sessionId`-bearing
/// notifications (which fan out to every subscriber so each renders an
/// identical stream), it must be routed to the SINGLE session driver: if every
/// attached client received it, each would enqueue + try to drive the same cron
/// turn, duplicating it (phantom `#N` queue entries, competing drivers, stuck
/// turns). The other clients render the resulting turn from the broadcast
/// `session/update` deltas, exactly like any other turn the driver runs.
/// The namespaced method a leader payload carries, normalizing the two ext wire
/// forms the gateway produces:
///   - direct:  `{"method":"echo.agent/foo", ...}`                                 -> `echo.agent/foo`
///   - wrapped: `{"method":"_echo.agent/foo","params":{"method":"echo.agent/foo",...}}`  -> `echo.agent/foo`
///
/// Gateway-forwarded ext methods/notifications (`ext_method` / `ext_notification`
/// — e.g. `ask_user_question`, `exit_plan_mode`, `scheduled_task_inject_prompt`,
/// `session_notification`) arrive WRAPPED: a top-level `_`-prefixed method with
/// the real method + params nested one level under `params`. Plain methods
/// (`session/request_permission`, `session/update`, …) arrive direct. Anything
/// that classifies a payload by method name MUST use this — matching the raw
/// top-level `method` misses the wrapped form. See `interaction_inner_params`
/// for the matching params accessor.
fn method_of(json: &serde_json::Value) -> Option<&str> {
    let top = json.get("method")?.as_str()?;
    if let Some(stripped) = top.strip_prefix('_') {
        return Some(
            json.get("params")
                .and_then(|p| p.get("method"))
                .and_then(|m| m.as_str())
                .unwrap_or(stripped),
        );
    }
    Some(top)
}
/// The real params object for a payload, unwrapping the gateway ext wrapper:
/// for a wrapped ext (its `params` carries its own `method` + nested `params`)
/// the real params live at `params.params`; otherwise `params` is already real.
fn interaction_inner_params(json: &serde_json::Value) -> Option<&serde_json::Value> {
    let params = json.get("params")?;
    if params.get("method").is_some()
        && let Some(inner) = params.get("params")
    {
        Some(inner)
    } else {
        Some(params)
    }
}
/// Whether a payload is the `echo.agent/scheduled_task_inject_prompt` notification.
///
/// This notification tells the receiving client to enqueue AND drive a
/// scheduled (`/loop`) cron prompt. Unlike ordinary `sessionId`-bearing
/// notifications (which fan out to every subscriber so each renders an
/// identical stream), it must be routed to the SINGLE session driver: if every
/// attached client received it, each would enqueue + try to drive the same cron
/// turn, duplicating it (phantom `#N` queue entries, competing drivers, stuck
/// turns). The other clients render the resulting turn from the broadcast
/// `session/update` deltas, exactly like any other turn the driver runs.
fn is_scheduled_task_inject_prompt(json: &serde_json::Value) -> bool {
    method_of(json) == Some("echo.agent/scheduled_task_inject_prompt")
}
/// Whether a payload is a blocking *interaction* reverse-request — a tool
/// permission, `ask_user_question`, or plan-approval. Unlike other
/// reverse-requests (driver-only), these are **shared**: broadcast to every
/// subscriber so any client can render + answer the modal, first-answer-wins.
/// See `SHARED_INTERACTIVE_MODALS.md`.
fn is_interaction_request(json: &serde_json::Value) -> bool {
    matches!(
        method_of(json),
        Some(
            "session/request_permission"
                | "echo.agent/ask_user_question"
                | "echo.agent/exit_plan_mode"
                | "echo.agent/mcp/elicit",
        )
    )
}
/// Extract the `tool_call_id` an interaction reverse-request carries, so the
/// leader can cache it (keyed by id) for replay-on-attach and evict it on
/// `InteractionResolved`. The ext-methods (`ask_user_question` /
/// `exit_plan_mode`) carry it directly under (inner) `params`;
/// `request_permission` nests it under `toolCall`. Tolerant of the gateway
/// wrapper (via `interaction_inner_params`) and camel/snake spelling.
fn extract_interaction_tool_call_id(json: &serde_json::Value) -> Option<String> {
    let params = interaction_inner_params(json)?;
    if let Some(id) = params
        .get("toolCallId")
        .or_else(|| params.get("tool_call_id"))
        .and_then(|v| v.as_str())
    {
        return Some(id.to_string());
    }
    let tc = params.get("toolCall").or_else(|| params.get("tool_call"))?;
    tc.get("toolCallId")
        .or_else(|| tc.get("tool_call_id"))
        .or_else(|| tc.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from)
}
/// If a payload is the `InteractionResolved` broadcast (an
/// `echo.agent/session_notification` whose `update.sessionUpdate ==
/// "interaction_resolved"`), return its `tool_call_id` so the leader can evict
/// the cached interaction request (first-answer-wins). Tolerant of the gateway
/// wrapper and camel/snake spelling for the inner field.
fn extract_interaction_resolved_tool_call_id(json: &serde_json::Value) -> Option<String> {
    if method_of(json) != Some("echo.agent/session_notification") {
        return None;
    }
    let update = interaction_inner_params(json)?.get("update")?;
    if update.get("sessionUpdate").and_then(|v| v.as_str()) != Some("interaction_resolved") {
        return None;
    }
    update
        .get("tool_call_id")
        .or_else(|| update.get("toolCallId"))
        .and_then(|v| v.as_str())
        .map(String::from)
}
/// Extract session_id from a prompt-complete notification.
fn extract_session_id_from_prompt_complete(json: &serde_json::Value) -> Option<String> {
    let method = json.get("method")?.as_str()?;
    if method != "echo.agent/session/prompt_complete" {
        return None;
    }
    json.get("params")?
        .get("sessionId")
        .or_else(|| json.get("params")?.get("session_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}
/// Extract session_id from a response's result (for session/new and session/load responses).
/// This is used to track session ownership when the session is first created.
fn extract_session_id_from_result(json: &serde_json::Value) -> Option<String> {
    let result = json.get("result")?;
    result
        .get("session_id")
        .or_else(|| result.get("sessionId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}
#[derive(Debug)]
enum ChildSessionEvent {
    Spawned(String),
    Finished(String),
}
/// Extract child session lifecycle events from subagent notifications.
fn extract_child_session_event(json: &serde_json::Value) -> Option<ChildSessionEvent> {
    let params = json.get("params")?;
    let update = params
        .get("update")
        .or_else(|| params.get("params")?.get("update"))?;
    let child_sid = update.get("child_session_id").and_then(|v| v.as_str())?;
    match update.get("sessionUpdate")?.as_str()? {
        "subagent_spawned" => Some(ChildSessionEvent::Spawned(child_sid.to_string())),
        "subagent_finished" => Some(ChildSessionEvent::Finished(child_sid.to_string())),
        _ => None,
    }
}
/// Drop a finished child's route + driver and detach it from the child forest,
/// RE-PARENTING any still-live grandchildren onto the finished child's own
/// parent. Re-parenting (not cascade-prune) keeps a still-running grandchild
/// of a finished intermediate reachable from the root: `backfill_child_routes`
/// only follows forward edges, so a subtree left dangling under the removed
/// child would never be reached on a root `session/load`. A genuinely-dead leaf
/// (no surviving children) is simply removed.
///
/// The current parent is found by searching the forest — not taken from the
/// finish notification's sessionId — so a grandchild already re-parented by an
/// earlier intermediate finish is still detached from its correct edge.
fn prune_child_route(
    child_sid: &str,
    session_subscribers: &mut HashMap<String, HashSet<ClientId>>,
    session_driver: &mut HashMap<String, ClientId>,
    child_sessions: &mut HashMap<String, HashSet<String>>,
) {
    session_subscribers.remove(child_sid);
    session_driver.remove(child_sid);
    let parent = child_sessions
        .iter()
        .find_map(|(p, kids)| kids.contains(child_sid).then(|| p.clone()));
    if let Some(ref parent) = parent
        && let Some(kids) = child_sessions.get_mut(parent)
    {
        kids.remove(child_sid);
    }
    if let Some(grandchildren) = child_sessions.remove(child_sid)
        && let Some(ref parent) = parent
    {
        child_sessions
            .entry(parent.clone())
            .or_default()
            .extend(grandchildren);
    }
    if let Some(parent) = parent
        && child_sessions.get(&parent).is_some_and(HashSet::is_empty)
    {
        child_sessions.remove(&parent);
    }
}
/// Subscribe `client` to every live descendant of `parent` (walking the
/// parent→children index, depth-safe via a visited set) and give driverless
/// descendants the parent's driver. Child routes are otherwise spawn-time
/// snapshots, so without this a client that attaches to the parent AFTER a
/// subagent spawned (late attach, reconnect) never receives the child's live
/// updates.
fn backfill_child_routes(
    parent: &str,
    client: ClientId,
    child_sessions: &HashMap<String, HashSet<String>>,
    session_subscribers: &mut HashMap<String, HashSet<ClientId>>,
    session_driver: &mut HashMap<String, ClientId>,
) {
    let parent_driver = session_driver.get(parent).copied();
    let mut stack: Vec<&str> = vec![parent];
    let mut visited: HashSet<&str> = HashSet::new();
    while let Some(sid) = stack.pop() {
        let Some(children) = child_sessions.get(sid) else {
            continue;
        };
        for child in children {
            if !visited.insert(child.as_str()) {
                continue;
            }
            session_subscribers
                .entry(child.clone())
                .or_default()
                .insert(client);
            if let Some(driver) = parent_driver {
                session_driver.entry(child.clone()).or_insert(driver);
            }
            stack.push(child);
        }
    }
}
/// Inject the requesting client's context into a `session/new`, `session/load`,
/// or `session/resume` request, **in place**. The agent's own state names
/// whichever client initialized last, which in leader mode is the wrong client.
///
/// For a session/new request:
/// - If the client has yolo_mode enabled, injects `yoloMode: true` into the request's `_meta` object.
/// - If the client has default_model set and the request doesn't already have a modelId,
///   injects `modelId` into the request's `_meta` object.
/// - Injects `clientIdentifier` so the agent can track which client owns each session
///   (used for scoping `yolo_mode_changed` broadcasts in leader mode).
///
/// Returns `true` when `json` was mutated.
fn inject_session_request_context(
    json: &mut serde_json::Value,
    capabilities: &ClientCapabilities,
    client_type: &str,
    client_id: ClientId,
) -> bool {
    let has_model = capabilities
        .default_model
        .as_ref()
        .is_some_and(|m| !m.is_empty());
    if !capabilities.yolo_mode
        && !capabilities.auto_mode
        && !has_model
        && client_type.is_empty()
        && !capabilities.code_nav_enabled
        && !capabilities.terminal
        && !capabilities.fs_read
        && !capabilities.fs_write
        && !capabilities.status_line
    {
        return false;
    }
    let method = json.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let is_session_new = method == AGENT_METHOD_NAMES.session_new;
    let is_session_load = method == AGENT_METHOD_NAMES.session_load;
    let is_session_resume = method == AGENT_METHOD_NAMES.session_resume;
    if !is_session_new && !is_session_load && !is_session_resume {
        return false;
    }
    let mut mutated = false;
    if let Some(params) = json.get_mut("params").and_then(|p| p.as_object_mut()) {
        let meta = params
            .entry("_meta")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(meta_obj) = meta.as_object_mut() {
            mutated = true;
            if is_session_new && capabilities.yolo_mode && !meta_obj.contains_key("yoloMode") {
                meta_obj.insert("yoloMode".to_string(), serde_json::json!(true));
                debug!("Injected yoloMode=true into session/new request");
            }
            if capabilities.auto_mode
                && !capabilities.yolo_mode
                && !meta_obj.contains_key("autoMode")
            {
                meta_obj.insert("autoMode".to_string(), serde_json::json!(true));
                debug!("Injected autoMode=true into session request");
            }
            if is_session_new
                && let Some(ref model_id) = capabilities.default_model
                && !model_id.is_empty()
                && !meta_obj.contains_key("modelId")
            {
                meta_obj.insert("modelId".to_string(), serde_json::json!(model_id));
                debug!(model_id, "Injected modelId into session/new request");
            }
            if !client_type.is_empty() && !meta_obj.contains_key("clientIdentifier") {
                meta_obj.insert(
                    "clientIdentifier".to_string(),
                    serde_json::json!(client_type),
                );
            }
            if !meta_obj.contains_key("echo.agent/leaderClientId") {
                meta_obj.insert(
                    "echo.agent/leaderClientId".to_string(),
                    serde_json::json!(client_id.0),
                );
            }
            meta_obj.insert(
                "codeNavEnabled".to_string(),
                serde_json::json!(capabilities.code_nav_enabled),
            );
            debug!(
                code_nav_enabled = capabilities.code_nav_enabled,
                "Injected codeNavEnabled into session request"
            );
            meta_obj.insert(
                "clientTerminal".to_string(),
                serde_json::json!(capabilities.terminal),
            );
            meta_obj.insert(
                "clientFsRead".to_string(),
                serde_json::json!(capabilities.fs_read),
            );
            meta_obj.insert(
                "clientFsWrite".to_string(),
                serde_json::json!(capabilities.fs_write),
            );
            meta_obj.insert(
                echo_agent_status_line::CLIENT_STATUS_LINE_META.to_string(),
                serde_json::json!(capabilities.status_line),
            );
        }
    }
    mutated
}
/// Inject client identity into an `initialize` request.
///
/// In leader mode, multiple clients (TUI, IDE extension, web) share one agent process.
/// The agent's `client_type` is set during `initialize` from `_meta.clientIdentifier`,
/// so the leader injects the IPC registration `client_type` to ensure the agent knows
/// the real client identity.
///
/// Only injects if `clientIdentifier` is not already present in `_meta` — respects
/// explicit client-provided values.
///
/// Mutates `json` in place. Returns `(mutated, was_initialize)`. The second
/// boolean is `true` only when the message was an `initialize` request,
/// allowing the caller to record that `initialize` has been seen.
fn inject_client_identity_into_initialize(
    json: &mut serde_json::Value,
    client_type: &str,
) -> (bool, bool) {
    if client_type.is_empty() {
        return (false, false);
    }
    let is_initialize = json
        .get("method")
        .and_then(|m| m.as_str())
        .is_some_and(|m| m == AGENT_METHOD_NAMES.initialize);
    if !is_initialize {
        return (false, false);
    }
    let mut mutated = false;
    if let Some(params) = json.get_mut("params").and_then(|p| p.as_object_mut()) {
        let meta = params
            .entry("_meta")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(meta_obj) = meta.as_object_mut()
            && !meta_obj.contains_key("clientIdentifier")
        {
            meta_obj.insert(
                "clientIdentifier".to_string(),
                serde_json::json!(client_type),
            );
            mutated = true;
            debug!(
                client_type,
                "Injected clientIdentifier into initialize request"
            );
        }
    }
    (mutated, true)
}
/// Extract yolo_mode change from echo.agent/yolo_mode_changed notification.
///
/// Returns Some(yolo_mode) if this is a yolo mode change notification.
fn extract_yolo_mode_change(json: &serde_json::Value) -> Option<bool> {
    let method = json.get("method")?.as_str()?;
    if method != "echo.agent/yolo_mode_changed" {
        return None;
    }
    let params = json.get("params")?;
    params.get("yolo_mode").and_then(|v| v.as_bool())
}
/// Extract the auto-mode intent from an `echo.agent/yolo_mode_changed` notification, so
/// the leader can keep `ClientCapabilities.auto_mode` fresh the same way it tracks
/// `yolo_mode`. Without this, a stale connect-time `auto_mode` capability would be
/// injected into later `session/new` requests, re-enabling Auto after the user opted
/// out. Returns `None` when the notification doesn't change auto state.
fn extract_auto_mode_change(json: &serde_json::Value) -> Option<bool> {
    let method = json.get("method")?.as_str()?;
    if method != "echo.agent/yolo_mode_changed" {
        return None;
    }
    let params = json.get("params")?;
    if let Some(b) = params.get("auto_mode").and_then(|v| v.as_bool()) {
        return Some(b);
    }
    match params.get("permission_mode").and_then(|v| v.as_str()) {
        Some("auto") => Some(true),
        Some("always-approve" | "ask" | "default") => Some(false),
        _ => None,
    }
}
/// Inject `clientIdentifier` into a `yolo_mode_changed` notification's params.
///
/// In leader mode, multiple clients share one agent. Without this injection, the agent
/// can't tell which client sent the yolo toggle and updates ALL sessions. With the
/// `clientIdentifier` in params, the agent scopes the update to only sessions owned
/// by the sending client.
///
/// Mutates `json` in place; returns `true` when mutated.
fn inject_client_identity_into_yolo_notification(
    json: &mut serde_json::Value,
    client_type: &str,
) -> bool {
    if client_type.is_empty() {
        return false;
    }
    let is_yolo = json
        .get("method")
        .and_then(|m| m.as_str())
        .is_some_and(|m| m == "echo.agent/yolo_mode_changed");
    if !is_yolo {
        return false;
    }
    let mut mutated = false;
    if let Some(params) = json.get_mut("params").and_then(|p| p.as_object_mut()) {
        params.insert(
            "clientIdentifier".to_string(),
            serde_json::json!(client_type),
        );
        mutated = true;
        debug!(
            client_type,
            "Injected clientIdentifier into yolo_mode_changed notification"
        );
    }
    mutated
}
/// Build a JSON-RPC error response for requests that arrive before the leader is ready.
///
/// Returns `Some(payload)` when the message has an `id` field (i.e. is a request),
/// so the client gets a structured response it can act on instead of hanging.
/// Returns `None` for notifications (no `id`) — those are silently dropped.
fn make_leader_starting_error(json: &serde_json::Value) -> Option<String> {
    let id = json.get("id").filter(|v| !v.is_null()).cloned()?;
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32002,
            "message": "leader_starting",
            "data": "Leader is still initializing (auth in progress). Retry shortly."
        }
    });
    Some(response.to_string())
}
/// Choose the bytes forwarded to the agent: the re-serialized `json` when an
/// injection/rewrite mutated it, the original `payload` verbatim otherwise
/// (including non-JSON payloads, which are never parsed or re-serialized).
fn select_outbound_payload(
    json: Option<&serde_json::Value>,
    payload_mutated: bool,
    payload: String,
) -> String {
    match json {
        Some(j) if payload_mutated => j.to_string(),
        _ => payload,
    }
}
/// Patch the `initialize` response so `meta.modelState.currentModelId` reflects the
/// client's `default_model` instead of the agent's global `current_model_id`.
///
/// Without this the TUI briefly shows the agent's startup default then jumps to the
/// client's preferred model once the first `session/new` response arrives.
///
/// Mutates `json` in place; returns `true` when patched.
fn patch_initialize_response_model(
    json: &mut serde_json::Value,
    default_model: &Option<String>,
) -> bool {
    let Some(model) = default_model.as_ref().filter(|m| !m.is_empty()) else {
        return false;
    };
    let needs_patch = json
        .pointer("/result/meta/modelState/currentModelId")
        .and_then(|v| v.as_str())
        .is_some_and(|current| current != model.as_str());
    if needs_patch {
        json["result"]["meta"]["modelState"]["currentModelId"] =
            serde_json::Value::String(model.clone());
        debug!(patched_model = %model, "Patched initialize response currentModelId");
        return true;
    }
    false
}
/// Extract model ID from a `session/setModel` request (for keeping `default_model` in sync).
fn extract_model_id_from_set_model(json: &serde_json::Value) -> Option<String> {
    let method = json.get("method")?.as_str()?;
    if method != AGENT_METHOD_NAMES.session_set_model {
        return None;
    }
    let params = json.get("params")?;
    params
        .get("modelId")
        .or_else(|| params.get("model_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}
fn cpu_profile_status_payload(status: CpuProfileStatus) -> ControlPayload {
    match status {
        CpuProfileStatus::Inactive => ControlPayload::CpuProfileStatus {
            active: false,
            stopping: false,
            started_at: None,
            svg_path: None,
            frequency_hz: None,
        },
        CpuProfileStatus::Active {
            started_at,
            svg_path,
            frequency_hz,
        } => ControlPayload::CpuProfileStatus {
            active: true,
            stopping: false,
            started_at: Some(started_at),
            svg_path: Some(svg_path),
            frequency_hz: Some(frequency_hz),
        },
        CpuProfileStatus::Stopping {
            started_at,
            svg_path,
            frequency_hz,
        } => ControlPayload::CpuProfileStatus {
            active: false,
            stopping: true,
            started_at: Some(started_at),
            svg_path: Some(svg_path),
            frequency_hz: Some(frequency_hz),
        },
    }
}
fn leader_info_payload(control_state: &LeaderServerControlState) -> ControlPayload {
    let manager = control_state.cpu_profile.lock();
    let status = manager.status();
    let (cpu_profile_active, cpu_profile_stopping, profile_started_at) = match status {
        CpuProfileStatus::Inactive => (false, false, None),
        CpuProfileStatus::Active { started_at, .. } => (true, false, Some(started_at)),
        CpuProfileStatus::Stopping { started_at, .. } => (false, true, Some(started_at)),
    };
    ControlPayload::LeaderInfo {
        pid: control_state.metadata.pid,
        socket_path: control_state.metadata.socket_path.clone(),
        lock_path: control_state.metadata.lock_path.clone(),
        ws_url_suffix: control_state.metadata.ws_url_suffix.clone(),
        leader_protocol_version: LEADER_PROTOCOL_VERSION,
        leader_binary_version: control_state.metadata.leader_binary_version.clone(),
        profiling_supported: manager.runtime_cpu_profile(),
        profiling_compiled_in: manager.profiling_compiled_in(),
        cpu_profile_active,
        cpu_profile_stopping,
        profile_started_at,
        profile_formats: manager.profile_formats().to_vec(),
    }
}
use crate::env::PROD_COMPUTER_HUB_WS_URL as PROD_COMPUTER_HUB_URL;
const WORKSPACE_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
fn workspace_err(message: impl Into<String>) -> ControlError {
    ControlError {
        code: ControlErrorCode::InternalError,
        message: message.into(),
        details: None,
    }
}
/// Resolve the hub credential, waiting if the leader is still wiring auth
/// (the IPC socket comes up first). Resolves the instant auth is wired or the
/// leader cancels — event-driven, no timeout.
async fn wait_for_leader_auth(
    ws: &WorkspaceControl,
    cancel: &CancellationToken,
) -> Result<Arc<dyn AuthProvider>, ControlError> {
    let mut rx = ws.auth.subscribe();
    let result = tokio::select! {
        result = rx.wait_for(|v| v.is_some()) => result,
        _ = cancel.cancelled() => {
            return Err(workspace_err(
                "leader is shutting down; cannot expose workspace to the hub",
            ));
        }
    };
    match result {
        Ok(guard) => Ok(guard.clone().expect("waited for Some")),
        Err(_) => Err(workspace_err(
            "leader is shutting down; cannot expose workspace to the hub",
        )),
    }
}
fn workspace_server_id() -> String {
    let raw = gethostname::gethostname()
        .to_string_lossy()
        .to_ascii_lowercase();
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = sanitized.trim_matches('-');
    if name.is_empty() {
        "echo-agent-workspace".to_string()
    } else {
        name.to_string()
    }
}
async fn drain_and_disconnect(handle: &WorkspaceHandle) {
    let tracker = handle.activity_tracker().clone();
    tracker.set_draining();
    if tokio::time::timeout(WORKSPACE_DRAIN_TIMEOUT, tracker.wait_until_drained())
        .await
        .is_err()
    {
        warn!(
            active = tracker.total_active(),
            "workspace drain timed out; disconnecting hub anyway"
        );
    }
    handle.shutdown_hub().await;
}
fn build_workspace_status(
    metadata: &LeaderServerMetadata,
    exposure: Option<&WorkspaceExposure>,
) -> ControlPayload {
    match exposure {
        None => ControlPayload::WorkspaceStatus {
            state: "none".to_string(),
            hub_url: None,
            cwd: None,
            uptime_ms: 0,
            active_tool_calls: 0,
            sessions: Vec::new(),
            pid: metadata.pid,
        },
        Some(exp) => {
            let snapshot = exp.handle.activity_tracker().snapshot();
            let mut sessions = exp.handle.session_ids();
            sessions.sort();
            ControlPayload::WorkspaceStatus {
                state: if exp.paused.load(std::sync::atomic::Ordering::Relaxed) {
                    "paused"
                } else {
                    "running"
                }
                .to_string(),
                hub_url: Some(exp.hub_url.clone()),
                cwd: Some(exp.cwd.display().to_string()),
                uptime_ms: exp.started_at.elapsed().as_millis() as u64,
                active_tool_calls: snapshot.active_tool_calls,
                sessions,
                pid: metadata.pid,
            }
        }
    }
}
async fn handle_workspace_start(
    control_state: LeaderServerControlState,
    hub_url: Option<String>,
    cwd: String,
    cancel: CancellationToken,
) -> Result<ControlPayload, ControlError> {
    let ws = &control_state.workspace;
    let url_str = hub_url
        .filter(|u| !u.trim().is_empty())
        .or_else(|| ws.default_hub_url.clone())
        .unwrap_or_else(|| PROD_COMPUTER_HUB_URL.to_string());
    let url = url::Url::parse(&url_str)
        .map_err(|e| workspace_err(format!("invalid hub url {url_str}: {e}")))?;
    let cwd_path = PathBuf::from(&cwd);
    let _serialize = ws.lock.lock().await;
    if let Some(existing) = ws.exposure.load_full()
        && !existing.paused.load(Ordering::Relaxed)
        && existing.cwd == cwd_path
        && existing.hub_url == url_str
    {
        return Ok(build_workspace_status(
            &control_state.metadata,
            Some(existing.as_ref()),
        ));
    }
    let allow_insecure_ws =
        url.scheme() == "ws" && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    let status_config = echo_agent_workspace::StatusConfig::from_env();
    let alpha_test_key = None;
    let auth = wait_for_leader_auth(ws, &cancel).await?;
    let server_id = workspace_server_id();
    let metadata = serde_json::json!({
        "source": "echo-agent-workspace",
        "hostname": gethostname::gethostname().to_string_lossy(),
        "cwd": cwd_path.display().to_string(),
    });
    let upload_queue_enabled =
        std::env::var("ECHO_AGENT_WORKSPACE_UPLOAD_QUEUE_ENABLED").as_deref() != Ok("false");
    crate::agent::folder_trust::resolve_and_record(&cwd_path, None, false);
    let project_lsp_trusted = crate::agent::folder_trust::project_scope_allowed(&cwd_path);
    let handle = echo_agent_workspace::connect_local_workspace(
        cwd_path.clone(),
        url,
        auth,
        Some(metadata),
        Some(server_id),
        alpha_test_key,
        allow_insecure_ws,
        status_config,
        upload_queue_enabled,
        project_lsp_trusted,
        None,
        false,
        false,
    )
    .await
    .map_err(|e| workspace_err(format!("failed to connect workspace to hub: {e}")))?;
    let exposure = Arc::new(WorkspaceExposure {
        handle,
        hub_url: url_str,
        cwd: cwd_path,
        started_at: Instant::now(),
        paused: AtomicBool::new(false),
    });
    let payload = build_workspace_status(&control_state.metadata, Some(exposure.as_ref()));
    if let Some(old) = ws.exposure.swap(Some(exposure)) {
        drain_and_disconnect(&old.handle).await;
    }
    Ok(payload)
}
async fn handle_workspace_pause(
    control_state: LeaderServerControlState,
) -> Result<ControlPayload, ControlError> {
    let ws = &control_state.workspace;
    let _serialize = ws.lock.lock().await;
    let Some(exp) = ws.exposure.load_full() else {
        return Err(workspace_err("no workspace exposure is running"));
    };
    if !exp.paused.load(Ordering::Relaxed) {
        drain_and_disconnect(&exp.handle).await;
        exp.paused.store(true, Ordering::Relaxed);
    }
    Ok(build_workspace_status(
        &control_state.metadata,
        Some(exp.as_ref()),
    ))
}
async fn handle_workspace_resume(
    control_state: LeaderServerControlState,
) -> Result<ControlPayload, ControlError> {
    let ws = &control_state.workspace;
    let _serialize = ws.lock.lock().await;
    let Some(exp) = ws.exposure.load_full() else {
        return Err(workspace_err("no workspace exposure is running"));
    };
    if exp.paused.load(Ordering::Relaxed) {
        exp.handle.activity_tracker().set_active();
        if let Err(e) = exp.handle.connect_hub().await {
            exp.handle.activity_tracker().set_draining();
            return Err(workspace_err(format!("failed to reconnect to hub: {e}")));
        }
        exp.paused.store(false, Ordering::Relaxed);
    }
    Ok(build_workspace_status(
        &control_state.metadata,
        Some(exp.as_ref()),
    ))
}
async fn handle_workspace_stop(
    control_state: LeaderServerControlState,
) -> Result<ControlPayload, ControlError> {
    let ws = &control_state.workspace;
    let _serialize = ws.lock.lock().await;
    if let Some(exp) = ws.exposure.swap(None) {
        drain_and_disconnect(&exp.handle).await;
    }
    Ok(build_workspace_status(&control_state.metadata, None))
}
async fn handle_workspace_status(
    control_state: LeaderServerControlState,
) -> Result<ControlPayload, ControlError> {
    let exposure = control_state.workspace.exposure.load_full();
    Ok(build_workspace_status(
        &control_state.metadata,
        exposure.as_deref(),
    ))
}
async fn finalize_workspace_on_shutdown(control_state: LeaderServerControlState) {
    let ws = &control_state.workspace;
    let _serialize = ws.lock.lock().await;
    if let Some(exp) = ws.exposure.swap(None) {
        info!("Draining workspace exposure on leader shutdown");
        drain_and_disconnect(&exp.handle).await;
    }
}
fn handle_control_command(
    control_state: &LeaderServerControlState,
    command: ControlCommand,
) -> Result<ControlPayload, ControlError> {
    match command {
        ControlCommand::GetLeaderInfo => Ok(leader_info_payload(control_state)),
        ControlCommand::CpuProfileStatus => {
            let manager = control_state.cpu_profile.lock();
            Ok(cpu_profile_status_payload(manager.status()))
        }
        ControlCommand::StartCpuProfile {
            output,
            frequency_hz,
        } => {
            let mut manager = control_state.cpu_profile.lock();
            let status = manager.start(CpuProfileStartOptions {
                output: output.map(PathBuf::from),
                frequency_hz,
            })?;
            match status {
                CpuProfileStatus::Active {
                    started_at,
                    svg_path,
                    frequency_hz,
                } => Ok(ControlPayload::CpuProfileStarted {
                    pid: control_state.metadata.pid,
                    svg_path,
                    frequency_hz,
                    started_at,
                }),
                CpuProfileStatus::Inactive => {
                    Ok(cpu_profile_status_payload(CpuProfileStatus::Inactive))
                }
                CpuProfileStatus::Stopping {
                    started_at,
                    svg_path,
                    frequency_hz,
                } => Ok(cpu_profile_status_payload(CpuProfileStatus::Stopping {
                    started_at,
                    svg_path,
                    frequency_hz,
                })),
            }
        }
        ControlCommand::StopCpuProfile => {
            unreachable!("StopCpuProfile must be handled asynchronously")
        }
        ControlCommand::WorkspaceStart { .. }
        | ControlCommand::WorkspacePause
        | ControlCommand::WorkspaceResume
        | ControlCommand::WorkspaceStop
        | ControlCommand::WorkspaceStatus => {
            unreachable!("workspace control commands are handled asynchronously")
        }
        ControlCommand::RelaunchForUpdate { .. } => {
            unreachable!("RelaunchForUpdate must be handled asynchronously")
        }
    }
}
async fn handle_stop_cpu_profile(
    control_state: LeaderServerControlState,
) -> Result<ControlPayload, ControlError> {
    let stop_handle = {
        let mut manager = control_state.cpu_profile.lock();
        manager.take_stop_handle()?
    };
    let pid = control_state.metadata.pid;
    let result = tokio::task::spawn_blocking(move || stop_handle.finish()).await;
    control_state.cpu_profile.lock().complete_stop();
    let result = result.map_err(|join_error| ControlError {
        code: ControlErrorCode::InternalError,
        message: "CPU profile stop task failed".to_string(),
        details: Some(serde_json::json!({ "error": join_error.to_string() })),
    })??;
    Ok(ControlPayload::CpuProfileStopped {
        pid,
        svg_path: result.svg_path,
        started_at: result.started_at,
        stopped_at: result.stopped_at,
    })
}
async fn finalize_cpu_profile_on_shutdown(control_state: LeaderServerControlState) {
    let (disposition, stop_handle, mut stop_completion_rx) = {
        let mut manager = control_state.cpu_profile.lock();
        let disposition = manager.shutdown_stop_disposition();
        let stop_completion_rx = manager.subscribe_stop_completion();
        let stop_handle = match manager.take_shutdown_stop_handle() {
            Ok(stop_handle) => stop_handle,
            Err(error) => {
                warn!(error = %error, "Failed to prepare active CPU profile for leader shutdown");
                return;
            }
        };
        (disposition, stop_handle, stop_completion_rx)
    };
    let Some(disposition) = disposition else {
        return;
    };
    let Some(stop_handle) = stop_handle else {
        match disposition {
            ShutdownStopDisposition::AlreadyStopping => {
                debug!(
                    "CPU profile stop already in progress during leader shutdown; waiting for in-flight finalization task"
                );
                if !*stop_completion_rx.borrow() && stop_completion_rx.changed().await.is_err() {
                    warn!("CPU profile stop completion channel closed during leader shutdown wait");
                }
            }
            ShutdownStopDisposition::StartedShutdownStop => {
                warn!("Expected shutdown CPU profile stop handle but none was available");
            }
        }
        return;
    };
    let result = tokio::task::spawn_blocking(move || stop_handle.finish()).await;
    control_state.cpu_profile.lock().complete_stop();
    match result {
        Ok(Ok(result)) => {
            info!(
                path = %result.svg_path.display(),
                started_at = %result.started_at,
                stopped_at = %result.stopped_at,
                "Finalized active CPU profile during leader shutdown"
            );
        }
        Ok(Err(error)) => {
            warn!(error = %error, "Failed to finalize active CPU profile during leader shutdown");
        }
        Err(join_error) => {
            warn!(
                error = %join_error,
                "CPU profile shutdown finalization task failed"
            );
        }
    }
}
/// Bounded grace the leader waits for in-flight turns to finish before a
/// `RelaunchForUpdate` relaunch. If the agent is still busy when this elapses,
/// the leader exits anyway — the in-flight turn ends and the session reloads
/// cleanly (truncated at the last persisted boundary).
const RELAUNCH_GRACE: Duration = Duration::from_secs(5);
/// Bound on the post-drain session flush ([`AgentActivity::flush_all_sessions`]).
const RELAUNCH_FLUSH_GRACE: Duration = Duration::from_secs(5);
/// Total shutdown budget advertised to clients in the `Relaunching` ack:
/// idle-drain plus session flush.
const RELAUNCH_TOTAL_GRACE: Duration =
    Duration::from_millis((RELAUNCH_GRACE.as_millis() + RELAUNCH_FLUSH_GRACE.as_millis()) as u64);
/// Poll cadence while waiting for the agent to go idle during the grace period.
const RELAUNCH_GRACE_POLL: Duration = Duration::from_millis(100);
/// Decide whether a [`ControlCommand::RelaunchForUpdate`] is accepted (the
/// synchronous half — kept separate from arming the drain so the caller can send
/// the `Relaunching` ack BEFORE the leader begins shutting down; otherwise an
/// idle leader can race the ack and the client sees a dropped control response).
///
/// Declines unless the target is strictly newer (directional guard) and no
/// relaunch is already in progress (idempotent across multiple clients). On
/// accept it sets `relaunching` so duplicate requests are declined.
fn decide_relaunch_for_update(
    control_state: &LeaderServerControlState,
    to_version: String,
    relaunching: &AtomicBool,
) -> Result<ControlPayload, ControlError> {
    let leader_version = control_state.metadata.leader_binary_version.clone();
    if !super::leader_is_older_than(&leader_version, &to_version) {
        debug!(
            from_version = %leader_version,
            to_version = %to_version,
            "RelaunchForUpdate declined: target is not strictly newer (or unparseable)"
        );
        return Ok(ControlPayload::RelaunchDeclined {
            reason: format!("leader version {leader_version} is not older than {to_version}"),
        });
    }
    if relaunching.swap(true, Ordering::SeqCst) {
        return Ok(ControlPayload::RelaunchDeclined {
            reason: "a relaunch is already in progress".to_string(),
        });
    }
    info!(
        from_version = %leader_version,
        to_version = %to_version,
        grace_ms = RELAUNCH_TOTAL_GRACE.as_millis() as u64,
        "RelaunchForUpdate accepted; draining before relaunch onto new binary"
    );
    Ok(ControlPayload::Relaunching {
        from_version: leader_version,
        to_version,
        grace_ms: RELAUNCH_TOTAL_GRACE.as_millis() as u64,
    })
}
/// Arm the bounded-grace drain for an accepted relaunch: wait up to
/// [`RELAUNCH_GRACE`] for the agent to go idle (`agent_busy` for IPC traffic
/// AND [`AgentActivity::is_busy`] for relay-driven turns / subagents), flush
/// every session actor, then set [`ShutdownReason::AutoUpdate`] and cancel —
/// the same exit path the auto-update checker uses. Must be called *after*
/// the `Relaunching` ack has been sent so the ack is delivered before
/// `ShuttingDown`.
fn spawn_relaunch_drain(
    shutdown_tx: watch::Sender<super::protocol::ShutdownReason>,
    cancel: CancellationToken,
    agent_busy: Arc<AtomicBool>,
    agent_activity: AgentActivity,
) {
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + RELAUNCH_GRACE;
        while agent_busy.load(Ordering::Relaxed) || agent_activity.is_busy() {
            if tokio::time::Instant::now() >= deadline {
                warn!(
                    "RelaunchForUpdate grace elapsed while agent busy; relaunching anyway (in-flight turn ends)"
                );
                break;
            }
            tokio::select! {
                // Another path already triggered shutdown — let it own the exit.
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(RELAUNCH_GRACE_POLL) => {}
            }
        }
        agent_activity
            .flush_all_sessions(RELAUNCH_FLUSH_GRACE)
            .await;
        let _ = shutdown_tx.send(super::protocol::ShutdownReason::AutoUpdate);
        cancel.cancel();
    });
}
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Failed to acquire leader lock: {0}")]
    LockFailed(#[from] super::lock::LockError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
/// Build the ACP notification payload for a leader/client version mismatch, or
/// return `None` when versions match or detection is disabled.
///
/// Extracted as a standalone function so the notification shape can be unit-tested
/// without running a full server.
fn make_version_mismatch_notification(
    client_version: &str,
    leader_version: &str,
) -> Option<String> {
    if client_version == leader_version || leader_version == "unknown" {
        return None;
    }
    Some(
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "echo.agent/leader/version_mismatch",
            "params": {
                "clientVersion": client_version,
                "leaderVersion": leader_version,
                "message": format!(
                    "Client version {client_version} differs from leader version \
                     {leader_version}. Restart the echoagent binary to use the same version."
                )
            }
        })
        .to_string(),
    )
}
/// Run the leader IPC server.
///
/// The socket_path is where the Unix socket will be created.
/// Caller is responsible for:
/// 1. Cleaning up any stale socket file before calling this
/// 2. Acquiring the leader lock AFTER this function creates the socket
///
/// This ordering ensures that:
/// - Clients waiting for socket can connect as soon as we're ready
/// - The lock acquisition happens after we're actually listening
///
/// # Readiness gating
///
/// The `ready_rx` watch channel controls whether ACP messages are forwarded to the
/// agent. While `*ready_rx.borrow() == false` (leader still initializing):
/// - Client connections and IPC registrations are accepted normally.
/// - ACP requests (messages with an `id`) receive a structured `leader_starting`
///   JSON-RPC error so the client can retry rather than hang.
/// - ACP notifications (no `id`) are dropped with a trace log.
///
/// Once `ready_rx` is signaled `true` (socket bound + bounded auth complete; the
/// model catalog and remote settings stream in afterward), all subsequent
/// ACP traffic is forwarded to the agent as normal.
///
/// # Arguments
///
/// * `socket_path` - Path for the Unix domain socket
/// * `acp_tx` - Channel to send ACP messages from clients to the agent
/// * `response_rx` - Channel to receive responses from the agent to route to clients
/// * `cancel` - Cancellation token for graceful shutdown
/// * `no_exit_on_disconnect` - If true, don't exit when all clients disconnect
/// * `client_count` - Atomic counter tracking the number of connected clients
/// * `agent_busy` - Atomic flag set while the agent has in-flight **IPC**
///   requests; relay-driven traffic never sets it
/// * `agent_activity` - Agent-derived activity view (running turns, parked
///   interactions, live subagents) consulted by the `RelaunchForUpdate` drain
///   alongside `agent_busy`, plus the pre-shutdown session flush
/// * `ready_rx` - Watch receiver; ACP forwarding is gated until this is `true`
/// * `relay_demand_tx` - Watch sender flipped to `true` when the first
///   [`ClientMode::Headless`] client registers. `run_leader` defers starting the
///   cloud.echo-agent.invalid WebSocket relay until this fires, so a leader serving only
///   interactive clients (TUI dashboard, IDE) never duplicates its ACP stream
///   onto the relay. Headless registration is the devbox-flow marker: those
///   clients are driven remotely *through* the relay.
/// * `shutdown_tx` - Watch sender for the shutdown reason. The server subscribes
///   its own receiver and reads it once when `cancel` fires (defaults to
///   [`ShutdownReason::Manual`]). The auto-update checker and the
///   [`ControlCommand::RelaunchForUpdate`] handler send [`ShutdownReason::AutoUpdate`]
///   before cancelling so clients see the real reason; senders must write before
///   cancelling.
/// * `leader_version_override` - If `Some`, overrides [`LEADER_VERSION`] for version
///   mismatch detection. Pass `None` in production; pass a test version string in
///   integration tests to bypass the `"unknown"` constant that appears in dev builds
///   where `VERSION_WITH_COMMIT` is not set.
/// * `control_state` - Leader-local control metadata and CPU profiling state
pub async fn run_leader_server(
    socket_path: std::path::PathBuf,
    acp_tx: mpsc::UnboundedSender<String>,
    mut response_rx: mpsc::UnboundedReceiver<String>,
    cancel: CancellationToken,
    no_exit_on_disconnect: bool,
    client_count: Arc<AtomicUsize>,
    agent_busy: Arc<AtomicBool>,
    agent_activity: AgentActivity,
    ready_rx: watch::Receiver<bool>,
    relay_demand_tx: watch::Sender<bool>,
    shutdown_tx: watch::Sender<super::protocol::ShutdownReason>,
    leader_version_override: Option<&'static str>,
    control_state: LeaderServerControlState,
) -> Result<(), ServerError> {
    let _ = std::fs::remove_file(&socket_path);
    let shutdown_reason_rx = shutdown_tx.subscribe();
    let listener = LeaderListener::bind(&socket_path)?;
    info!("Leader server listening");
    let (event_tx, event_rx) = kanal::unbounded_async::<ServerEvent>();
    let mut clients: HashMap<ClientId, ClientState> = HashMap::new();
    let mut session_driver: HashMap<String, ClientId> = HashMap::new();
    let mut session_subscribers: HashMap<String, std::collections::HashSet<ClientId>> =
        HashMap::new();
    let mut child_sessions: HashMap<String, HashSet<String>> = HashMap::new();
    let mut pending_load_by_req: HashMap<String, (ClientId, String)> = HashMap::new();
    let mut load_live_buffer: HashMap<(ClientId, String), Vec<BufferedLive>> = HashMap::new();
    let mut orphan_replay_warned: HashSet<ClientId> = HashSet::new();
    let mut load_replay_max_seq: HashMap<(ClientId, String), u64> = HashMap::new();
    let mut interaction_requests: HashMap<String, HashMap<String, Arc<str>>> = HashMap::new();
    let mut last_active_client: Option<ClientId> = None;
    let mut had_clients = false;
    let mut pending_requests: usize = 0;
    let relaunching = Arc::new(AtomicBool::new(false));
    loop {
        let poll = tokio::select! {
            biased;
            _ = cancel.cancelled() => LeaderServerPoll::Cancelled,
            accept_result = listener.accept() => {
                LeaderServerPoll::Accept(accept_result.map(|(stream, _)| stream))
            }
            Ok(event) = event_rx.recv() => LeaderServerPoll::Event(event),
            Some(payload) = response_rx.recv() => LeaderServerPoll::Response(payload),
        };
        match poll {
            LeaderServerPoll::Cancelled => {
                let reason = shutdown_reason_rx.borrow().clone();
                info!(?reason, "Leader server shutting down (cancelled)");
                if pending_requests > 0 {
                    debug!(pending_requests, "Resetting agent_busy on shutdown");
                    agent_busy.store(false, Ordering::Relaxed);
                }
                broadcast_shutdown(&clients, reason).await;
                break;
            }
            LeaderServerPoll::Accept(accept_result) => match accept_result {
                Ok(stream) => {
                    had_clients = true;
                    let client_id = ClientId::new();
                    let (tx, rx) = kanal::unbounded_async();
                    clients.insert(
                        client_id,
                        ClientState {
                            tx,
                            mode: ClientMode::Stdio,
                            capabilities: ClientCapabilities::default(),
                            client_type: String::new(),
                            initialize_seen: false,
                            patch_initialize_model: false,
                            registered: false,
                        },
                    );
                    spawn_client_handler(
                        client_id,
                        stream,
                        rx,
                        event_tx.clone(),
                        cancel.child_token(),
                        ready_rx.clone(),
                        control_state.clone(),
                    );
                }
                Err(e) => error!(error = %e, "Accept failed"),
            },
            LeaderServerPoll::Event(event) => match event {
                ServerEvent::Registered(id, mode, capabilities, client_type) => {
                    if let Some(client) = clients.get_mut(&id) {
                        client.mode = mode;
                        client.capabilities = capabilities;
                        client.client_type = client_type;
                        client.registered = true;
                        client_count.fetch_add(1, Ordering::Relaxed);
                        debug!(client_id = id.0, ?mode, yolo_mode = client.capabilities.yolo_mode, client_type = %client.client_type, "Client registered");
                        echo_agent_telemetry::unified_log::info(
                            "leader.client.registered",
                            None,
                            Some(serde_json::json!({
                                "client_id": id.0,
                                "client_type": client.client_type,
                            })),
                        );
                        if mode == ClientMode::Headless {
                            let newly_demanded = relay_demand_tx.send_if_modified(|demanded| {
                                let changed = !*demanded;
                                *demanded = true;
                                changed
                            });
                            if newly_demanded {
                                info!(
                                    client_id = id.0,
                                    "First headless client registered; signalling relay demand"
                                );
                            }
                        }
                        let effective_leader_version =
                            leader_version_override.unwrap_or(LEADER_VERSION);
                        if let Some(ref cv) = client.capabilities.client_version
                            && let Some(payload) = make_version_mismatch_notification(
                                cv.as_str(),
                                effective_leader_version,
                            )
                        {
                            warn!(
                                client_id = id.0,
                                client_version = cv.as_str(),
                                leader_version = effective_leader_version,
                                "Version mismatch: client binary differs from leader binary"
                            );
                            let _ = client.tx.try_send(ClientOutbound::Acp(payload.into()));
                        }
                    }
                }
                ServerEvent::Disconnected(id) => {
                    let was_registered = clients.get(&id).is_some_and(|c| c.registered);
                    clients.remove(&id);
                    if was_registered {
                        client_count.fetch_sub(1, Ordering::Relaxed);
                        echo_agent_telemetry::unified_log::info(
                            "leader.client.disconnected",
                            None,
                            Some(serde_json::json!({ "client_id": id.0 })),
                        );
                    }
                    pending_load_by_req.retain(|_, (c, _)| *c != id);
                    load_live_buffer.retain(|(c, _), _| *c != id);
                    load_replay_max_seq.retain(|(c, _), _| *c != id);
                    let mut detached_sessions: Vec<String> = Vec::new();
                    let viewed: Vec<String> = session_subscribers
                        .iter()
                        .filter(|(_, subs)| subs.contains(&id))
                        .map(|(sid, _)| sid.clone())
                        .collect();
                    for sid in viewed {
                        let now_empty = if let Some(subs) = session_subscribers.get_mut(&sid) {
                            subs.remove(&id);
                            subs.is_empty()
                        } else {
                            true
                        };
                        if now_empty {
                            session_subscribers.remove(&sid);
                            session_driver.remove(&sid);
                            detached_sessions.push(sid);
                        } else if session_driver.get(&sid) == Some(&id) {
                            if let Some(&next) =
                                session_subscribers.get(&sid).and_then(|s| s.iter().next())
                            {
                                session_driver.insert(sid.clone(), next);
                                debug!(
                                    session_id = %sid,
                                    old_driver = id.0,
                                    new_driver = next.0,
                                    "Transferred session driver after disconnect"
                                );
                            } else {
                                session_driver.remove(&sid);
                            }
                        }
                    }
                    if last_active_client == Some(id) {
                        last_active_client = None;
                    }
                    if !detached_sessions.is_empty() {
                        let _ = acp_tx.send(internal_notification(
                            InternalMethod::EvictSessions,
                            serde_json::json!({ "sessionIds": detached_sessions }),
                        ));
                        info!(
                            client_id = id.0,
                            session_count = detached_sessions.len(),
                            "Sent client-disconnect detach notification for disconnected client"
                        );
                    }
                    debug!(client_id = id.0, "Client removed");
                    if clients.is_empty() && had_clients && !no_exit_on_disconnect {
                        info!("Leader server shutting down (all clients disconnected)");
                        break;
                    }
                }
                ServerEvent::Message(
                    id,
                    ClientMessage::Control {
                        request_id,
                        command,
                    },
                ) => {
                    if let Some(client) = clients.get(&id) {
                        let client_tx = client.tx.clone();
                        let control_state = control_state.clone();
                        let cancel = cancel.clone();
                        let shutdown_tx = shutdown_tx.clone();
                        let agent_busy = agent_busy.clone();
                        let agent_activity = agent_activity.clone();
                        let relaunching = relaunching.clone();
                        tokio::spawn(async move {
                            let result = match command {
                                ControlCommand::StopCpuProfile => {
                                    handle_stop_cpu_profile(control_state).await
                                }
                                ControlCommand::WorkspaceStart { hub_url, cwd } => {
                                    handle_workspace_start(
                                        control_state,
                                        hub_url,
                                        cwd,
                                        cancel.clone(),
                                    )
                                    .await
                                }
                                ControlCommand::WorkspacePause => {
                                    handle_workspace_pause(control_state).await
                                }
                                ControlCommand::WorkspaceResume => {
                                    handle_workspace_resume(control_state).await
                                }
                                ControlCommand::WorkspaceStop => {
                                    handle_workspace_stop(control_state).await
                                }
                                ControlCommand::WorkspaceStatus => {
                                    handle_workspace_status(control_state).await
                                }
                                ControlCommand::RelaunchForUpdate { to_version } => {
                                    decide_relaunch_for_update(
                                        &control_state,
                                        to_version,
                                        &relaunching,
                                    )
                                }
                                other => handle_control_command(&control_state, other),
                            };
                            let arm_relaunch =
                                matches!(result, Ok(ControlPayload::Relaunching { .. }));
                            if let Err(e) = client_tx
                                .send(ServerMessage::ControlResult { request_id, result }.into())
                                .await
                            {
                                warn!(client_id = id.0, error = %e, "Failed to send control response to client");
                            }
                            if arm_relaunch {
                                spawn_relaunch_drain(
                                    shutdown_tx,
                                    cancel,
                                    agent_busy,
                                    agent_activity,
                                );
                            }
                        });
                    }
                }
                ServerEvent::Message(id, ClientMessage::Acp { payload }) => {
                    let mut json: Option<serde_json::Value> = serde_json::from_str(&payload).ok();
                    let mut payload_mutated = false;
                    if !*ready_rx.borrow() {
                        if let Some(error_payload) =
                            json.as_ref().and_then(make_leader_starting_error)
                        {
                            if let Some(client) = clients.get(&id) {
                                let _ = client
                                    .tx
                                    .try_send(ClientOutbound::Acp(error_payload.into()));
                            }
                            trace!(
                                client_id = id.0,
                                "Returned leader_starting error (not yet ready)"
                            );
                        } else {
                            trace!(
                                client_id = id.0,
                                "Dropped pre-ready notification (leader not yet ready)"
                            );
                        }
                        continue;
                    }
                    if let Some(client) = clients.get(&id)
                        && client.mode == ClientMode::Stdio
                    {
                        last_active_client = Some(id);
                    }
                    if let Some(session_id) = json.as_ref().and_then(extract_session_id) {
                        session_subscribers
                            .entry(session_id.clone())
                            .or_default()
                            .insert(id);
                        session_driver.entry(session_id.clone()).or_insert(id);
                        backfill_child_routes(
                            &session_id,
                            id,
                            &child_sessions,
                            &mut session_subscribers,
                            &mut session_driver,
                        );
                    }
                    if let (Some(json), Some(client)) = (json.as_ref(), clients.get_mut(&id)) {
                        if let Some(yolo_mode) = extract_yolo_mode_change(json) {
                            client.capabilities.yolo_mode = yolo_mode;
                            debug!(
                                client_id = id.0,
                                yolo_mode, "Updated client yolo_mode from notification"
                            );
                        }
                        if let Some(auto_mode) = extract_auto_mode_change(json) {
                            client.capabilities.auto_mode = auto_mode;
                            debug!(
                                client_id = id.0,
                                auto_mode, "Updated client auto_mode from notification"
                            );
                        }
                        if let Some(new_model) = extract_model_id_from_set_model(json) {
                            debug!(client_id = id.0, model = %new_model, "Updated client default_model from session/setModel");
                            client.capabilities.default_model = Some(new_model);
                        }
                    }
                    if let (Some(json), Some(client)) = (json.as_mut(), clients.get_mut(&id)) {
                        if !client.initialize_seen {
                            let (injected, was_initialize) =
                                inject_client_identity_into_initialize(json, &client.client_type);
                            payload_mutated |= injected;
                            if was_initialize {
                                client.initialize_seen = true;
                                if client
                                    .capabilities
                                    .default_model
                                    .as_ref()
                                    .is_some_and(|m| !m.is_empty())
                                {
                                    client.patch_initialize_model = true;
                                }
                            }
                        }
                        payload_mutated |= inject_session_request_context(
                            json,
                            &client.capabilities,
                            &client.client_type,
                            id,
                        );
                        payload_mutated |= inject_client_identity_into_yolo_notification(
                            json,
                            &client.client_type,
                        );
                    }
                    let rewritten = json.as_mut().and_then(|j| rewrite_request_id(j, id));
                    payload_mutated |= rewritten.is_some();
                    if let Some(json) = json.as_ref()
                        && is_session_attach_request(json)
                        && let Some(load_sid) = extract_session_id(json)
                        && let Some((ns_id, _)) = rewritten.as_ref()
                    {
                        pending_load_by_req.insert(ns_id.clone(), (id, load_sid.clone()));
                        load_live_buffer.entry((id, load_sid)).or_default();
                    }
                    if rewritten.is_some() {
                        pending_requests += 1;
                        agent_busy.store(true, Ordering::Relaxed);
                    }
                    let outbound = select_outbound_payload(json.as_ref(), payload_mutated, payload);
                    let _ = acp_tx.send(outbound);
                }
                ServerEvent::Message(_, _) => {}
            },
            LeaderServerPoll::Response(payload) => {
                let mut json: Option<serde_json::Value> = serde_json::from_str(&payload).ok();
                let parsed_response = json.as_mut().and_then(parse_response_id);
                if parsed_response.is_some() {
                    pending_requests = pending_requests.saturating_sub(1);
                    agent_busy.store(pending_requests > 0, Ordering::Relaxed);
                }
                if let Some((orphan_client, ref orphan_req_id)) = parsed_response
                    && !clients.contains_key(&orphan_client)
                {
                    warn!(
                        client_id = orphan_client.0,
                        request_id = orphan_req_id.as_str(),
                        "Dropping RPC response: requesting client disconnected (response orphaned)"
                    );
                    echo_agent_telemetry::unified_log::warn(
                        "leader.response.orphaned",
                        None,
                        Some(serde_json::json!({
                            "client_id": orphan_client.0,
                            "request_id": orphan_req_id,
                        })),
                    );
                }
                if let Some((client_id, ref raw_response_id)) = parsed_response
                    && let Some(client) = clients.get_mut(&client_id)
                    && let Some(json) = json.as_mut()
                {
                    if let Some(session_id) = extract_session_id_from_result(json) {
                        session_subscribers
                            .entry(session_id.clone())
                            .or_default()
                            .insert(client_id);
                        session_driver
                            .entry(session_id.clone())
                            .or_insert(client_id);
                        backfill_child_routes(
                            &session_id,
                            client_id,
                            &child_sessions,
                            &mut session_subscribers,
                            &mut session_driver,
                        );
                        trace!(
                            client_id = client_id.0,
                            session_id, "Subscribed client to session from response"
                        );
                    }
                    if client.patch_initialize_model {
                        client.patch_initialize_model = false;
                        patch_initialize_response_model(json, &client.capabilities.default_model);
                    }
                    let restored_payload: Arc<str> = json.to_string().into();
                    match client.tx.try_send(ClientOutbound::Acp(restored_payload)) {
                        Ok(true) => {
                            trace!(client_id = client_id.0, "Routed response via request ID");
                        }
                        Ok(false) => {
                            warn!(
                                client_id = client_id.0,
                                "Failed to send response to client (channel full)"
                            );
                            echo_agent_telemetry::unified_log::warn(
                                "leader.response.send_failed",
                                None,
                                Some(serde_json::json!({
                                    "client_id": client_id.0,
                                    "reason": "channel_full",
                                })),
                            );
                        }
                        Err(e) => {
                            warn!(client_id = client_id.0, error = %e, "Failed to send response to client (channel closed)");
                            echo_agent_telemetry::unified_log::warn(
                                "leader.response.send_failed",
                                None,
                                Some(serde_json::json!({
                                    "client_id": client_id.0,
                                    "reason": "channel_closed",
                                })),
                            );
                        }
                    }
                    if let Some((buf_client, buf_sid)) = pending_load_by_req.remove(raw_response_id)
                    {
                        let replay_cutoff: Option<u64> =
                            load_replay_max_seq.remove(&(buf_client, buf_sid.clone()));
                        if let Some(buffered) =
                            load_live_buffer.remove(&(buf_client, buf_sid.clone()))
                            && let Some(target) = clients.get(&buf_client)
                        {
                            let mut count = 0usize;
                            let mut deduped = 0usize;
                            for (buffered_payload, buffered_seq) in buffered {
                                if let Some(cutoff) = replay_cutoff
                                    && buffered_seq.is_some_and(|s| s <= cutoff)
                                {
                                    deduped += 1;
                                    continue;
                                }
                                if let Err(e) =
                                    target.tx.try_send(ClientOutbound::Acp(buffered_payload))
                                {
                                    warn!(client_id = buf_client.0, error = %e, "Failed to flush buffered live notification after load (channel closed)");
                                    break;
                                }
                                count += 1;
                            }
                            if count > 0 || deduped > 0 {
                                trace!(
                                    client_id = buf_client.0,
                                    count,
                                    deduped,
                                    "Flushed buffered live notifications after load (replay-overlap dropped)"
                                );
                            }
                        }
                        if let Some(cached) = interaction_requests.get(buf_sid.as_str())
                            && let Some(target) = clients.get(&buf_client)
                        {
                            let count = cached.len();
                            for req in cached.values() {
                                if let Err(e) = target.tx.try_send(ClientOutbound::Acp(req.clone()))
                                {
                                    warn!(client_id = buf_client.0, error = %e, "Failed to replay interaction request after load (channel closed)");
                                    break;
                                }
                            }
                            if count > 0 {
                                trace!(
                                    client_id = buf_client.0,
                                    count,
                                    session_id = buf_sid.as_str(),
                                    "Replayed pending interaction modals to newly-attached client"
                                );
                            }
                        }
                    }
                    continue;
                }
                let payload: Arc<str> = payload.into();
                let json = json;
                if json
                    .as_ref()
                    .is_some_and(is_machine_wide_broadcast_notification)
                {
                    for client in clients.values() {
                        let _ = client.tx.try_send(ClientOutbound::Acp(payload.clone()));
                    }
                    trace!("Broadcast machine-wide notification to all clients");
                    continue;
                }
                if let Some(target) = json.as_ref().and_then(extract_target_client_id) {
                    if let Some(client) = clients.get(&target) {
                        match json.as_ref().and_then(extract_child_session_event) {
                            Some(ChildSessionEvent::Spawned(child_sid)) => {
                                if let Some(parent) = json.as_ref().and_then(extract_session_id) {
                                    child_sessions
                                        .entry(parent)
                                        .or_default()
                                        .insert(child_sid.clone());
                                }
                                debug!(client_id = target.0, child_session_id = %child_sid, "Registered child route from replayed SubagentSpawned");
                                session_subscribers
                                    .entry(child_sid)
                                    .or_default()
                                    .insert(target);
                            }
                            Some(ChildSessionEvent::Finished(child_sid)) => {
                                let emptied =
                                    session_subscribers.get_mut(&child_sid).is_some_and(|subs| {
                                        subs.remove(&target);
                                        subs.is_empty()
                                    });
                                if emptied {
                                    prune_child_route(
                                        &child_sid,
                                        &mut session_subscribers,
                                        &mut session_driver,
                                        &mut child_sessions,
                                    );
                                }
                            }
                            None => {}
                        }
                        let replay_seq = json
                            .as_ref()
                            .and_then(extract_session_id)
                            .zip(json.as_ref().and_then(event_seq_of));
                        match client.tx.try_send(ClientOutbound::Acp(payload)) {
                            Ok(true) => {
                                if let Some((sid, seq)) = replay_seq {
                                    let entry =
                                        load_replay_max_seq.entry((target, sid)).or_insert(0);
                                    *entry = (*entry).max(seq);
                                }
                                trace!(
                                    client_id = target.0,
                                    "Unicast replay notification to loading client"
                                );
                            }
                            Ok(false) => {
                                warn!(
                                    client_id = target.0,
                                    "Replay notification dropped: loading client channel full (not counted toward flush cutoff)"
                                );
                            }
                            Err(e) => {
                                warn!(client_id = target.0, error = %e, "Failed to unicast replay notification to loading client (channel closed)");
                            }
                        }
                    } else {
                        if let Some(ChildSessionEvent::Finished(child_sid)) =
                            json.as_ref().and_then(extract_child_session_event)
                            && session_subscribers
                                .get(&child_sid)
                                .is_none_or(|subs| subs.is_empty())
                        {
                            prune_child_route(
                                &child_sid,
                                &mut session_subscribers,
                                &mut session_driver,
                                &mut child_sessions,
                            );
                        }
                        if orphan_replay_warned.insert(target) {
                            warn!(
                                client_id = target.0,
                                "Dropping targeted replay notification: loading client disconnected mid-replay (rest of burst logged at trace)"
                            );
                        } else {
                            trace!(
                                client_id = target.0,
                                "Dropping targeted replay notification: loading client disconnected mid-replay"
                            );
                        }
                    }
                    continue;
                }
                let session_id = json.as_ref().and_then(extract_session_id).or_else(|| {
                    json.as_ref()
                        .and_then(extract_session_id_from_prompt_complete)
                });
                if let Some(ref sid) = session_id
                    && let Some(tcid) = json
                        .as_ref()
                        .and_then(extract_interaction_resolved_tool_call_id)
                    && let Some(map) = interaction_requests.get_mut(sid.as_str())
                {
                    map.remove(&tcid);
                    if map.is_empty() {
                        interaction_requests.remove(sid.as_str());
                    }
                }
                let is_reverse_request = json
                    .as_ref()
                    .is_some_and(|j| j.get("id").is_some() && j.get("method").is_some());
                let is_inject_prompt = json.as_ref().is_some_and(is_scheduled_task_inject_prompt);
                let is_interaction =
                    is_reverse_request && json.as_ref().is_some_and(is_interaction_request);
                if is_interaction
                    && let Some(ref sid) = session_id
                    && let Some(tcid) = json.as_ref().and_then(extract_interaction_tool_call_id)
                {
                    interaction_requests
                        .entry(sid.clone())
                        .or_default()
                        .insert(tcid, payload.clone());
                }
                if let Some(ref sid) = session_id
                    && session_subscribers.contains_key(sid.as_str())
                {
                    let child_event = json.as_ref().and_then(extract_child_session_event);
                    let event_seq = json.as_ref().and_then(event_seq_of);
                    if (is_reverse_request && !is_interaction) || is_inject_prompt {
                        if let Some(&driver_id) = session_driver.get(sid.as_str()) {
                            if let Some(client) = clients.get(&driver_id) {
                                if let Err(e) =
                                    client.tx.try_send(ClientOutbound::Acp(payload.clone()))
                                {
                                    warn!(client_id = driver_id.0, session_id = sid.as_str(), is_inject = is_inject_prompt, error = %e, "Failed to route driver-only message (channel closed)");
                                } else {
                                    trace!(
                                        client_id = driver_id.0,
                                        session_id = sid.as_str(),
                                        is_inject = is_inject_prompt,
                                        "Routed driver-only message to driver"
                                    );
                                }
                            } else {
                                trace!(
                                    session_id = sid.as_str(),
                                    is_inject = is_inject_prompt,
                                    "Dropping driver-only message: no live driver"
                                );
                            }
                        } else {
                            trace!(
                                session_id = sid.as_str(),
                                is_inject = is_inject_prompt,
                                "Dropping driver-only message: session has no driver"
                            );
                        }
                    } else if let Some(subs) = session_subscribers.get(sid.as_str()) {
                        for &cid in subs.iter() {
                            if let Some(buf) = load_live_buffer.get_mut(&(cid, sid.clone())) {
                                if buf.len() < MAX_BUFFERED_LIVE_PER_LOAD {
                                    buf.push((payload.clone(), event_seq));
                                    trace!(
                                        client_id = cid.0,
                                        session_id = sid.as_str(),
                                        "Buffered live notification during in-flight load"
                                    );
                                    continue;
                                }
                                warn!(
                                    client_id = cid.0,
                                    session_id = sid.as_str(),
                                    "Live buffer for in-flight load exceeded cap; forwarding live (ordering not guaranteed)"
                                );
                            }
                            if let Some(client) = clients.get(&cid) {
                                if let Err(e) =
                                    client.tx.try_send(ClientOutbound::Acp(payload.clone()))
                                {
                                    warn!(client_id = cid.0, session_id = sid.as_str(), error = %e, "Failed to broadcast notification to subscriber (channel closed)");
                                } else {
                                    trace!(
                                        client_id = cid.0,
                                        session_id = sid.as_str(),
                                        "Broadcast notification to subscriber"
                                    );
                                }
                            }
                        }
                    }
                    match child_event {
                        Some(ChildSessionEvent::Spawned(child_sid)) => {
                            let parent_subs = session_subscribers
                                .get(sid.as_str())
                                .cloned()
                                .unwrap_or_default();
                            info!(child_session_id = %child_sid, subscriber_count = parent_subs.len(), "Registered child session from SubagentSpawned");
                            session_subscribers.insert(child_sid.clone(), parent_subs);
                            if let Some(&driver_id) = session_driver.get(sid.as_str()) {
                                session_driver.insert(child_sid.clone(), driver_id);
                            }
                            child_sessions
                                .entry(sid.clone())
                                .or_default()
                                .insert(child_sid);
                        }
                        Some(ChildSessionEvent::Finished(child_sid)) => {
                            debug!(child_session_id = %child_sid, "Deregistered child session from SubagentFinished");
                            prune_child_route(
                                &child_sid,
                                &mut session_subscribers,
                                &mut session_driver,
                                &mut child_sessions,
                            );
                        }
                        None => {}
                    }
                    continue;
                }
                let is_notification = json.as_ref().is_some_and(|j| j.get("id").is_none());
                let is_relay_session_notification = is_notification
                    && session_id
                        .as_ref()
                        .is_some_and(|s| !session_subscribers.contains_key(s.as_str()));
                if !is_notification {
                    trace!("Dropping non-routable response (likely relay-originated)");
                } else if is_relay_session_notification {
                    if let Some(ChildSessionEvent::Finished(child_sid)) =
                        json.as_ref().and_then(extract_child_session_event)
                        && session_subscribers
                            .get(&child_sid)
                            .is_none_or(|subs| subs.is_empty())
                    {
                        prune_child_route(
                            &child_sid,
                            &mut session_subscribers,
                            &mut session_driver,
                            &mut child_sessions,
                        );
                    }
                    trace!(
                        "Dropping notification for relay-owned session (already delivered via WS)"
                    );
                } else if let Some(client_id) = last_active_client
                    && let Some(client) = clients.get(&client_id)
                {
                    debug!(
                        client_id = client_id.0,
                        "Using fallback routing to last active client"
                    );
                    if let Err(e) = client.tx.try_send(ClientOutbound::Acp(payload)) {
                        warn!(client_id = client_id.0, error = %e, "Failed to send notification via fallback routing (channel closed)");
                    }
                } else {
                    debug!("No client available for notification routing, message dropped");
                }
            }
        }
    }
    finalize_workspace_on_shutdown(control_state.clone()).await;
    finalize_cpu_profile_on_shutdown(control_state).await;
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}
fn spawn_client_handler(
    client_id: ClientId,
    stream: LeaderStream,
    server_rx: AsyncReceiver<ClientOutbound>,
    event_tx: AsyncSender<ServerEvent>,
    cancel: CancellationToken,
    ready_rx: watch::Receiver<bool>,
    control_state: LeaderServerControlState,
) {
    tokio::spawn(async move {
        let result = run_client_session(
            client_id,
            stream,
            server_rx,
            event_tx.clone(),
            cancel,
            ready_rx,
            control_state,
        )
        .await;
        if let Err(e) = &result {
            debug!(client_id = client_id.0, error = %e, "Client session ended");
        }
        let _ = event_tx.send(ServerEvent::Disconnected(client_id)).await;
    });
}
async fn run_client_session(
    client_id: ClientId,
    stream: LeaderStream,
    server_rx: AsyncReceiver<ClientOutbound>,
    event_tx: AsyncSender<ServerEvent>,
    cancel: CancellationToken,
    mut ready_rx: watch::Receiver<bool>,
    control_state: LeaderServerControlState,
) -> Result<(), ProtocolError> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let msg: ClientMessage =
        match tokio::time::timeout(REGISTRATION_TIMEOUT, read_message(&mut reader)).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                warn!(client_id = client_id.0, error = %e, "Registration failed");
                return Err(e);
            }
            Err(_) => {
                warn!(
                    client_id = client_id.0,
                    "Registration timeout - client did not register within {:?}",
                    REGISTRATION_TIMEOUT
                );
                let _ = write_message(
                    &mut writer,
                    &ServerMessage::Error {
                        code: 3,
                        message: "Registration timeout".into(),
                    },
                )
                .await;
                return Ok(());
            }
        };
    let (client_type, mode, capabilities, was_ready_at_registration) = match msg {
        ClientMessage::Register {
            client_type,
            mode,
            capabilities,
        } => {
            let ready = *ready_rx.borrow();
            write_message(
                &mut writer,
                &ServerMessage::Registered {
                    client_id: client_id.0,
                    ready,
                    leader_protocol_version: Some(LEADER_PROTOCOL_VERSION),
                    leader_binary_version: Some(
                        control_state.metadata.leader_binary_version.clone(),
                    ),
                    leader_capabilities: Some(control_state.leader_capabilities()),
                },
            )
            .await?;
            (client_type, mode, capabilities, ready)
        }
        _ => {
            write_message(
                &mut writer,
                &ServerMessage::Error {
                    code: 1,
                    message: "Expected Register message".into(),
                },
            )
            .await?;
            return Ok(());
        }
    };
    if !was_ready_at_registration {
        debug!(
            client_id = client_id.0,
            "Client registered before leader ready; waiting for readiness"
        );
        while !*ready_rx.borrow() {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    drain_client_outbound_on_cancel(&server_rx, &mut writer).await;
                    return Ok(());
                }
                result = ready_rx.changed() => {
                    if result.is_err() {
                        // Watch sender was dropped (leader shutting down without ready).
                        return Ok(());
                    }
                    // Loop re-checks *ready_rx.borrow() at top; no Ref held across await.
                }
            }
        }
        write_message(&mut writer, &ServerMessage::LeaderReady).await?;
        debug!(
            client_id = client_id.0,
            "Leader ready; sent LeaderReady to client"
        );
    }
    let _ = event_tx
        .send(ServerEvent::Registered(
            client_id,
            mode,
            capabilities.clone(),
            client_type.clone(),
        ))
        .await;
    info!(client_id = client_id.0, client_type = %client_type, ?mode, yolo_mode = capabilities.yolo_mode, client_version = ?capabilities.client_version, "Client registered");
    loop {
        tokio::select! {
            biased;

            _ = cancel.cancelled() => {
                drain_client_outbound_on_cancel(&server_rx, &mut writer).await;
                break;
            }

            Ok(msg) = server_rx.recv() => {
                if write_outbound(&mut writer, &msg).await.is_err() {
                    break;
                }
            }

            msg_result = read_message::<_, ClientMessage>(&mut reader) => {
                match handle_client_inbound_message(
                    msg_result,
                    client_id,
                    &event_tx,
                    &mut writer,
                )
                .await?
                {
                    ClientSessionAction::Continue => {}
                    ClientSessionAction::Break => break,
                }
            }
        }
    }
    Ok(())
}
enum ClientSessionAction {
    Continue,
    Break,
}
async fn drain_client_outbound_on_cancel<W>(
    server_rx: &AsyncReceiver<ClientOutbound>,
    writer: &mut W,
) where
    W: tokio::io::AsyncWrite + Unpin,
{
    for _ in 0..10 {
        if !server_rx.is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    while let Ok(Some(msg)) = server_rx.try_recv() {
        if write_outbound(writer, &msg).await.is_err() {
            break;
        }
    }
}
async fn handle_client_inbound_message<W>(
    msg_result: Result<ClientMessage, ProtocolError>,
    client_id: ClientId,
    event_tx: &AsyncSender<ServerEvent>,
    writer: &mut W,
) -> Result<ClientSessionAction, ProtocolError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg_result {
        Ok(msg @ (ClientMessage::Acp { .. } | ClientMessage::Control { .. })) => {
            let _ = event_tx.send(ServerEvent::Message(client_id, msg)).await;
            Ok(ClientSessionAction::Continue)
        }
        Ok(ClientMessage::Ping) => {
            write_message(writer, &ServerMessage::Pong).await?;
            Ok(ClientSessionAction::Continue)
        }
        Ok(ClientMessage::Disconnect) | Err(ProtocolError::ConnectionClosed) => {
            info!(client_id = client_id.0, "Client disconnected");
            Ok(ClientSessionAction::Break)
        }
        Ok(ClientMessage::Register { .. }) => {
            write_message(
                writer,
                &ServerMessage::Error {
                    code: 2,
                    message: "Already registered".into(),
                },
            )
            .await?;
            Ok(ClientSessionAction::Continue)
        }
        Err(e) => {
            warn!(client_id = client_id.0, error = %e, "Protocol error");
            Ok(ClientSessionAction::Break)
        }
    }
}
/// Broadcast a planned shutdown to all connected clients.
///
/// Sends `ShuttingDown` (advance notice with reason and `delay_ms: 0`)
/// followed immediately by `Shutdown`. Both messages are sent before the
/// server exits, so clients that process the channel quickly will see both.
///
/// `delay_ms` is set to 0 because the server sends `Shutdown` immediately
/// after `ShuttingDown` — there is no actual grace period. The cancel token
/// propagates to client session handlers simultaneously, so a sleep between
/// the two messages would allow session writers to exit before `Shutdown`
/// is delivered. Clients should treat `ShuttingDown` as a signal that
/// `Shutdown` is imminent and pre-arm their reconnection handlers.
async fn broadcast_shutdown(
    clients: &HashMap<ClientId, ClientState>,
    reason: super::protocol::ShutdownReason,
) {
    for client in clients.values() {
        let _ = client
            .tx
            .send(
                ServerMessage::ShuttingDown {
                    reason: reason.clone(),
                    delay_ms: 0,
                }
                .into(),
            )
            .await;
        let _ = client.tx.send(ServerMessage::Shutdown.into()).await;
    }
}
pub struct ServerHandle {
    pub cancel: CancellationToken,
    /// Receive ACP messages from clients (server routes them here)
    pub acp_rx: mpsc::UnboundedReceiver<String>,
    /// Send ACP responses back (server routes to correct client based on request ID)
    pub response_tx: mpsc::UnboundedSender<String>,
    /// Atomic counter tracking the number of connected clients
    pub client_count: Arc<AtomicUsize>,
    /// Atomic flag: `true` while the agent has pending (in-flight) requests
    pub agent_busy: Arc<AtomicBool>,
    /// Signal the IPC server that the leader is fully ready (socket bound + bounded auth;
    /// catalog/settings refresh runs in the background).
    ///
    /// Send `true` once the leader has finished initializing. Until then, ACP requests
    /// receive a `leader_starting` error and ACP notifications are dropped.
    ///
    /// `spawn_leader_server` sends `true` immediately so that callers that do not need
    /// staged startup (e.g. tests, in-process use) get a fully-ready server out of the box.
    /// Production leader startup (`run_leader`) holds this back until bounded auth completes
    /// (catalog/settings are no longer prefetched; they refresh in the background).
    pub ready_tx: watch::Sender<bool>,
    /// Set the shutdown reason before cancelling so clients receive the correct `ShuttingDown`
    /// reason. The default value is [`ShutdownReason::Manual`]; send
    /// [`ShutdownReason::AutoUpdate`] before cancelling for auto-update shutdowns.
    pub shutdown_tx: watch::Sender<super::protocol::ShutdownReason>,
    /// Observe relay demand: flips to `true` when the first headless client
    /// registers (see `relay_demand_tx` on [`run_leader_server`]).
    pub relay_demand_rx: watch::Receiver<bool>,
    /// Leader-local control metadata and CPU profiling state, exposed for tests.
    pub control_state: LeaderServerControlState,
}
fn default_test_control_state(socket_path: &Path) -> LeaderServerControlState {
    LeaderServerControlState::new(LeaderServerMetadata {
        pid: std::process::id(),
        socket_path: socket_path.to_path_buf(),
        lock_path: socket_path.with_extension("lock"),
        ws_url_suffix: String::new(),
        leader_binary_version: env!("CARGO_PKG_VERSION").to_string(),
    })
}
pub async fn spawn_leader_server(socket_path: PathBuf) -> Result<ServerHandle, ServerError> {
    let (acp_tx, acp_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let client_count = Arc::new(AtomicUsize::new(0));
    let agent_busy = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = watch::channel(true);
    let (shutdown_tx, _shutdown_reason_rx) =
        watch::channel(super::protocol::ShutdownReason::Manual);
    let (relay_demand_tx, relay_demand_rx) = watch::channel(false);
    let control_state = default_test_control_state(&socket_path);
    let cancel_clone = cancel.clone();
    let socket_path_clone = socket_path.clone();
    let client_count_clone = client_count.clone();
    let agent_busy_clone = agent_busy.clone();
    let control_state_for_server = control_state.clone();
    let shutdown_tx_for_server = shutdown_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = run_leader_server(
            socket_path_clone,
            acp_tx,
            response_rx,
            cancel_clone,
            false,
            client_count_clone,
            agent_busy_clone,
            AgentActivity::default(),
            ready_rx,
            relay_demand_tx,
            shutdown_tx_for_server,
            None,
            control_state_for_server,
        )
        .await
        {
            error!(error = %e, "Leader server error");
        }
    });
    Ok(ServerHandle {
        cancel,
        acp_rx,
        response_tx,
        client_count,
        agent_busy,
        ready_tx,
        shutdown_tx,
        relay_demand_rx,
        control_state,
    })
}
#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
