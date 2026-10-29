//! MCP server integration using the official rmcp SDK.

use std::collections::HashMap;
use std::ffi::OsString;
use std::future::Future;
use std::sync::{Arc, LazyLock};

use agent_client_protocol as acp;
use futures::StreamExt;
use regex::Regex;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    process::{ChildStderr, Command},
    sync::{Mutex, Notify},
};

use rmcp::{
    ClientHandler, ServiceExt,
    model::{
        CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation,
        PaginatedRequestParams,
    },
    service::{
        ClientInitializeError, NotificationContext, RequestContext, RoleClient, RunningService,
        ServiceError,
    },
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{
        StreamableHttpClientTransport, Transport,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};

use crate::oauth_config::McpOAuthConfig;

use echo_agent_tools::types::{
    output::{MCPOutput, MCPOutputDetails, ToolOutput},
    tool::{ToolKind, ToolNamespace},
    tool_metadata::ToolMetadata,
};
use echo_agent_tools::util::{ProcessGroup, ProcessScope};

/// MCP tool name delimiter: server names are qualified as `"server__tool"`.
/// Canonical definition lives in `echo_agent_workspace_types`; re-exported here
/// for callers that historically imported it from this module.
pub use echo_agent_workspace_types::MCP_TOOL_NAME_DELIMITER;

/// Reqwest 0.13 twin of the 0.12 adapters in `echo_agent_extra_ca`.
fn with_extra_root_certificates(mut builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    echo_agent_extra_ca::ensure_default_crypto_provider();
    builder = builder.tls_backend_rustls();
    for der in echo_agent_extra_ca::extra_root_ders() {
        match reqwest::Certificate::from_der(der) {
            Ok(cert) => builder = builder.add_root_certificate(cert),
            Err(e) => tracing::warn!(
                error = %e,
                "extra CA bundle: validated DER rejected by reqwest 0.13; skipping cert"
            ),
        }
    }
    builder
}

/// Regex for strictest cross-provider tool name validation.
///
/// Requirements across providers:
/// - Anthropic/OpenAI: `^[a-zA-Z0-9_-]{1,64}$` (allows starting with digit/hyphen)
/// - Google Gemini: `^[a-zA-Z_][a-zA-Z0-9_.-]{0,63}$` (must start with letter/underscore, allows dots)
///
/// Strictest common denominator: must start with letter/underscore, only alphanumeric/_/- allowed, max 64 chars.
static TOOL_NAME_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z_][a-zA-Z0-9_-]{0,63}$").unwrap());

/// Validate that a tool name matches the strictest cross-provider LLM API requirements.
///
/// Pattern: `^[a-zA-Z_][a-zA-Z0-9_-]{0,63}$`
/// - Must start with a letter or underscore (Gemini requirement)
/// - Only letters, digits, underscores, hyphens allowed (no dots — Anthropic/OpenAI requirement)
/// - Maximum 64 characters
///
/// Returns `Ok(())` if valid, or `Err(reason)` if invalid.
pub fn validate_tool_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("tool name cannot be empty".to_string());
    }
    if !TOOL_NAME_REGEX.is_match(name) {
        return Err(format!(
            "tool name '{}' is invalid — must match ^[a-zA-Z_][a-zA-Z0-9_-]{{0,63}}$ (start with letter/underscore, max 64 chars)",
            name
        ));
    }
    Ok(())
}

/// Max protocol icons kept per server/tool at ingest.
pub const MAX_MCP_ICONS_PER_ENTITY: usize = 8;

/// Max bytes for a single icon `src` (including data URIs) at ingest.
pub const MAX_MCP_ICON_SRC_BYTES: usize = 64 * 1024;

/// Max bytes for a single icon `mime_type` string at ingest.
pub const MAX_MCP_ICON_MIME_TYPE_BYTES: usize = 128;

/// Max size tokens kept per icon (`48x48`, `any`, …) at ingest.
pub const MAX_MCP_ICON_SIZES: usize = 8;

/// Max bytes for a single size token at ingest.
pub const MAX_MCP_ICON_SIZE_TOKEN_BYTES: usize = 32;

/// Wire theme for MCP protocol icons.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpIconTheme {
    Light,
    Dark,
    #[serde(other)]
    Unknown,
}

/// ACP-facing MCP protocol icon (SEP-973), mirrored from rmcp so clients
/// never depend on the quarantined SDK types.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpIcon {
    pub src: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sizes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<McpIconTheme>,
}

impl McpIcon {
    /// Convert an rmcp icon with ingest rules: trim `src`, allow only
    /// `https://` and `data:image/…`, drop empty/oversized values.
    pub fn from_rmcp(icon: rmcp::model::Icon) -> Option<Self> {
        let src = icon.src.trim();
        if src.is_empty() || src.len() > MAX_MCP_ICON_SRC_BYTES {
            return None;
        }
        if !is_allowed_mcp_icon_src(src) {
            return None;
        }
        let theme = match icon.theme {
            Some(rmcp::model::IconTheme::Light) => Some(McpIconTheme::Light),
            Some(rmcp::model::IconTheme::Dark) => Some(McpIconTheme::Dark),
            _ => None,
        };
        let mime_type = icon.mime_type.and_then(|mime| {
            let mime = mime.trim();
            if mime.is_empty() || mime.len() > MAX_MCP_ICON_MIME_TYPE_BYTES {
                None
            } else {
                Some(mime.to_owned())
            }
        });
        let sizes = icon.sizes.map(|sizes| {
            sizes
                .into_iter()
                .filter_map(|size| {
                    let size = size.trim();
                    if size.is_empty() || size.len() > MAX_MCP_ICON_SIZE_TOKEN_BYTES {
                        None
                    } else {
                        Some(size.to_owned())
                    }
                })
                .take(MAX_MCP_ICON_SIZES)
                .collect::<Vec<_>>()
        });
        let sizes = sizes.filter(|sizes| !sizes.is_empty());
        Some(Self {
            src: src.to_owned(),
            mime_type,
            sizes,
            theme,
        })
    }

    pub fn from_rmcp_list(icons: Option<Vec<rmcp::model::Icon>>) -> Vec<Self> {
        icons
            .unwrap_or_default()
            .into_iter()
            .filter_map(Self::from_rmcp)
            .take(MAX_MCP_ICONS_PER_ENTITY)
            .collect()
    }
}

fn is_allowed_mcp_icon_src(src: &str) -> bool {
    src.starts_with("data:image/") || src.starts_with("https://")
}

/// Sanitize an MCP server or tool name into a single safe path segment
/// (e.g. `"user-Hugging Face"` becomes `user-Hugging_Face`). Shared so the
/// per-server folder advertised in the prompt matches the tool files on disk.
pub fn sanitize_descriptor_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Result of a diff-based MCP config update.
#[derive(Debug)]
pub struct McpConfigDiff {
    /// Server names that are new or had their config changed.
    pub added: Vec<McpServerName>,
    /// Server names that were removed or had their config changed (old instance torn down).
    pub removed: Vec<McpServerName>,
    /// Server names whose config is identical — clients kept alive.
    pub retained: Vec<McpServerName>,
}

/// MCP server name used as the key in client/tool maps (e.g. `"github"`, `"echo_agent_com_linear"`).
pub type McpServerName = String;

/// Unqualified MCP tool name (e.g. `"create_issue"`, without the `server__` prefix).
type ToolName = String;

/// Typed state machine for MCP-pool initialization.
///
/// Replaces the previous trio of correlated fields — `initialized: bool`,
/// `initializing: bool`, `initializing_servers: HashSet<McpServerName>` —
/// whose product space could represent nonsensical combinations such as
/// "initialized AND initializing" or "no init started AND per-server
/// handshakes outstanding". With one enum field, every legal state has
/// exactly one representation and the compiler enforces exhaustiveness
/// at every match site.
///
/// Lifecycle:
///
/// ```text
///   ┌─────────────┐  try_start  ┌──────────────────────┐
///   │  NotStarted │ ──────────▶ │  Starting{handshakes}│
///   └─────────────┘ ◀── cancel ─┴──────────┬───────────┘
///         ▲                                │ finish
///         │ cancel                         ▼
///         │                  ┌──────────────────────────┐
///         └──────────────────┤  Finished{handshakes}    │
///                            └──────────────────────────┘
/// ```
///
/// `Starting` is the pre-`finish_init` window; `Finished` is the post-
/// `finish_init` window where per-server background handshakes may still
/// be draining. `is_complete()` requires `Finished` with an empty
/// handshaking set.
#[derive(Debug, Default)]
pub enum InitProgress {
    /// Init has never been started, or was cancelled / reset by a
    /// config change.
    #[default]
    NotStarted,
    /// `try_start_init` was called; per-server tasks may be spawning;
    /// `finish_init` has NOT yet fired. `handshaking` tracks the set of
    /// servers whose background handshake is in flight.
    Starting {
        handshaking: std::collections::HashSet<McpServerName>,
    },
    /// `finish_init` fired (deliberately early, so the session is not
    /// blocked on MCP for non-MCP work). Background per-server
    /// handshakes may still be running; `handshaking` shrinks as each
    /// completes. `is_complete()` returns `true` only when it is empty.
    Finished {
        handshaking: std::collections::HashSet<McpServerName>,
    },
}

impl InitProgress {
    /// True iff every per-server handshake has settled and `finish_init`
    /// has fired. Pairs with [`Self::is_in_progress`].
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Finished { handshaking } if handshaking.is_empty())
    }

    /// True iff any init work is outstanding — either we are pre-
    /// `finish_init`, or per-server handshakes are still in flight in
    /// the background.
    pub fn is_in_progress(&self) -> bool {
        match self {
            Self::Starting { .. } => true,
            Self::Finished { handshaking } => !handshaking.is_empty(),
            Self::NotStarted => false,
        }
    }

    /// True iff `finish_init` has fired, regardless of whether
    /// background handshakes are still draining. Used for diagnostic
    /// logging where the caller wants to distinguish pre-finish from
    /// post-finish-with-bg-work.
    pub fn has_finished_init(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }

    /// True iff the named server is currently handshaking.
    pub fn is_server_handshaking(&self, name: &str) -> bool {
        match self {
            Self::Starting { handshaking } | Self::Finished { handshaking } => {
                handshaking.contains(name)
            }
            Self::NotStarted => false,
        }
    }

    /// Iterate over server names whose background handshake is still
    /// in flight. Empty when [`Self::NotStarted`] or fully complete.
    pub fn handshaking_servers(&self) -> impl Iterator<Item = &McpServerName> {
        match self {
            Self::Starting { handshaking } | Self::Finished { handshaking } => {
                Some(handshaking.iter())
            }
            Self::NotStarted => None,
        }
        .into_iter()
        .flatten()
    }

    /// Number of in-flight per-server handshakes.
    pub fn handshaking_count(&self) -> usize {
        match self {
            Self::Starting { handshaking } | Self::Finished { handshaking } => handshaking.len(),
            Self::NotStarted => 0,
        }
    }

    /// Transition `NotStarted` → `Starting { ∅ }`. Returns `true` on
    /// successful transition, `false` if init was already started or
    /// finished (mirrors the pre-refactor `try_start_init` contract).
    pub fn try_start(&mut self) -> bool {
        if matches!(self, Self::NotStarted) {
            *self = Self::Starting {
                handshaking: std::collections::HashSet::new(),
            };
            true
        } else {
            false
        }
    }

    /// Transition `Starting { hs }` → `Finished { hs }`, preserving the
    /// handshaking set. No-op if already `Finished`; no-op-with-log if
    /// called from `NotStarted` (defensive — that would be a caller bug).
    pub fn finish(&mut self) {
        match self {
            Self::Starting { handshaking } => {
                // Move only the inner set, leaving the outer `&mut self`
                // ready to be reassigned without going through
                // `mem::take(self)` (which would force a `NotStarted`
                // placeholder and a redundant put-back in the
                // already-Finished arm below).
                let handshaking = std::mem::take(handshaking);
                *self = Self::Finished { handshaking };
            }
            // Idempotent: already past the finish boundary. Per-server
            // handshakes continue draining via `mark_handshake_complete`.
            Self::Finished { .. } => {}
            Self::NotStarted => {
                tracing::warn!(
                    "InitProgress::finish called from NotStarted; staying in NotStarted"
                );
            }
        }
    }

    /// Transition any state → `NotStarted`. Clears all per-server
    /// progress. Used on generation mismatch (config change racing
    /// active init) and on full reset.
    pub fn cancel(&mut self) {
        *self = Self::NotStarted;
    }

    /// Add names to the handshaking set. Only meaningful in `Starting`
    /// or `Finished`; warns if called from `NotStarted` (that would
    /// mean a per-server handshake started without a `try_start_init`,
    /// which is a caller bug).
    pub fn mark_handshaking(&mut self, names: impl IntoIterator<Item = McpServerName>) {
        match self {
            Self::Starting { handshaking } | Self::Finished { handshaking } => {
                handshaking.extend(names);
            }
            Self::NotStarted => {
                tracing::warn!("InitProgress::mark_handshaking called from NotStarted; ignoring");
            }
        }
    }

    /// Remove a server from the handshaking set (on success or failure
    /// of its handshake). No-op if not present or if `NotStarted`.
    pub fn mark_handshake_complete(&mut self, name: &str) {
        match self {
            Self::Starting { handshaking } | Self::Finished { handshaking } => {
                handshaking.remove(name);
            }
            Self::NotStarted => {}
        }
    }

    /// Clear the handshaking set entirely. Used by the proxy-mode and
    /// bg-handshake completion paths as a defensive sweep after the
    /// per-server `mark_handshake_complete` calls — ensures the set is
    /// empty before/after `finish_init` fires.
    pub fn clear_handshaking(&mut self) {
        match self {
            Self::Starting { handshaking } | Self::Finished { handshaking } => {
                handshaking.clear();
            }
            Self::NotStarted => {}
        }
    }
}

/// One in-process SDK MCP server registration: its tool-namespace name and the
/// SDK-side id echoed back in `echo.agent/mcp/sdk_call`. A named struct (rather than a
/// `(String, String)` tuple) so callers can't transpose the two strings.
///
/// `Deserialize`d straight from a `_meta["echo.agent/mcp/servers"]` entry, so the
/// `serverId` wire field name is declared (and serde-checked) exactly once here.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AcpServerEntry {
    pub name: McpServerName,
    #[serde(rename = "serverId")]
    pub server_id: String,
}

/// The session's in-process SDK MCP servers (declared via `_meta["echo.agent/mcp/servers"]`,
/// reached over the ACP reverse channel), bundled with the shared reverse-RPC invoker.
/// Held as `McpState::acp_mcp: Option<_>` so the set is one atom — present together or
/// absent, never "servers without an invoker" — and survives `update_configs` clears
/// (config reloads only touch `configs`/`owned_clients`). Per-server config.toml overrides
/// are NOT cached here — they are re-resolved per init (see [`McpState::build_pending_acp_clients`]).
struct AcpMcpRegistry {
    /// Registered servers (`name -> serverId`).
    servers: Vec<AcpServerEntry>,
    /// Shared reverse-RPC invoker all these servers' tools are called through (emits
    /// `echo.agent/mcp/sdk_call` over the ACP connection).
    invoker: Arc<dyn crate::acp_transport::AcpReverseInvoker>,
}

/// Consolidated MCP state behind a single lock. Generation counter detects stale inits.
pub struct McpState {
    pub configs: Vec<acp::McpServer>,
    pub meta_config_map: McpMetaConfigMap,
    /// Clients owned by this session; cleared on config changes.
    pub owned_clients: crate::owned_clients::OwnedClients,
    /// Clients inherited from parent via `SharedMcpPool`; never cleared by config changes.
    pub shared_clients: HashMap<McpServerName, Arc<McpClient>>,
    /// The session's in-process SDK MCP servers + their shared invoker/overrides; `None`
    /// when the session has none. See [`AcpMcpRegistry`]. Kept out of `configs` (the closed
    /// `acp::McpServer` enum) so it survives `update_configs` clears.
    acp_mcp: Option<AcpMcpRegistry>,
    /// Encapsulated init lifecycle. Access via [`Self::is_initialized`],
    /// [`Self::is_initializing`], [`Self::try_start_init`],
    /// [`Self::finish_init`], etc. — those route through a single
    /// [`InitProgress`] state machine that rules out nonsensical
    /// combinations like "initialized AND initializing".
    ///
    /// Private on purpose: external callers must go through the typed
    /// transition methods, not poke the variant directly.
    init_progress: InitProgress,
    pub generation: u64,
    /// Qualified tool name → `_meta` from MCP tools/list. Populated during init.
    pub mcp_tool_meta: HashMap<String, serde_json::Value>,
    /// Qualified tool name → protocol `icons` from MCP tools/list.
    pub mcp_tool_icons: HashMap<String, Vec<McpIcon>>,
    /// HTTP servers that support OAuth but haven't been authenticated yet.
    pub auth_required: std::collections::HashSet<McpServerName>,
    /// Servers whose background init failed (handshake error, `tools/list`
    /// error, or overall init timeout) even though a client object exists,
    /// mapped to a short failure cause surfaced to the model in the MCP
    /// reminder. Surfaced as `Unavailable` in status snapshots so a server
    /// that connected but never finished initializing — e.g. wedged on
    /// `tools/list` and registered zero tools — does not misleadingly show
    /// as `Ready`. Cleared when the server begins a fresh init attempt.
    pub init_failed: std::collections::HashMap<McpServerName, String>,
    /// Per-server set of unqualified tool names that the user has disabled.
    /// Persisted to `~/.echo-agent/config.toml` under `[mcp_servers.<name>].disabled_tools`.
    pub disabled_tools: HashMap<McpServerName, std::collections::HashSet<ToolName>>,
    /// Stashed registrations for disabled tools so they can be re-enabled
    /// without a full MCP re-init (no need to call `list_tools` again).
    pub disabled_tool_registrations: HashMap<String, McpToolRegistration>,
    event_writer: echo_agent_session_events::EventWriter,
    /// Sender wired by the session actor to its `StatusDispatcher`
    /// task.  When `Some`, the state — and every [`McpClient`] reached
    /// through [`Self::all_clients`] / [`Self::get_client`] — forwards
    /// [`McpClientEvent`]s here for coalescing and fan-out as ACP
    /// `echo.agent/mcp/server_status` notifications.
    ///
    /// Intentionally `None` in subagent-pool / shared-pool snapshots
    /// ([`SharedMcpPool`]) where the **parent** session is the
    /// single owner of liveness/notification flow. Clients in those
    /// snapshots inherit the parent's `Arc<McpClient>` (with the
    /// parent's `notify_tx` slot still pointing at the parent), so
    /// duplicating event flow into a subagent would just double-push
    /// every event.
    ///
    /// Populated by [`Self::set_client_event_tx`], which fans the
    /// sender into every existing client's `notify_tx` slot.
    ///
    /// **Private on purpose.** Callers MUST go through
    /// [`Self::set_client_event_tx`] so the sender is fanned out into
    /// every existing `owned_clients` entry; a direct field write
    /// (`state.client_event_tx = Some(tx)`) would leave all
    /// already-owned clients with `notify_tx = None`, silently
    /// dropping `tools/list_changed`, `Ready`, and `HandshakeFailed`
    /// emits for them. Read access is via [`Self::client_event_tx`].
    client_event_tx: Option<tokio::sync::mpsc::UnboundedSender<McpClientEvent>>,
    elicitation_job_tx: Option<crate::elicitation::ElicitationInbox>,
}

impl McpState {
    pub fn new(configs: Vec<acp::McpServer>) -> Self {
        Self::new_with_meta(configs, McpMetaConfigMap::new())
    }

    pub fn new_with_meta(configs: Vec<acp::McpServer>, meta_config_map: McpMetaConfigMap) -> Self {
        Self {
            configs,
            meta_config_map,
            owned_clients: crate::owned_clients::OwnedClients::new(),
            shared_clients: HashMap::new(),
            acp_mcp: None,
            init_progress: InitProgress::default(),
            generation: 0,
            mcp_tool_meta: HashMap::new(),
            mcp_tool_icons: HashMap::new(),
            auth_required: std::collections::HashSet::new(),
            init_failed: HashMap::new(),
            disabled_tools: HashMap::new(),
            disabled_tool_registrations: HashMap::new(),
            event_writer: echo_agent_session_events::EventWriter::noop(),
            client_event_tx: None,
            elicitation_job_tx: None,
        }
    }

    /// Install (or remove) the [`McpClientEvent`] sender owned by the
    /// session-actor `StatusDispatcher`.
    ///
    /// Synchronous: the per-client slot is a `parking_lot::Mutex`, so
    /// the iteration no longer holds `&mut McpState` across `.await`.
    ///
    /// Side effect: clones the sender into every existing client's
    /// shared `notify_tx` slot. New clients added later (e.g. on a
    /// config diff that re-spawns a server) MUST be wired by the
    /// caller post-construction — typically by calling
    /// [`McpClient::set_event_tx`] **before**
    /// `get_tool_registrations` (so `ensure_initialized`'s
    /// `Ready`/`HandshakeFailed` emit fires with `Some(tx)` and the
    /// `EchoAgentClientHandler` cloned during `try_handshake` reads
    /// through the same Arc).
    pub fn set_client_event_tx(
        &mut self,
        tx: Option<tokio::sync::mpsc::UnboundedSender<McpClientEvent>>,
    ) {
        self.client_event_tx = tx.clone();
        for client in self.owned_clients.values() {
            client.set_event_tx(tx.clone());
        }
        // Shared clients are intentionally NOT wired here: see the
        // `client_event_tx` doc-comment for why a subagent must not
        // duplicate the parent's event flow.
    }

    /// Read-only access to the installed [`McpClientEvent`] sender.
    ///
    /// Returns a clone of the sender wired by
    /// [`Self::set_client_event_tx`], or `None` if no dispatcher is
    /// attached (subagent / shared-pool snapshot). Exposed as a getter
    /// rather than a `pub` field so the fan-out contract documented on
    /// `client_event_tx` cannot be bypassed by a direct assignment.
    pub fn client_event_tx(&self) -> Option<tokio::sync::mpsc::UnboundedSender<McpClientEvent>> {
        self.client_event_tx.clone()
    }

    pub fn set_elicitation_tx(&mut self, tx: Option<crate::elicitation::ElicitationInbox>) {
        for client in self.owned_clients.values() {
            client.set_elicitation_tx(tx.clone());
        }
        self.elicitation_job_tx = tx;
    }

    pub fn elicitation_tx(&self) -> Option<crate::elicitation::ElicitationInbox> {
        self.elicitation_job_tx.clone()
    }

    pub fn set_event_writer(&mut self, writer: echo_agent_session_events::EventWriter) {
        self.event_writer = writer;
    }

    pub fn event_writer(&self) -> &echo_agent_session_events::EventWriter {
        &self.event_writer
    }

    /// Snapshot tool icons for `mcp/list`. Empty clears any prior entry so
    /// a tools/list refresh without icons does not keep a stale set.
    pub fn record_tool_icons(&mut self, qualified_name: String, icons: Vec<McpIcon>) {
        if icons.is_empty() {
            self.mcp_tool_icons.remove(&qualified_name);
        } else {
            self.mcp_tool_icons.insert(qualified_name, icons);
        }
    }

    /// Register the session's in-process SDK MCP servers (`name -> serverId`) plus the
    /// reverse-RPC invoker. Held across `update_configs` clears so each init re-adds them.
    pub fn set_acp_servers(
        &mut self,
        servers: Vec<AcpServerEntry>,
        invoker: Arc<dyn crate::acp_transport::AcpReverseInvoker>,
    ) {
        self.acp_mcp = Some(AcpMcpRegistry { servers, invoker });
    }

    /// Whether any in-process SDK MCP servers are registered (so the session knows to
    /// run MCP init even with no `configs`).
    pub fn has_acp_servers(&self) -> bool {
        self.acp_mcp
            .as_ref()
            .is_some_and(|acp| !acp.servers.is_empty())
    }

    /// Registered SDK servers not yet connected (no owned/shared client) — the ones an
    /// init pass should build. Shared by [`build_pending_acp_clients`] and
    /// [`pending_acp_server_names`] so the "what to build" filter lives in one place.
    fn pending_acp_entries(&self) -> impl Iterator<Item = &AcpServerEntry> {
        self.acp_mcp.iter().flat_map(|acp| {
            acp.servers.iter().filter(|entry| {
                !self.owned_clients.contains_key(&entry.name)
                    && !self.shared_clients.contains_key(&entry.name)
            })
        })
    }

    /// Names of the SDK servers [`build_pending_acp_clients`] will build — used to mark
    /// them initializing before the (async) build.
    pub fn pending_acp_server_names(&self) -> Vec<String> {
        self.pending_acp_entries()
            .map(|entry| entry.name.to_string())
            .collect()
    }

    /// Build [`McpClient`]s for registered ACP servers not already connected. Appended to the
    /// init handshake batch so they register tools + land in `owned_clients` on the SAME path
    /// as HTTP/stdio servers.
    ///
    /// `overrides` is the per-server config.toml tuning (keyed by server name), resolved by
    /// the caller per init — kept caller-side so this method stays pure (no file I/O under
    /// the `McpState` lock).
    pub fn build_pending_acp_clients(
        &self,
        overrides: &HashMap<String, McpClientTimeoutOverrides>,
    ) -> Vec<McpClient> {
        let Some(acp) = &self.acp_mcp else {
            return Vec::new();
        };
        self.pending_acp_entries()
            .map(|entry| {
                McpClient::new_acp(
                    entry.name.clone(),
                    entry.server_id.clone(),
                    acp.invoker.clone(),
                    overrides.get(&entry.name),
                    self.meta_config_map.get(&entry.name),
                )
            })
            .collect()
    }

    /// Check if a specific tool is disabled for a server (unqualified tool name).
    pub fn is_tool_disabled(&self, server_name: &str, tool_name: &str) -> bool {
        self.disabled_tools
            .get(server_name)
            .is_some_and(|set| set.contains(tool_name))
    }

    /// Update configs and reset initialization state.
    /// Returns true if the configs actually changed, false if they were identical.
    pub fn update_configs(&mut self, new_configs: Vec<acp::McpServer>) -> bool {
        if mcp_servers_equal(&self.configs, &new_configs) {
            tracing::debug!("MCP configs unchanged, skipping update");
            return false;
        }

        // Clear owned clients only — shared (inherited) clients are untouched.
        self.owned_clients.clear();
        self.mcp_tool_meta.clear();
        self.mcp_tool_icons.clear();
        self.disabled_tool_registrations.clear();
        self.configs = new_configs;
        self.init_progress.cancel();
        self.auth_required.clear();
        self.init_failed.clear();
        self.generation = self.generation.wrapping_add(1);
        true
    }

    /// Per-server teardown shared by config-update paths: forget every piece
    /// of per-server state so a removed or changed server leaves nothing
    /// stale behind.
    fn forget_server(&mut self, name: &str) {
        self.owned_clients.remove(name);
        self.auth_required.remove(name);
        self.init_failed.remove(name);
        self.init_progress.mark_handshake_complete(name);
        let prefix = format!("{}{}", name, MCP_TOOL_NAME_DELIMITER);
        self.mcp_tool_meta.retain(|k, _| !k.starts_with(&prefix));
        self.mcp_tool_icons.retain(|k, _| !k.starts_with(&prefix));
        self.disabled_tool_registrations
            .retain(|k, _| !k.starts_with(&prefix));
    }

    /// Diff-based config update: only tears down servers whose config changed
    /// or were removed, keeps healthy unchanged servers alive.
    ///
    /// Returns `None` if configs are identical (no work needed), or `Some(diff)`
    /// describing which servers to add/remove.
    pub fn update_configs_diff(
        &mut self,
        new_configs: Vec<acp::McpServer>,
    ) -> Option<McpConfigDiff> {
        if mcp_servers_equal(&self.configs, &new_configs) {
            tracing::debug!("MCP configs unchanged, skipping update");
            return None;
        }

        let old_by_name: HashMap<&str, String> = self
            .configs
            .iter()
            .filter_map(|c| match serde_json::to_string(c) {
                Ok(json) => Some((mcp_server_name(c), json)),
                Err(e) => {
                    tracing::warn!(server = mcp_server_name(c), error = %e, "Failed to serialize MCP server config for diff");
                    None
                }
            })
            .collect();

        let new_by_name: HashMap<&str, String> = new_configs
            .iter()
            .filter_map(|c| match serde_json::to_string(c) {
                Ok(json) => Some((mcp_server_name(c), json)),
                Err(e) => {
                    tracing::warn!(server = mcp_server_name(c), error = %e, "Failed to serialize MCP server config for diff");
                    None
                }
            })
            .collect();

        let mut removed = Vec::new();
        let mut added = Vec::new();
        let mut retained = Vec::new();

        for (name, old_json) in &old_by_name {
            match new_by_name.get(name) {
                None => removed.push(name.to_string()),
                Some(new_json) if new_json != old_json => {
                    removed.push(name.to_string());
                }
                Some(_) => retained.push(name.to_string()),
            }
        }

        for (name, new_json) in &new_by_name {
            match old_by_name.get(name) {
                None => added.push(name.to_string()),
                Some(old_json) if old_json != new_json => {
                    added.push(name.to_string());
                }
                Some(_) => {}
            }
        }

        for name in &removed {
            self.forget_server(name);
        }

        tracing::info!(
            retained = retained.len(),
            added = added.len(),
            removed = removed.len(),
            "MCP config diff: {} retained, {} added, {} removed",
            retained.len(),
            added.len(),
            removed.len(),
        );

        self.configs = new_configs;
        self.init_progress.cancel();
        self.generation = self.generation.wrapping_add(1);

        Some(McpConfigDiff {
            added,
            removed,
            retained,
        })
    }

    /// Returns `true` only when MCP setup is fully complete: the
    /// init lifecycle reached [`InitProgress::Finished`] AND every
    /// per-server background handshake has settled (success or failure).
    ///
    /// The strict per-server check matters because session actors call
    /// [`Self::finish_init`] **early** (right after spawning processes,
    /// before any handshake completes) so the session isn't blocked on
    /// MCP for non-MCP work. Callers that gate MCP-tool dispatch on
    /// "is MCP actually ready" — e.g. the Blocking-strategy waits in
    /// `prepare_tool_definitions_timed`, `wait_for_mcp_initialized`,
    /// and the tool-dispatch fast path — therefore need the *combined*
    /// check or they'd race the in-flight per-server handshakes and the
    /// first tool call would land inside the
    /// [`ClientState::Initializing`] window.
    ///
    /// Delegates to [`InitProgress::is_complete`]; see that doc for the
    /// full state machine.
    pub fn is_initialized(&self) -> bool {
        self.init_progress.is_complete()
    }

    /// Returns `true` whenever any initialization work is still
    /// outstanding: pre-`finish_init` OR at least one per-server
    /// handshake is still running in the background.
    ///
    /// Pairs with [`Self::is_initialized`]: during the window between
    /// the early [`Self::finish_init`] and the background task draining
    /// the per-server handshaking set, `is_initialized()` is still
    /// `false` (per-server work remains) AND `is_initializing()` is
    /// `true` (so wait-loops keep waiting instead of kicking off a
    /// second init).
    pub fn is_initializing(&self) -> bool {
        self.init_progress.is_in_progress()
    }

    /// Returns `true` once `finish_init` has fired, regardless of
    /// whether per-server background handshakes are still draining.
    /// Used for diagnostic logging where the caller wants to
    /// distinguish "pre-finish window" from "post-finish, bg work
    /// outstanding".
    pub fn has_finished_init(&self) -> bool {
        self.init_progress.has_finished_init()
    }

    /// Borrow the underlying [`InitProgress`] state machine, primarily
    /// for tests that want to assert against the discriminant directly.
    pub fn init_progress(&self) -> &InitProgress {
        &self.init_progress
    }

    /// Try to start initialization. Returns `true` if we transitioned
    /// from [`InitProgress::NotStarted`] to [`InitProgress::Starting`];
    /// returns `false` if init is already in progress or finished.
    pub fn try_start_init(&mut self) -> bool {
        self.init_progress.try_start()
    }

    /// Transition [`InitProgress::Starting`] → [`InitProgress::Finished`],
    /// preserving the per-server handshaking set. Called early (before
    /// per-server handshakes complete) so the session is unblocked for
    /// non-MCP work — `is_initialized()` still returns `false` until
    /// every handshake has reported via [`Self::mark_server_ready`].
    pub fn finish_init(&mut self) {
        self.init_progress.finish();
    }

    /// Cancel initialization back to [`InitProgress::NotStarted`].
    /// Used when generation changed during init (config change races
    /// with active init) and on full reset.
    pub fn cancel_init(&mut self) {
        self.init_progress.cancel();
    }

    /// Add server names to the handshaking set. Call after filtering
    /// `configs_to_start`, before spawning per-server tasks. Only
    /// meaningful in [`InitProgress::Starting`] / [`InitProgress::Finished`];
    /// logs a warning otherwise.
    pub fn mark_servers_initializing(&mut self, names: impl IntoIterator<Item = McpServerName>) {
        let names: Vec<McpServerName> = names.into_iter().collect();
        // A fresh init attempt clears any prior failure for these servers so
        // a server that recovers on retry stops showing as `Unavailable`.
        for name in &names {
            self.init_failed.remove(name);
        }
        self.init_progress.mark_handshaking(names);
    }

    /// Remove a server from the handshaking set (on success or failure
    /// of its handshake). Safe if not present.
    pub fn mark_server_ready(&mut self, name: &str) {
        self.init_progress.mark_handshake_complete(name);
    }

    /// Record a per-server background-init failure for status reporting,
    /// routing it to the correct set so the two stay disjoint.
    ///
    /// `needs_auth` failures are owned by the auth state machine: its recovery
    /// paths (`handle_mcp_auth_trigger`, `retry_auth_required_servers`)
    /// re-handshake and clear `auth_required`. Such servers must therefore NOT
    /// also land in `init_failed`, or a server that successfully authenticates
    /// would stay reported as `Unavailable` with zero tools. Every other
    /// failure (handshake / `tools/list` error or init timeout) goes to
    /// `init_failed` so the server surfaces as `Unavailable`.
    ///
    /// `detail` is a short cause stored for non-auth failures (the value in
    /// [`Self::init_failed`]); ignored for `needs_auth`.
    pub fn record_init_failure(&mut self, name: &str, needs_auth: bool, detail: Option<String>) {
        if needs_auth {
            self.auth_required.insert(name.to_string());
        } else {
            self.init_failed
                .insert(name.to_string(), detail.unwrap_or_default());
        }
    }

    /// Clear a prior init failure for `name` (symmetric with
    /// [`Self::record_init_failure`]). Used by the reactive managed re-auth
    /// path so a server that recovers is no longer reported as `Unavailable`
    /// with a stale non-auth `detail`.
    pub fn clear_init_failed(&mut self, name: &str) {
        self.init_failed.remove(name);
    }

    /// Clear the entire handshaking set in one shot. Used by the
    /// proxy-mode "init complete" path and the bg-handshake completion
    /// path as a defensive sweep after the per-server
    /// [`Self::mark_server_ready`] calls; cheap no-op if already empty.
    pub fn mark_all_servers_ready(&mut self) {
        self.init_progress.clear_handshaking();
    }

    /// True iff the named server's handshake is still in flight.
    /// Used by status snapshots and tool-dispatch gating that need to
    /// know per-server progress without cloning the whole set.
    pub fn is_server_handshaking(&self, name: &str) -> bool {
        self.init_progress.is_server_handshaking(name)
    }

    /// Iterate over server names whose background handshake is still
    /// in flight. Empty when init has not started or has fully
    /// completed.
    pub fn handshaking_servers_iter(&self) -> impl Iterator<Item = &McpServerName> {
        self.init_progress.handshaking_servers()
    }

    /// Snapshot of the handshaking set as a cloned `HashSet`. Used by
    /// the status snapshot API where the caller wants an owned copy
    /// that survives lock release.
    pub fn handshaking_servers_cloned(&self) -> std::collections::HashSet<McpServerName> {
        self.init_progress.handshaking_servers().cloned().collect()
    }

    /// Number of in-flight per-server handshakes.
    pub fn handshaking_servers_count(&self) -> usize {
        self.init_progress.handshaking_count()
    }

    /// Get current generation (for stale check after async init)
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Look up a client by server name.
    /// Owned clients take priority (they can override inherited ones).
    pub fn get_client(&self, name: &str) -> Option<&Arc<McpClient>> {
        self.owned_clients
            .get(name)
            .or_else(|| self.shared_clients.get(name))
    }

    /// Iterate over all clients (owned first, then shared — skipping shared
    /// entries whose name is overridden by an owned client).
    pub fn all_clients(&self) -> impl Iterator<Item = (&McpServerName, &Arc<McpClient>)> {
        self.owned_clients.iter().chain(
            self.shared_clients
                .iter()
                .filter(|(name, _)| !self.owned_clients.contains_key(name.as_str())),
        )
    }

    /// Import shared clients from a parent pool snapshot.
    /// Clients whose name collides with an agent-definition-owned server
    /// are skipped (the owned server takes priority).
    pub fn import_shared_clients(&mut self, pool: &SharedMcpPool) {
        let config_names: std::collections::HashSet<&str> =
            self.configs.iter().map(mcp_server_name).collect();
        for (name, client) in &pool.clients {
            if !config_names.contains(name.as_str()) {
                self.shared_clients.insert(name.clone(), Arc::clone(client));
            }
        }
    }
}

/// Snapshot of an MCP connection pool, taken at subagent spawn time.
///
/// The HashMap is cloned (cheap — values are `Arc<McpClient>`), so the
/// subagent's map is independent of the parent's. The `Arc<McpClient>`
/// entries are shared — both parent and child use the same transport.
/// This is intentionally snapshot-based, not live-updating.
#[derive(Clone)]
pub struct SharedMcpPool {
    clients: HashMap<McpServerName, Arc<McpClient>>,
    configs: Vec<acp::McpServer>,
    meta_config_map: McpMetaConfigMap,
}

impl SharedMcpPool {
    /// Create a snapshot from an existing `McpState`.
    /// Captures both owned and shared clients (deduped — owned wins).
    pub fn from_state(state: &McpState) -> Self {
        Self {
            clients: state
                .all_clients()
                .map(|(k, v)| (k.clone(), Arc::clone(v)))
                .collect(),
            configs: state.configs.clone(),
            meta_config_map: state.meta_config_map.clone(),
        }
    }

    pub fn get_client(&self, name: &str) -> Option<&Arc<McpClient>> {
        self.clients.get(name)
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    pub fn server_names(&self) -> impl Iterator<Item = &str> {
        self.clients.keys().map(String::as_str)
    }

    pub fn configs(&self) -> &[acp::McpServer] {
        &self.configs
    }

    pub fn meta_config_map(&self) -> &McpMetaConfigMap {
        &self.meta_config_map
    }

    /// Retain only clients whose name satisfies `predicate`.
    ///
    /// Only filters the `clients` map. `configs` and `meta_config_map` are
    /// left unchanged — callers that need config-level consistency should
    /// filter those separately. In the subagent inheritance path this is
    /// fine because `import_shared_clients` only iterates `clients`.
    pub fn retain_clients(&mut self, predicate: impl Fn(&str) -> bool) {
        self.clients.retain(|name, _| predicate(name));
    }
}

/// Compare two MCP server config lists for equality.
///
/// Since `acp::McpServer` may not implement PartialEq, we serialize to JSON and compare.
/// This is only called during config updates, so the overhead is acceptable.
pub(crate) fn mcp_servers_equal(a: &[acp::McpServer], b: &[acp::McpServer]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // Compare JSON serializations
    match (serde_json::to_string(a), serde_json::to_string(b)) {
        (Ok(a_json), Ok(b_json)) => a_json == b_json,
        _ => false, // If serialization fails, assume not equal
    }
}

/// Default timeout for an MCP server's `initialize` handshake & initial tool
/// listing, used when no override is supplied. 30s is generous enough that
/// cold-start `uvx` / `uv run --with` stdio servers that download deps on
/// first launch aren't killed mid-handshake. The shell resolves env / config /
/// requirements / remote overrides and injects them via `McpClientTimeoutOverrides`.
const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 30;

/// Default timeout for individual tool calls.
const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 6000;

/// How long a stdio server gets to exit after its transport closes before
/// its process group is killed.
const STDIO_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// Timeout for OAuth metadata discovery when building an HTTP transport.
/// Bounds transport setup for servers without OAuth support.
const OAUTH_DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Budget for the anonymous-access tie-break request after an inconclusive OAuth probe.
const ANONYMOUS_ACCESS_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Per-MCP-server config overrides from `_meta.mcpConfig` in session/new or session/load.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerMetaConfig {
    /// Init handshake timeout in ms. Overrides config.toml `startup_timeout_sec`.
    #[serde(default)]
    pub startup_timeout_ms: Option<u64>,
    /// Per-tool-call timeout in ms. Overrides config.toml `tool_timeout_sec`.
    #[serde(default)]
    pub tool_timeout_ms: Option<u64>,
    /// Per-tool timeout overrides in ms: `{ "create_issue": 120000, "search": 30000 }`.
    /// Overrides config.toml `tool_timeouts` (and `tool_timeout_sec`) for matching tools.
    #[serde(default)]
    pub tool_timeouts_ms: Option<HashMap<ToolName, u64>>,
    /// Also keep the raw base64 in tool-result text (in addition to the
    /// vision-token rendering) so the agent can decode + forward it via
    /// path-based tools like `send_file`. Costs ~2× tokens per image.
    /// Default `false`. See [`format_mcp_image`].
    #[serde(default)]
    pub expose_image_base64: Option<bool>,
}

/// MCP server name → per-server config overrides from `_meta.mcpConfig`.
pub type McpMetaConfigMap = HashMap<McpServerName, McpServerMetaConfig>;

/// Parse `mcpConfig` from a session request's `_meta`. Empty map if absent/invalid.
pub fn parse_mcp_meta_config(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> McpMetaConfigMap {
    meta.and_then(|m| m.get("mcpConfig"))
        .and_then(|v| serde_json::from_value::<McpMetaConfigMap>(v.clone()).ok())
        .unwrap_or_default()
}

/// MCP initialization strategy. Defined in `echo-agent-telemetry`; re-exported
/// here so existing call sites continue to work.
pub use echo_agent_telemetry::enums::McpInitStrategy;

/// Parse a non-empty `server__tool` ID with one overlap-aware delimiter and
/// valid [`echo_agent_tool_protocol::ToolId`] syntax.
pub fn parse_mcp_qualified_name(
    name: &str,
) -> Option<(echo_agent_tool_protocol::ToolId, &str, &str)> {
    let delimiter = MCP_TOOL_NAME_DELIMITER.as_bytes();
    // Byte windows preserve both overlapping `__` boundaries in `___`.
    let mut boundaries = name
        .as_bytes()
        .windows(delimiter.len())
        .enumerate()
        .filter_map(|(index, window)| (window == delimiter).then_some(index));
    let boundary = boundaries.next()?;
    if boundaries.next().is_some() {
        return None;
    }
    let (server, tool_with_delimiter) = name.split_at(boundary);
    let tool = &tool_with_delimiter[MCP_TOOL_NAME_DELIMITER.len()..];
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((
        echo_agent_tool_protocol::ToolId::new(name).ok()?,
        server,
        tool,
    ))
}

/// Parse an MCP tool name in `server__tool` format into owned segments.
pub fn parse_mcp_tool_name(name: &str) -> Option<(String, String)> {
    parse_mcp_qualified_name(name).map(|(_, server, tool)| (server.to_owned(), tool.to_owned()))
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("MCP client error: {0}")]
    ClientError(String),

    #[error("MCP server '{server}' timed out after {timeout_secs}s")]
    Timeout { server: String, timeout_secs: u64 },

    #[error("Failed to spawn MCP server '{server}': {source}")]
    SpawnFailed {
        server: String,
        source: std::io::Error,
    },

    #[error("MCP server '{server}' handshake failed: {source}")]
    HandshakeFailed {
        server: String,
        source: Box<ClientInitializeError>,
    },

    /// Pre-spawn gate: server needs OAuth but this session cannot complete interactive auth.
    #[error(
        "MCP server '{server}': Auth required (non-interactive session; authenticate in TUI or set Authorization header)"
    )]
    AuthRequired { server: String },

    #[error("MCP service error: {0}")]
    ServiceError(#[from] ServiceError),
}

impl McpError {
    fn timeout(server: &str, duration: std::time::Duration) -> Self {
        Self::Timeout {
            server: server.to_string(),
            timeout_secs: duration.as_secs(),
        }
    }

    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout { .. })
    }

    pub fn error_category(&self) -> echo_agent_session_events::McpErrorCategory {
        use echo_agent_session_events::McpErrorCategory;
        match self {
            Self::SpawnFailed { .. } => McpErrorCategory::SpawnFailed,
            Self::Timeout { .. } => McpErrorCategory::Timeout,
            Self::HandshakeFailed { .. } => McpErrorCategory::HandshakeFailed,
            Self::AuthRequired { .. } => McpErrorCategory::AuthRequired,
            Self::ClientError(_) | Self::ServiceError(_) => McpErrorCategory::ClientError,
        }
    }

    pub fn server_name(&self) -> Option<&str> {
        match self {
            Self::SpawnFailed { server, .. }
            | Self::Timeout { server, .. }
            | Self::HandshakeFailed { server, .. }
            | Self::AuthRequired { server } => Some(server),
            Self::ClientError(_) | Self::ServiceError(_) => None,
        }
    }

    /// True if this error indicates the server rejected us for auth reasons (a
    /// credential re-fetch could help). Timeout/spawn failures can't be cured by
    /// re-fetching credentials, so they're never auth.
    pub fn is_auth_rejection(&self) -> bool {
        match self {
            Self::AuthRequired { .. } => true,
            Self::HandshakeFailed { source, .. } => is_auth_rejection_message(&source.to_string()),
            Self::ServiceError(e) => is_auth_rejection_message(&e.to_string()),
            Self::ClientError(s) => is_auth_rejection_message(s),
            Self::Timeout { .. } | Self::SpawnFailed { .. } => false,
        }
    }
}

/// True when a failed refresh-token grant was a **network-level** failure
/// that never reached the IdP (RT validity unknown, presumed good); IdP
/// rejections and missing credentials stay terminal (escalate to browser).
/// rmcp 2.1 collapses the error into `TokenRefreshFailed(String)`, so this
/// anchors on the `oauth2` crate's stable `Display` texts via
/// `starts_with` (an IdP error description can't spoof a match):
/// `"Request failed"` = network, `"Failed to parse server response"` =
/// non-OAuth 5xx/proxy bodies; `"Server returned error response: …"` does
/// NOT match.
pub(crate) fn mcp_refresh_failure_is_transient(err: &rmcp::transport::auth::AuthError) -> bool {
    match err {
        rmcp::transport::auth::AuthError::TokenRefreshFailed(msg) => {
            msg.starts_with("Request failed") || msg.starts_with("Failed to parse server response")
        }
        _ => false,
    }
}

/// True if an MCP error *message* indicates an auth rejection (vs. a transport
/// drop, timeout, or protocol error), so host recovery can decide whether a
/// credential re-fetch would help.
///
/// Matches auth wording and context-anchored 401 patterns only, so a bare digit
/// ("took 401ms", ports) can't trip it. Excludes 403/forbidden — a non-auth
/// policy denial here, not a credential problem.
pub fn is_auth_rejection_message(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    // Auth wording has no numeric component, so plain substrings are safe.
    if l.contains("auth required")
        || l.contains("authorizationrequired")
        || l.contains("authrequired")
        || l.contains("authentication")
        || l.contains("unauthorized")
    {
        return true;
    }
    // Require a non-alphanumeric (or end) after "401" so "http 401" matches but
    // "http 4012" (other status) and "http 401ms" (a duration) do not.
    [
        "status: 401",
        "status code 401",
        "http status 401",
        "http 401",
        "error 401",
    ]
    .iter()
    .any(|token| token_at_word_boundary(&l, token))
}

/// Whether `haystack` contains `token` at a right word boundary, so a
/// digit-terminated token (`...401`) doesn't match a longer run (`4012`) or an
/// adjacent unit (`401ms`).
///
/// Invariant: `token` must be ASCII (all callers pass ASCII literals). A
/// non-ASCII token could advance `from` mid-UTF-8 and panic on the next slice.
fn token_at_word_boundary(haystack: &str, token: &str) -> bool {
    debug_assert!(
        token.is_ascii(),
        "token_at_word_boundary requires an ASCII token"
    );
    let mut from = 0;
    while let Some(idx) = haystack[from..].find(token) {
        let end = from + idx + token.len();
        if !haystack[end..].starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return true;
        }
        from += idx + 1;
    }
    false
}

#[derive(Clone)]
pub struct McpTool {
    name: String,
    description: String,
    server_name: String,
    mcp_state: Arc<Mutex<McpState>>,
    schema: serde_json::Value,
    meta: Option<serde_json::Value>,
}

/// Data needed to register an MCP tool via `register_erased()`.
///
/// MCP tools have two visibility audiences controlled by `_meta.ui.visibility`:
///
/// - **Model-visible** (default, or `["model", "app"]`): registered in `ToolBridge`
///   so the LLM can invoke them during a conversation.
/// - **App-visible only** (`["app"]`): not registered in `ToolBridge`, so the LLM
///   never sees them. These are UI-only actions (e.g. refresh buttons) surfaced to
///   the frontend via `echo.agent/mcp/tools_changed` notifications and callable via
///   `echo.agent/mcp/call`.
pub struct McpToolRegistration {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub tool: McpErasedTool,
    pub meta: Option<serde_json::Value>,
    pub icons: Vec<McpIcon>,
    pub model_visible: bool,
}

impl McpTool {
    /// Reconstruct an `McpTool` from its constituent parts. Used when stashing
    /// a disabled tool at runtime so it can be re-enabled without a full re-init.
    pub fn new(
        name: String,
        description: String,
        server_name: String,
        mcp_state: Arc<Mutex<McpState>>,
        schema: serde_json::Value,
        meta: Option<serde_json::Value>,
    ) -> Self {
        Self {
            name,
            description,
            server_name,
            mcp_state,
            schema,
            meta,
        }
    }

    /// Convert into the data needed for `ToolBridge::register_erased()`.
    ///
    /// Invalid or ambiguous qualified IDs and provider-invalid names are logged
    /// and skipped; the upstream connector must provide non-empty `server` and
    /// `tool` segments separated by exactly one `__` boundary.
    pub fn into_registration(self) -> Option<McpToolRegistration> {
        let qualified_name = format!(
            "{}{}{}",
            self.server_name, MCP_TOOL_NAME_DELIMITER, self.name
        );

        if parse_mcp_qualified_name(&qualified_name).is_none() {
            tracing::error!(
                server = %self.server_name,
                tool = %self.name,
                qualified = %qualified_name,
                "Skipping MCP tool with invalid or ambiguous qualified name"
            );
            return None;
        }
        if let Err(reason) = validate_tool_name(&qualified_name) {
            tracing::error!(
                tool_name = %qualified_name,
                server = %self.server_name,
                reason = %reason,
                "Skipping MCP tool with invalid name"
            );
            return None;
        }

        let description = self.description.clone();
        let input_schema = self.schema.clone();
        let meta = self.meta.clone();

        let model_visible = meta
            .as_ref()
            .and_then(|m| m.get("ui"))
            .and_then(|ui| ui.get("visibility"))
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().any(|s| s.as_str() == Some("model")))
            .unwrap_or(true); // default: visible to model

        Some(McpToolRegistration {
            name: qualified_name,
            description,
            input_schema,
            tool: McpErasedTool { tool: self },
            meta,
            icons: Vec::new(),
            model_visible,
        })
    }
}

/// MCP tool wrapper for runtime dispatch.
///
/// MCP tools are already untyped (JSON → JSON), so they implement
/// `echo_agent_tool_runtime::Tool` directly instead of going through typed wrappers.
pub struct McpErasedTool {
    tool: McpTool,
}

impl std::fmt::Debug for McpErasedTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpErasedTool")
            .field("name", &self.tool.name)
            .field("server", &self.tool.server_name)
            .finish()
    }
}

impl ToolMetadata for McpErasedTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::MCP
    }

    fn description_template(&self) -> &str {
        &self.tool.description
    }
}

impl echo_agent_tool_runtime::Tool for McpErasedTool {
    type Args = serde_json::Value;
    type Output = ToolOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        // Use the qualified name (server__tool) so that two MCP servers
        // exposing the same raw tool name get distinct LocalRegistry entries.
        let qualified = format!(
            "{}{}{}",
            self.tool.server_name, MCP_TOOL_NAME_DELIMITER, self.tool.name
        );
        echo_agent_tool_protocol::ToolId::new(&qualified)
            .unwrap_or_else(|_| echo_agent_tool_protocol::ToolId::new("mcp_tool").expect("valid"))
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(&self.tool.name, &self.tool.description)
    }

    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        raw: serde_json::Value,
    ) -> Result<ToolOutput, echo_agent_tool_runtime::ToolError> {
        let no_replay =
            if let Ok(resources) = echo_agent_tools::types::tool_metadata::shared_resources(&ctx) {
                resources
                    .lock()
                    .await
                    .get::<echo_agent_tools::types::resources::DesktopMcpNoReplay>()
                    .is_some_and(|policy| policy.0)
            } else {
                false
            };
        let mcp_call_start = std::time::Instant::now();
        let (client, event_writer) = {
            let state = self.tool.mcp_state.lock().await;
            let c = Arc::clone(state.get_client(&self.tool.server_name).ok_or_else(|| {
                echo_agent_tool_runtime::ToolError::custom(
                    "process_manager",
                    format!("MCP server '{}' not found", self.tool.server_name),
                )
            })?);
            (c, state.event_writer().clone())
        };

        let server = &self.tool.server_name;
        let tool = &self.tool.name;
        let tool_timeout = client.tool_timeout_for(tool);
        let qualified_name = format!("{}{}{}", server, MCP_TOOL_NAME_DELIMITER, tool);
        event_writer.emit(echo_agent_session_events::Event::McpToolCallStarted {
            server_name: server.clone(),
            tool_name: tool.clone(),
            call_id: qualified_name.clone(),
            timeout_sec: tool_timeout,
        });

        let mut auth_retry_attempted = false;
        let mut reconnect_attempted = false;
        let mut is_timeout = false;
        let ew = &event_writer;
        let dispatch_result = match self
            .try_call_tool_with_replay_policy(
                &client,
                &raw,
                &mut reconnect_attempted,
                &mut is_timeout,
                ew,
                no_replay,
            )
            .await
        {
            Ok(result) => Ok(result),
            Err(first_err) if client.has_auth() && !no_replay => {
                auth_retry_attempted = true;
                let reauth_ok = client.force_reauth(false).await;
                ew.emit(echo_agent_session_events::Event::McpAuthRetry {
                    server_name: server.clone(),
                    trigger: "tool_call_failed".to_string(),
                    success: reauth_ok,
                });
                if reauth_ok {
                    self.try_call_tool_with_replay_policy(
                        &client,
                        &raw,
                        &mut reconnect_attempted,
                        &mut is_timeout,
                        ew,
                        no_replay,
                    )
                    .await
                    .map_err(|e| {
                        echo_agent_tool_runtime::ToolError::custom("process_manager", e.to_string())
                    })
                } else {
                    Err(first_err)
                }
            }
            Err(e) => Err(e),
        };

        let call_result = match dispatch_result {
            Ok(result) => result,
            Err(e) => {
                ew.emit(echo_agent_session_events::Event::McpToolCallCompleted {
                    server_name: server.clone(),
                    tool_name: tool.clone(),
                    call_id: qualified_name,
                    duration_ms: mcp_call_start.elapsed().as_millis() as u64,
                    success: false,
                    is_timeout,
                    error: Some(e.to_string()),
                    reconnect_attempted,
                    auth_retry_attempted,
                });
                return Err(e);
            }
        };

        let is_error = call_result.is_error.unwrap_or(false);
        let mut output = if is_error {
            let error_msg = call_result
                .content
                .iter()
                .filter_map(|c| match c {
                    rmcp::model::ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            ToolOutput::MCP(MCPOutput::errored(tool.clone(), server.clone(), error_msg))
        } else {
            let expose_base64 = client.expose_image_base64();
            let parts: Vec<String> = call_result
                .content
                .into_iter()
                .filter_map(|c| match c {
                    rmcp::model::ContentBlock::Text(t) => Some(t.text),
                    rmcp::model::ContentBlock::Image(img) => {
                        Some(format_mcp_image(&img.mime_type, &img.data, expose_base64))
                    }
                    rmcp::model::ContentBlock::Resource(r) => match &r.resource {
                        rmcp::model::ResourceContents::BlobResourceContents {
                            mime_type,
                            blob,
                            ..
                        } if mime_type
                            .as_deref()
                            .is_some_and(|m| m.starts_with("image/")) =>
                        {
                            let mime = mime_type.as_deref().unwrap();
                            Some(format_mcp_image(mime, blob, expose_base64))
                        }
                        _ => serde_json::to_string(&r).ok(),
                    },
                    _ => None,
                })
                .collect();
            let text = parts.join("\n");
            ToolOutput::MCP(MCPOutput::okay_output(tool.clone(), server.clone(), text))
        };

        if let ToolOutput::MCP(ref mut mcp_out) = output {
            mcp_out.auth_retry_attempted = auth_retry_attempted;
            mcp_out.reconnect_attempted = reconnect_attempted;
            mcp_out.is_timeout = is_timeout;
        }

        let success = !is_error;
        let duration_ms = mcp_call_start.elapsed().as_millis() as u64;
        let error_text = if is_error {
            match &output {
                ToolOutput::MCP(mcp) => match mcp.output() {
                    MCPOutputDetails::Error(e) => Some(e.clone()),
                    _ => None,
                },
                _ => None,
            }
        } else {
            None
        };
        event_writer.emit(echo_agent_session_events::Event::McpToolCallCompleted {
            server_name: server.clone(),
            tool_name: tool.clone(),
            call_id: qualified_name.clone(),
            duration_ms,
            success,
            is_timeout,
            error: error_text,
            reconnect_attempted,
            auth_retry_attempted,
        });
        echo_agent_telemetry::session_ctx::log_event(echo_agent_telemetry::events::McpToolCalled {
            server_name: server.clone(),
            tool_name: tool.clone(),
            qualified_name,
            success,
            duration_ms,
        });
        Ok(output)
    }
}

/// Render an MCP image content block. The data URI is consumed by the
/// session-layer `extract_base64_images` and rendered as vision tokens.
/// When `expose_base64`, also emit a `<mcp_image_base64>` wrapper that
/// survives extraction (wrapper has no `data:image/` prefix → regex skips
/// it), exposing the raw bytes to the agent for path-based forwarding.
fn format_mcp_image(mime: &str, base64_data: &str, expose_base64: bool) -> String {
    if expose_base64 {
        format!(
            "data:{mime};base64,{base64_data}\n\
             <mcp_image_base64 mime=\"{mime}\">\n\
             {base64_data}\n\
             </mcp_image_base64>"
        )
    } else {
        format!("data:{mime};base64,{base64_data}")
    }
}

/// Check whether a `ServiceError` indicates the underlying transport has died
/// and a fresh connection could recover it.
fn is_retriable_transport_error(err: &ServiceError) -> bool {
    matches!(
        err,
        ServiceError::TransportClosed | ServiceError::TransportSend(_)
    )
}

/// Recover for every JSON-RPC code except the deterministic client set
/// {-32700, -32600, -32601, -32602} (those mean the request was wrong, not the session).
fn should_recover_mcp_error(code: i32) -> bool {
    use rmcp::model::ErrorCode;
    let deterministic_client_error = code == ErrorCode::PARSE_ERROR.0
        || code == ErrorCode::INVALID_REQUEST.0
        || code == ErrorCode::METHOD_NOT_FOUND.0
        || code == ErrorCode::INVALID_PARAMS.0;
    !deterministic_client_error
}

/// Recovers transport errors, and an HTTP `McpError` once per dispatch except
/// deterministic client codes and auth-class errors — a rebuild reuses stale
/// creds, so auth is routed to the re-auth paths instead.
fn should_recover_service_error(
    err: &ServiceError,
    is_http: bool,
    reconnect_attempted: bool,
) -> bool {
    is_retriable_transport_error(err)
        || matches!(
            err,
            ServiceError::McpError(e)
                if is_http
                    && !reconnect_attempted
                    && should_recover_mcp_error(e.code.0)
                    && !is_auth_rejection_message(e.message.as_ref())
        )
}

impl McpErasedTool {
    #[cfg(test)]
    async fn try_call_tool(
        &self,
        client: &Arc<McpClient>,
        raw: &serde_json::Value,
        reconnect_attempted: &mut bool,
        is_timeout: &mut bool,
        ew: &echo_agent_session_events::EventWriter,
    ) -> Result<rmcp::model::CallToolResult, echo_agent_tool_runtime::ToolError> {
        self.try_call_tool_with_replay_policy(
            client,
            raw,
            reconnect_attempted,
            is_timeout,
            ew,
            false,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn try_call_tool_with_replay_policy(
        &self,
        client: &Arc<McpClient>,
        raw: &serde_json::Value,
        reconnect_attempted: &mut bool,
        is_timeout: &mut bool,
        ew: &echo_agent_session_events::EventWriter,
        no_replay: bool,
    ) -> Result<rmcp::model::CallToolResult, echo_agent_tool_runtime::ToolError> {
        let mcp_service = client.ensure_initialized().await.map_err(|e| {
            echo_agent_tool_runtime::ToolError::custom("process_manager", e.to_string())
        })?;
        let tool_timeout = client.tool_timeout_for(&self.tool.name);
        let timeout_duration = std::time::Duration::from_secs(tool_timeout);
        let mut params = CallToolRequestParams::new(self.tool.name.clone());
        params.arguments = raw.as_object().cloned();

        let result =
            tokio::time::timeout(timeout_duration, mcp_service.call_tool(params.clone())).await;

        match result {
            Ok(Ok(call_result)) => Ok(call_result),
            // A JSON-RPC error is an actual response from this tools/call.
            // Coding must not replay it, and its native lease may settle as
            // a failed terminal. Transport loss and timeout remain uncertain.
            Ok(Err(ServiceError::McpError(error))) if no_replay => {
                Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::ContentBlock::text(error.to_string()),
                ]))
            }
            Ok(Err(service_err))
                if !no_replay
                    && should_recover_service_error(
                        &service_err,
                        client.is_http(),
                        *reconnect_attempted,
                    ) =>
            {
                self.recover_and_retry(
                    client,
                    params,
                    timeout_duration,
                    tool_timeout,
                    service_err,
                    reconnect_attempted,
                    is_timeout,
                    ew,
                )
                .await
            }
            Ok(Err(e)) => Err(echo_agent_tool_runtime::ToolError::custom(
                "process_manager",
                e.to_string(),
            )),
            Err(_) => {
                *is_timeout = true;
                // Reset for the next call but don't retry — a slow side-effecting tool must not run twice.
                if client.is_http() && !*reconnect_attempted {
                    client.reset_transport().await;
                    *reconnect_attempted = true;
                }
                Err(echo_agent_tool_runtime::ToolError::custom(
                    "process_manager",
                    format!(
                        "MCP tool '{}' timed out after {} seconds",
                        self.tool.name, tool_timeout
                    ),
                ))
            }
        }
    }

    /// On `recover()` failure surface the original error, else the retry error
    /// (preserves the auth signal managed re-auth reads from the string).
    #[allow(clippy::too_many_arguments)]
    async fn recover_and_retry(
        &self,
        client: &Arc<McpClient>,
        params: CallToolRequestParams,
        timeout_duration: std::time::Duration,
        tool_timeout: u64,
        original_err: ServiceError,
        reconnect_attempted: &mut bool,
        is_timeout: &mut bool,
        ew: &echo_agent_session_events::EventWriter,
    ) -> Result<rmcp::model::CallToolResult, echo_agent_tool_runtime::ToolError> {
        *reconnect_attempted = true;
        tracing::warn!(
            server = self.tool.server_name.as_str(),
            tool = self.tool.name.as_str(),
            error = %original_err,
            "MCP transport error, attempting reconnect"
        );
        ew.emit(echo_agent_session_events::Event::McpTransportError {
            server_name: self.tool.server_name.clone(),
            tool_name: self.tool.name.clone(),
            error: original_err.to_string(),
        });
        let mcp_service = match client.recover().await {
            Ok(service) => {
                ew.emit(echo_agent_session_events::Event::McpTransportReconnect {
                    server_name: self.tool.server_name.clone(),
                    success: true,
                    error: None,
                });
                service
            }
            Err(e) => {
                ew.emit(echo_agent_session_events::Event::McpTransportReconnect {
                    server_name: self.tool.server_name.clone(),
                    success: false,
                    error: Some(e.to_string()),
                });
                return Err(echo_agent_tool_runtime::ToolError::custom(
                    "process_manager",
                    original_err.to_string(),
                ));
            }
        };
        match tokio::time::timeout(timeout_duration, mcp_service.call_tool(params)).await {
            Ok(Ok(call_result)) => Ok(call_result),
            Ok(Err(retry_err)) => Err(echo_agent_tool_runtime::ToolError::custom(
                "process_manager",
                retry_err.to_string(),
            )),
            Err(_) => {
                *is_timeout = true;
                Err(echo_agent_tool_runtime::ToolError::custom(
                    "process_manager",
                    format!(
                        "MCP tool '{}' timed out after {} seconds",
                        self.tool.name, tool_timeout
                    ),
                ))
            }
        }
    }
}

/// Whether this session can complete an interactive (browser) OAuth flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OauthInteractivity {
    Interactive,
    NonInteractive,
}

impl OauthInteractivity {
    /// Headless/SDK sessions set `non_interactive = true` and cannot complete browser OAuth.
    pub fn from_non_interactive(non_interactive: bool) -> Self {
        if non_interactive {
            Self::NonInteractive
        } else {
            Self::Interactive
        }
    }
}

/// Outcome of probing whether an HTTP/SSE MCP server needs OAuth and whether
/// we have credentials usable without an interactive browser flow.
enum HttpOauthPrep {
    /// Server does not advertise OAuth (or discovery failed conservatively).
    NoOauthSupport,
    /// Ready to connect with an auth manager (stored token works, or interactive deferred auth).
    ManagerReady(Arc<tokio::sync::Mutex<rmcp::transport::auth::AuthorizationManager>>),
    /// OAuth is required but cannot complete in non-interactive mode — do not start unauthenticated.
    NeedsInteractiveLogin,
}

/// Result of the proactive OAuth probe. `Inconclusive` (probe error or timeout in non-interactive
/// mode) must be settled by the anonymous-access tie-break before a connection decision exists,
/// which [`resolve_http_oauth_prep`] enforces by type.
enum OauthProbeOutcome {
    Resolved(HttpOauthPrep),
    Inconclusive,
}

impl OauthProbeOutcome {
    /// Inconclusive probe: interactive proceeds as plain HTTP; non-interactive defers
    /// to the anonymous-access tie-break instead of failing closed outright.
    fn on_probe_failure(mode: OauthInteractivity) -> Self {
        match mode {
            OauthInteractivity::Interactive => Self::Resolved(HttpOauthPrep::NoOauthSupport),
            OauthInteractivity::NonInteractive => Self::Inconclusive,
        }
    }
}

/// Proactive OAuth discovery per RFC 8414 + 9728.
///
/// Creates an `AuthorizationManager` with our `CredentialStoreAdapter`,
/// discovers server metadata, and loads stored tokens if available.
///
/// With no stored tokens but server OAuth support, behavior splits on `mode`:
/// `Interactive` spawns the browser flow in the background (non-blocking; the
/// first tool call picks up tokens via `force_reauth` → `initialize_from_store`
/// once the user consents), while `NonInteractive` fails closed
/// (`NeedsInteractiveLogin`) rather than start an unauthenticated worker that
/// fatals with `Auth(AuthorizationRequired)` on stderr while the prompt still
/// succeeds.
///
/// Known gap: rmcp `get_access_token` returns stored tokens as-is when expiry
/// metadata is absent, so an expiry-less revoked token can still reach
/// `ManagerReady`.
async fn discover_and_prepare_auth(
    server_name: &str,
    server_url: &str,
    mode: OauthInteractivity,
) -> OauthProbeOutcome {
    let Ok(parsed_url) = url::Url::parse(server_url) else {
        return OauthProbeOutcome::Resolved(HttpOauthPrep::NoOauthSupport);
    };
    let adapter =
        crate::credentials::McpCredentialStoreAdapter::new(server_name.to_string(), parsed_url);

    let mut manager = match rmcp::transport::auth::AuthorizationManager::new(server_url).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(server = server_name, %e, "Failed to create OAuth manager");
            return OauthProbeOutcome::on_probe_failure(mode);
        }
    };
    manager.set_credential_store(adapter);

    if let Ok(true) = manager.initialize_from_store().await {
        // Stored creds may be expired/unrefreshable; in non-interactive mode that
        // still yields rmcp worker fatal AuthorizationRequired on stderr. This probe
        // shares the caller's 5s discovery timeout budget.
        if mode == OauthInteractivity::NonInteractive
            && let Err(e) = manager.get_access_token().await
        {
            tracing::warn!(
                server = server_name,
                error = %e,
                "Skipping OAuth MCP in non-interactive mode (stored credentials unusable); re-authenticate in TUI"
            );
            return OauthProbeOutcome::Resolved(HttpOauthPrep::NeedsInteractiveLogin);
        }
        tracing::info!(server = server_name, "Loaded stored OAuth credentials");
        return OauthProbeOutcome::Resolved(HttpOauthPrep::ManagerReady(Arc::new(
            tokio::sync::Mutex::new(manager),
        )));
    }

    match manager.discover_metadata().await {
        Ok(metadata) => {
            manager.set_metadata(metadata);
            if mode == OauthInteractivity::NonInteractive {
                tracing::warn!(
                    server = server_name,
                    "Skipping OAuth MCP in non-interactive mode (no stored tokens); authenticate in TUI or set an Authorization header"
                );
                return OauthProbeOutcome::Resolved(HttpOauthPrep::NeedsInteractiveLogin);
            }
            tracing::info!(
                server = server_name,
                "Server supports OAuth but has no stored tokens"
            );
            OauthProbeOutcome::Resolved(HttpOauthPrep::ManagerReady(Arc::new(
                tokio::sync::Mutex::new(manager),
            )))
        }
        Err(rmcp::transport::auth::AuthError::NoAuthorizationSupport) => {
            tracing::debug!(server = server_name, "Server does not support OAuth");
            OauthProbeOutcome::Resolved(HttpOauthPrep::NoOauthSupport)
        }
        Err(e) => {
            tracing::warn!(server = server_name, %e, "OAuth discovery failed");
            OauthProbeOutcome::on_probe_failure(mode)
        }
    }
}

/// Whether an MCP server answers a request that carries no credentials.
enum AnonymousAccess {
    Accepted,
    AuthChallenged,
    Unreachable,
}

/// One POST to the MCP endpoint, judged by status class only. Not a GET, because streamable-http
/// servers legally answer GET with a never-ending SSE stream, which is what hangs discovery.
async fn probe_anonymous_access(
    server_name: &str,
    url: &str,
    headers: &[(String, String)],
) -> AnonymousAccess {
    // Redirects are not followed: a gateway that redirects an anonymous POST to a
    // login page is challenging, not accepting.
    // reqwest 0.13; the policy chokepoint is typed for 0.12 and cannot wrap this builder.
    #[allow(clippy::disallowed_methods)]
    let client = match with_extra_root_certificates(reqwest::Client::builder())
        .timeout(ANONYMOUS_ACCESS_PROBE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::warn!(server = server_name, error = %e, "anonymous-access probe: building HTTP client failed");
            return AnonymousAccess::Unreachable;
        }
    };
    // The real transport sends the configured headers (e.g. `X-Api-Key`); the probe
    // must too, or header-authenticated servers would fail closed. Authorization is
    // known absent on this path.
    let mut probe_headers = parse_config_headers(
        server_name,
        "anonymous-probe",
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    // Protocol-required values win over configured ones.
    probe_headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    probe_headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json, text/event-stream"),
    );
    apply_user_agent_policy(&mut probe_headers, server_name, url);
    let request = client.post(url).headers(probe_headers).body("{}");
    match request.send().await {
        Ok(response) => {
            let status = response.status();
            // 407 signals proxy credentials rather than server auth, but those are still
            // credentials this session cannot supply headlessly.
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
                || status == reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED
                || status.is_redirection()
            {
                AnonymousAccess::AuthChallenged
            } else if status.is_server_error() {
                // Often an ingress/proxy blip that says nothing about auth; fail closed.
                AnonymousAccess::Unreachable
            } else {
                // Any other response (even a 4xx complaint about the `{}` body) proves
                // the server answers unauthenticated requests.
                AnonymousAccess::Accepted
            }
        }
        Err(e) => {
            tracing::warn!(server = server_name, error = %e, "anonymous-access probe failed");
            AnonymousAccess::Unreachable
        }
    }
}

/// OAuth discovery plus the anonymous-access tie-break for inconclusive results, so
/// tokenless servers connect headless while auth-challenging (or unreachable) servers
/// keep failing closed.
async fn resolve_http_oauth_prep(
    server_name: &str,
    url: &str,
    headers: &[(String, String)],
    ctx: &McpSpawnCtx<'_>,
    discovery_timeout: std::time::Duration,
) -> HttpOauthPrep {
    let outcome = {
        let _auth_discovery_timer =
            echo_agent_telemetry::instrumentation::timer("mcp_http_auth_discovery");
        match tokio::time::timeout(
            discovery_timeout,
            discover_and_prepare_auth(server_name, url, ctx.mode),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    server = server_name,
                    url = %url,
                    mode = ?ctx.mode,
                    timeout_secs = discovery_timeout.as_secs(),
                    "OAuth discovery timed out"
                );
                ctx.event_writer
                    .emit(echo_agent_session_events::Event::McpOAuthDiscoveryTimeout {
                        server_name: server_name.to_string(),
                        url: url.to_string(),
                    });
                OauthProbeOutcome::on_probe_failure(ctx.mode)
            }
        }
    };
    match outcome {
        OauthProbeOutcome::Resolved(prep) => prep,
        OauthProbeOutcome::Inconclusive => {
            let (verdict, prep) = match probe_anonymous_access(server_name, url, headers).await {
                AnonymousAccess::Accepted => {
                    tracing::info!(
                        server = server_name,
                        "OAuth discovery was inconclusive but the server accepts unauthenticated requests; connecting without auth"
                    );
                    ("accepted", HttpOauthPrep::NoOauthSupport)
                }
                AnonymousAccess::AuthChallenged => {
                    tracing::warn!(
                        server = server_name,
                        "OAuth discovery was inconclusive and the server challenges unauthenticated requests; authenticate in TUI or set an Authorization header"
                    );
                    ("auth_challenged", HttpOauthPrep::NeedsInteractiveLogin)
                }
                AnonymousAccess::Unreachable => {
                    tracing::warn!(
                        server = server_name,
                        "OAuth discovery was inconclusive and the anonymous-access probe could not reach the server; failing closed as auth required"
                    );
                    ("unreachable", HttpOauthPrep::NeedsInteractiveLogin)
                }
            };
            ctx.event_writer
                .emit(echo_agent_session_events::Event::McpOAuthProbeResolved {
                    server_name: server_name.to_string(),
                    verdict: verdict.to_string(),
                });
            prep
        }
    }
}

/// Configuration for HTTP MCP server connection.
#[derive(Clone)]
pub struct HttpConfig {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// Newline-delimited JSON-RPC stdio transport whose read side survives a
/// single undecodable line.
///
/// Used instead of rmcp's `AsyncRwTransport` for two reasons:
/// - **Wire silence:** a bad line is skipped without replying, whereas rmcp
///   answers shape-mismatched JSON with a -32600 error — a reply an off-spec
///   server could echo back as more invalid input.
/// - **Telemetry:** each skip emits an `McpTransportDecodeError` event (with a
///   truncated sample of the offending line) so the failure is visible in the
///   session trace — rmcp's own tracing is not captured there.
///
/// We read lines ourselves (rather than via `FramedRead` + rmcp's codec) so
/// reading continues after a bad line; only a genuine end-of-stream returns
/// `None`. A stray non-JSON stdout line, a JSON-RPC batch array, or an
/// off-spec response therefore never collapses the transport ("Transport
/// closed" failing every in-flight request — the "connector shows but doesn't
/// work" report).
///
/// Generic over `R`/`W` so it can be unit-tested with in-memory pipes; the
/// production transport binds `ChildStdout`/`ChildStdin`.
struct ResilientRwTransport<R, W>
where
    R: AsyncRead,
    W: AsyncWrite,
{
    read: BufReader<R>,
    /// `Arc<Mutex<Option<…>>>` so `send` can return a `Send + 'static` future
    /// (the `Transport` contract) without borrowing `self`, and so `close` can
    /// drop the writer — mirrors rmcp's own `AsyncRwTransport`.
    write: Arc<Mutex<Option<W>>>,
    server_name: String,
    event_writer: echo_agent_session_events::EventWriter,
}

/// Max bytes of an offending line copied into the decode-error event.
const DECODE_ERROR_SAMPLE_LEN: usize = 200;

/// A line that failed to deserialize but is a JSON *notification* (an object
/// with a `method` and no `id`) is benign — many servers emit non-MCP / unknown
/// notifications (e.g. LSP-style). Skip those quietly instead of flagging a
/// decode error, mirroring rmcp's compatibility handling.
fn is_ignorable_notification(line: &[u8]) -> bool {
    match serde_json::from_slice::<serde_json::Value>(line) {
        Ok(v) => v.get("id").is_none() && v.get("method").and_then(|m| m.as_str()).is_some(),
        Err(_) => false,
    }
}

impl<R, W> ResilientRwTransport<R, W>
where
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    fn new(
        read: R,
        write: W,
        server_name: String,
        event_writer: echo_agent_session_events::EventWriter,
    ) -> Self {
        Self {
            read: BufReader::new(read),
            write: Arc::new(Mutex::new(Some(write))),
            server_name,
            event_writer,
        }
    }

    /// Record a skipped, undecodable stdout line: a `warn!` log plus an
    /// `McpTransportDecodeError` event carrying the serde error and a truncated
    /// sample of the raw line (the diagnostic the untagged-enum serde error
    /// alone lacks).
    fn record_decode_error(&self, line: &[u8], err: &serde_json::Error) {
        let sample: String = String::from_utf8_lossy(line)
            .chars()
            .take(DECODE_ERROR_SAMPLE_LEN)
            .collect();
        tracing::warn!(
            server = %self.server_name,
            error = %err,
            sample = %sample,
            "Skipping undecodable MCP stdout line; keeping transport alive",
        );
        self.event_writer
            .emit(echo_agent_session_events::Event::McpTransportDecodeError {
                server_name: self.server_name.clone(),
                error: err.to_string(),
                sample,
            });
    }
}

impl<R, W> Transport<RoleClient> for ResilientRwTransport<R, W>
where
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let lock = self.write.clone();
        async move {
            let mut bytes = serde_json::to_vec(&item).map_err(std::io::Error::other)?;
            bytes.push(b'\n');
            let mut guard = lock.lock().await;
            match guard.as_mut() {
                Some(write) => {
                    write.write_all(&bytes).await?;
                    write.flush().await
                }
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "transport is closed",
                )),
            }
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        loop {
            let mut line = Vec::new();
            match self.read.read_until(b'\n', &mut line).await {
                Ok(0) => return None, // genuine end-of-stream
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(
                        server = %self.server_name,
                        error = %e,
                        "MCP stdio read error; closing transport",
                    );
                    return None;
                }
            }
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                continue;
            }

            match serde_json::from_slice::<RxJsonRpcMessage<RoleClient>>(&line) {
                Ok(msg) => return Some(msg),
                // The whole point: a single undecodable line must not
                // collapse the transport — skip it and keep reading.
                Err(err) => {
                    if is_ignorable_notification(&line) {
                        tracing::trace!(
                            server = %self.server_name,
                            "Ignoring unrecognized MCP notification",
                        );
                    } else {
                        self.record_decode_error(&line, &err);
                    }
                    continue;
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        let mut guard = self.write.lock().await;
        drop(guard.take());
        Ok(())
    }
}

/// Stdio MCP transport with a non-panicking cleanup path.
///
/// Unlike `rmcp`'s `TokioChildProcess` (which `tokio::spawn`s from `Drop` and so
/// panics when dropped without an entered runtime), this wrapper's `Drop` is
/// best-effort: it reaps via the current runtime if present, else a short-lived
/// cleanup thread, so the child never leaks as a zombie. Since the caller's
/// `detach_command` `setsid`s the child into its own group, teardown also
/// `killpg`s the whole group via [`ProcessGroup`] to avoid orphaning
/// grandchildren (e.g. `npx` -> `node`) before reaping the leader.
pub struct SafeTokioChildProcess {
    child: Option<tokio::process::Child>,
    /// Strong `Arc` owner; the scope holds only a `Weak`, dropped on reap.
    process_group: Option<Arc<ProcessGroup>>,
    transport: ResilientRwTransport<tokio::process::ChildStdout, tokio::process::ChildStdin>,
}

/// Holds a newly launched stdio child and its process group until ownership
/// moves to [`SafeTokioChildProcess`]. It is built on the background thread that
/// launches the child, so if the launch is cancelled partway (the session is
/// closing) this value is still dropped, and dropping it stops the whole group:
/// the child and anything it started. The caller sets `kill_on_drop(true)` so
/// the child itself is also cleaned up on that path.
struct SpawnGuard {
    child: Option<tokio::process::Child>,
    process_group: Option<Arc<ProcessGroup>>,
}

impl SpawnGuard {
    fn new(child: tokio::process::Child, process_group: Option<Arc<ProcessGroup>>) -> Self {
        Self {
            child: Some(child),
            process_group,
        }
    }

    fn child_mut(&mut self) -> &mut tokio::process::Child {
        self.child
            .as_mut()
            .expect("guard child is present until disarm")
    }

    fn process_group(&self) -> Option<&Arc<ProcessGroup>> {
        self.process_group.as_ref()
    }

    fn disarm(mut self) -> (tokio::process::Child, Option<Arc<ProcessGroup>>) {
        (
            self.child
                .take()
                .expect("guard child is present until disarm"),
            self.process_group.take(),
        )
    }
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        if let Some(group) = &self.process_group
            && let Err(e) = group.kill()
        {
            tracing::warn!("Error killing MCP child process group on spawn cancel: {e}");
        }
    }
}

impl SafeTokioChildProcess {
    /// `server_name` and `event_writer` are passed to the transport so a skipped
    /// unreadable output line reports an `McpTransportDecodeError` event for that
    /// server. `scope`, when set, registers the child's group so it is cleaned up
    /// when the session closes.
    async fn spawn(
        mut cmd: Command,
        scope: Option<&ProcessScope>,
        server_name: String,
        event_writer: echo_agent_session_events::EventWriter,
    ) -> std::io::Result<(Self, Option<ChildStderr>)> {
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Launch the child on a background thread so the session thread is never
        // blocked, and take ownership of its group in that same step so the
        // returned guard owns the child from the moment it exists (see
        // `SpawnGuard` for what happens if the launch is cancelled).
        let mut guard = tokio::task::spawn_blocking(move || -> std::io::Result<SpawnGuard> {
            #[allow(clippy::disallowed_methods)] // group ownership is taken below, in this task
            let child = cmd.spawn()?;
            // Best effort: without a group we can still clean up the child itself.
            let process_group = match ProcessGroup::new() {
                Ok(mut group) => match group.attach(&child) {
                    Ok(()) => Some(Arc::new(group)),
                    Err(e) => {
                        tracing::warn!("Failed to attach MCP child to process group: {e}");
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to create MCP child process group: {e}");
                    None
                }
            };
            Ok(SpawnGuard::new(child, process_group))
        })
        .await
        .map_err(|e| std::io::Error::other(format!("MCP spawn task failed: {e}")))??;

        // The guard stays active through the failures below, so any early exit
        // still cleans up the whole group instead of leaking it.
        let child = guard.child_mut();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("stdin was already taken"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("stdout was already taken"))?;
        let stderr = child.stderr.take();
        // Tie this child to the spawning session: once that session closes,
        // the child is cleaned up even if another session still holds the
        // client.
        let scope_closed = match (scope, guard.process_group()) {
            (Some(scope), Some(group)) => !scope.register(group),
            _ => false,
        };
        if scope_closed {
            // `register` already stopped the group when it found the session
            // already closing, so disarm the guard and fail fast rather than
            // let its Drop stop the group a second time.
            let _ = guard.disarm();
            return Err(std::io::Error::other(
                "session is closing (process scope already reclaimed); MCP server not started",
            ));
        }

        let (child, process_group) = guard.disarm();
        Ok((
            Self {
                child: Some(child),
                process_group,
                transport: ResilientRwTransport::new(stdout, stdin, server_name, event_writer),
            },
            stderr,
        ))
    }

    fn id(&self) -> Option<u32> {
        self.child.as_ref()?.id()
    }

    /// Force-stops the whole group: the child and anything it started. It is
    /// synchronous, so it can run from `Drop` without a runtime; the child
    /// itself still needs to be waited on afterwards.
    fn kill_process_group(&self) {
        if let Some(group) = &self.process_group
            && let Err(e) = group.kill()
        {
            tracing::warn!("Error killing MCP child process group: {e}");
        }
    }

    async fn graceful_shutdown(&mut self) -> std::io::Result<()> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        self.transport.close().await?;

        let result = tokio::select! {
            _ = tokio::time::sleep(STDIO_SHUTDOWN_GRACE) => {
                self.kill_process_group();
                match child.kill().await {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        tracing::warn!("Error killing MCP child: {e}");
                        Err(e)
                    }
                }
            }
            res = child.wait() => {
                // Clean up anything the child started now, while the group id is
                // still in use, and before the child's id can be reused.
                self.kill_process_group();
                match res {
                    Ok(status) => {
                        tracing::info!("MCP child exited gracefully {status}");
                        Ok(())
                    }
                    Err(e) => {
                        tracing::warn!("Error waiting for MCP child: {e}");
                        Err(e)
                    }
                }
            }
        };

        // Leader is reaped; drop the group so `Drop` can't `killpg` a now-reusable pid.
        self.process_group = None;
        result
    }
}

impl Drop for SafeTokioChildProcess {
    fn drop(&mut self) {
        // Group teardown is synchronous, so do it first regardless of runtime.
        self.kill_process_group();

        let Some(mut child) = self.child.take() else {
            return;
        };

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(e) = child.kill().await {
                    tracing::warn!("Error killing MCP child process: {e}");
                }
            });
        } else if let Err(e) = std::thread::Builder::new()
            .name("mcp-stdio-child-cleanup".to_string())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::warn!("Error creating runtime to clean up MCP child process: {e}");
                        if let Err(e) = child.start_kill() {
                            tracing::warn!("Error signaling MCP child process during drop: {e}");
                        }
                        return;
                    }
                };

                rt.block_on(async move {
                    if let Err(e) = child.kill().await {
                        tracing::warn!("Error killing MCP child process: {e}");
                    }
                });
            })
        {
            tracing::warn!("Error spawning MCP child cleanup thread: {e}");
        }
    }
}

impl Transport<RoleClient> for SafeTokioChildProcess {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.transport.send(item)
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        self.transport.receive()
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.graceful_shutdown().await
    }
}

/// Transport configuration before connection is established.
enum PendingTransport {
    Stdio(Box<SafeTokioChildProcess>),
    Http(HttpConfig),
    HttpAuth {
        config: HttpConfig,
        auth_manager: Arc<tokio::sync::Mutex<rmcp::transport::auth::AuthorizationManager>>,
    },
    /// In-process SDK MCP server reached over the ACP reverse channel
    /// (`echo.agent/mcp/sdk_call`). Rebuildable from its `server_id` + invoker, so handshake
    /// failures restore like Http (unlike the consumed Stdio child).
    Acp {
        server_id: String,
        invoker: Arc<dyn crate::acp_transport::AcpReverseInvoker>,
    },
}

/// A connected MCP service (rmcp's RunningService wrapped in Arc).
/// Uses [`EchoAgentClientHandler`] rather than rmcp's default `ClientInfo`
/// handler: rmcp 2.1 parameterizes `RunningService` over the handler
/// type, and `ClientInfo` is only a `ClientHandler` impl with no
/// notification routing. The custom handler keeps the same protocol
/// behavior (same `get_info`) while plumbing
/// `tools/list_changed` / `resources/list_changed` notifications
/// through to the session-actor dispatcher.
pub type McpService = Arc<RunningService<RoleClient, EchoAgentClientHandler>>;

/// MCP client connection state machine.
///
/// Single-flight handshake invariant: at most one task at a time may run
/// the handshake. While the handshake is in flight the state is
/// [`ClientState::Initializing`]; the holder owns the transport for the
/// duration of [`McpClient::try_handshake`]. Concurrent callers of
/// [`McpClient::ensure_initialized`] observe [`ClientState::Initializing`]
/// and park on [`McpClient::init_done`] until the holder publishes a
/// result, instead of failing fast with
/// `"MCP client already initializing"` as in earlier versions.
enum ClientState {
    /// No transport configured. Reachable from:
    /// - [`McpClient::stub`] (test placeholder; `ensure_initialized`
    ///   returns a configuration error).
    /// - Stdio handshake failure (the spawned child process is consumed
    ///   by `client.serve` and cannot be reused — Http/HttpAuth keep
    ///   their `HttpConfig` clone and transition back to `Pending`).
    Empty,
    /// Transport is configured and ready for the next handshake.
    Pending(PendingTransport),
    /// A caller is currently inside [`McpClient::try_handshake`] and owns
    /// the transport. New callers MUST park on
    /// [`McpClient::init_done`] (with a bounded timeout) rather than
    /// attempt a parallel handshake. If the holder is cancelled or
    /// panics before publishing a result, an [`InitGuard`] restores the
    /// transport on a best-effort basis so other callers can retry.
    Initializing,
    /// Handshake completed; the service is reference-counted via `Arc`.
    Ready {
        service: McpService,
        _connected: echo_agent_telemetry::activity::ActivityGaugeGuard,
    },
}

/// `Copy` projection of [`ClientState`] used for cheap state-machine
/// inspection (see [`McpClient::state_kind`]). Mirrors the variants
/// 1:1, dropping the payloads so callers can pattern-match without
/// borrowing the state mutex's inner data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientStateKind {
    Empty,
    Pending,
    Initializing,
    Ready,
}

/// Classification used by [`crate::liveness::spawn_transport_liveness`].
///
/// Returned by [`McpClient::liveness_check`] under a single state-mutex
/// acquisition, so the watcher's per-tick predicate is atomic.
///
/// `Transient` covers states the watcher should silently exit on
/// (re-handshake races, externally-reset transports, post-failure
/// `Empty` slots). Only `Ready + transport closed` produces a
/// `TransportClosed` ACP push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivenessCheck {
    /// `Ready` + `is_transport_closed() == false`. Keep polling.
    Healthy,
    /// `Ready` + `is_transport_closed() == true`. Emit + exit.
    TransportClosed,
    /// Anything else (`Initializing`, `Pending`, `Empty`). The
    /// watcher exits silently — the new state is being managed
    /// externally; if it returns to `Ready` the owner can re-arm.
    Transient,
}

/// Events emitted by a live MCP client to its session-side dispatcher.
///
/// Produced by three sources:
///
/// 1. [`crate::liveness::spawn_transport_liveness`] when an `is_healthy`
///    poll observes that the rmcp service loop has shut down its receiver
///    (`TransportClosed`).
/// 2. [`EchoAgentClientHandler`] when the server pushes a notification we
///    care about — currently `notifications/tools/list_changed` and
///    `notifications/resources/list_changed`.
/// 3. The session/managed-config layer when a server is added, removed,
///    or successfully (re-)initialized.
///
/// Consumers fan these out to ACP `echo.agent/mcp/server_status` after 50 ms
/// of tumbling-window coalescing keyed by `(server, kind)`; see the
/// session-actor `StatusDispatcher`.
#[derive(Debug, Clone)]
pub enum McpClientEvent {
    /// The rmcp service loop has terminated; the client is no longer
    /// usable for tool calls and must be torn down (or restarted).
    TransportClosed {
        server: McpServerName,
        /// Identity of the client whose transport closed (see
        /// [`McpClient::client_id`]). A mismatch with the client
        /// currently registered under `server` marks the event stale —
        /// it must not tear down the replacement. Every emitter holds the
        /// closing `McpClient`, so the id is always known.
        client_id: u64,
    },
    /// `ensure_initialized` returned `Err(_)`; `reason` is the full
    /// stringified error, surfaced verbatim to the client (no
    /// sanitization) so failures are easy to debug.
    HandshakeFailed {
        server: McpServerName,
        reason: String,
    },
    /// Server pushed `notifications/tools/list_changed`.
    ToolsChanged { server: McpServerName },
    /// Server pushed `notifications/resources/list_changed`.
    ResourcesChanged { server: McpServerName },
    ElicitationComplete {
        server: McpServerName,
        elicitation_id: String,
    },
    /// Client transitioned to [`ClientState::Ready`]; dispatcher uses
    /// this to surface "ready" status without polling. Emitted from
    /// `ensure_initialized`; the dispatcher maps it to
    /// `reason=initialized` (NOT `reason=restart_succeeded`, which is
    /// reserved for the restart path).
    Ready { server: McpServerName },
    /// Managed/local config diff resolved. The dispatcher fans this
    /// out into one [`Self::ConfigAdded`] / [`Self::ConfigRemoved`]
    /// event per affected server before buffering.
    ConfigDiff {
        added: Vec<McpServerName>,
        removed: Vec<McpServerName>,
    },
    /// Per-server `(server, ConfigAdded)` fan-out variant produced by
    /// the dispatcher from a [`Self::ConfigDiff`]. Keeps the
    /// `kind ↔ event payload` invariant: storing a fake `Ready`
    /// payload at a `ConfigAdded` key would be a footgun whenever a
    /// real `Ready` and a `ConfigDiff` collided in the same coalesce
    /// window.
    ConfigAdded { server: McpServerName },
    /// Per-server `(server, ConfigRemoved)` fan-out variant — the
    /// dispatched analogue of [`Self::ConfigAdded`] for the removed
    /// set of a [`Self::ConfigDiff`].
    ConfigRemoved { server: McpServerName },
}

/// Discriminant for [`McpClientEvent`], used as the second half of the
/// coalescing key `(server, kind)`. Two events with the same
/// `(server, kind)` collapse into the latest one inside the
/// dispatcher's 50 ms window.
///
/// Distinct from [`McpClientEvent`] because the latter carries
/// payload (e.g. `reason` on `HandshakeFailed`) that we don't want
/// participating in equality / hashing.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum McpClientEventKind {
    TransportClosed,
    HandshakeFailed,
    ToolsChanged,
    ResourcesChanged,
    ElicitationComplete,
    Ready,
    ConfigAdded,
    ConfigRemoved,
}

impl McpClientEvent {
    /// Server name carried by the event, if any.
    ///
    /// Returns `None` only for [`McpClientEvent::ConfigDiff`] — that
    /// variant is fanned out per-server by the dispatcher into
    /// [`Self::ConfigAdded`] / [`Self::ConfigRemoved`], where each
    /// fan-out child has a single server name.
    pub fn server_name(&self) -> Option<&str> {
        match self {
            Self::TransportClosed { server, .. }
            | Self::HandshakeFailed { server, .. }
            | Self::ToolsChanged { server }
            | Self::ResourcesChanged { server }
            | Self::ElicitationComplete { server, .. }
            | Self::Ready { server }
            | Self::ConfigAdded { server }
            | Self::ConfigRemoved { server } => Some(server.as_str()),
            Self::ConfigDiff { .. } => None,
        }
    }
}

/// RAII guard that restores [`ClientState::Pending`] if the
/// [`McpClient::ensure_initialized`] holder is dropped before publishing
/// its handshake result (task cancellation, panic). Without this guard a
/// cancellation mid-handshake would leave `state` stuck in
/// [`ClientState::Initializing`] and every subsequent caller would block
/// until the wait-timeout fallback fires, then return an error — the
/// caller would have to call [`McpClient::reset_transport`]
/// manually to recover.
///
/// On the success path the holder calls [`Self::disarm`] before storing
/// `Ready`/`Empty` under the state lock, which converts the `Drop` into
/// a no-op. The guard never restores on the success path.
///
/// Drop uses [`tokio::sync::Mutex::try_lock`] because `Drop` runs
/// synchronously and we cannot block the runtime here. If the lock is
/// contended (extremely rare — the only competing locker is another
/// `ensure_initialized` caller which holds the lock for the duration of
/// a match arm, microseconds), the restore is skipped and the
/// inflight-wait timeout in `ensure_initialized` becomes the
/// last-resort recovery path.
struct InitGuard<'a> {
    state: &'a Mutex<ClientState>,
    init_done: &'a Notify,
    /// `Some` until [`Self::disarm`] is called. Holds the restorable
    /// transport (HTTP / HttpAuth) or `None` for Stdio (whose child
    /// process is consumed by `client.serve` and cannot be reused).
    restore: Option<PendingTransport>,
}

impl InitGuard<'_> {
    /// Mark the guard as having published a result. Subsequent `Drop`
    /// becomes a no-op so it doesn't fight with the holder's own
    /// state-store-under-the-lock or wake waiters twice.
    fn disarm(&mut self) {
        self.restore = None;
    }
}

impl Drop for InitGuard<'_> {
    fn drop(&mut self) {
        let Some(restore) = self.restore.take() else {
            return;
        };
        // Best-effort restore. `try_lock` cannot block the runtime from
        // inside Drop; on contention the slot stays Initializing and the
        // inflight-wait timeout becomes the recovery path.
        if let Ok(mut guard) = self.state.try_lock()
            && matches!(&*guard, ClientState::Initializing)
        {
            *guard = ClientState::Pending(restore);
        }
        // Notify whether or not we managed to restore — parked waiters
        // need to wake up and either retry against the restored
        // transport or hit the wait-timeout error path.
        self.init_done.notify_waiters();
    }
}

/// Build a restorable handle for a pending transport, or `None` if the
/// transport cannot be reused after a handshake failure.
///
/// `PendingTransport` deliberately does not implement `Clone`: the
/// `Stdio` variant owns a [`tokio::process::Child`] that is consumed by
/// `client.serve`, and a "restored" Stdio entry would be a dead handle.
/// HTTP and HttpAuth, by contrast, only carry config + an `Arc` to a
/// shared auth manager, so a clone is the canonical way to retry.
fn restorable_transport(pending: &PendingTransport) -> Option<PendingTransport> {
    match pending {
        PendingTransport::Http(cfg) => Some(PendingTransport::Http(cfg.clone())),
        PendingTransport::HttpAuth {
            config,
            auth_manager,
        } => Some(PendingTransport::HttpAuth {
            config: config.clone(),
            auth_manager: auth_manager.clone(),
        }),
        PendingTransport::Acp { server_id, invoker } => Some(PendingTransport::Acp {
            server_id: server_id.clone(),
            invoker: invoker.clone(),
        }),
        PendingTransport::Stdio(_) => None,
    }
}

/// Monotonic source for [`McpClient::client_id`]. Process-global so every
/// client instance — including test stubs — gets a unique identity.
static NEXT_CLIENT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_client_id() -> u64 {
    NEXT_CLIENT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub struct McpClient {
    /// Unique identity of this client *instance*. See [`Self::client_id`].
    client_id: u64,
    server_name: McpServerName,
    state: Mutex<ClientState>,
    /// Wakes [`Self::ensure_initialized`] callers that observed
    /// [`ClientState::Initializing`] and parked. Notified after each
    /// handshake attempt finishes (success **or** failure) and `state`
    /// has been updated. See [`ClientState`] for the single-flight
    /// invariant this preserves.
    ///
    /// Replaces the previous fail-fast
    /// `McpError::ClientError("MCP client already initializing")` branch
    /// which leaked into model-visible tool results whenever the model's
    /// first tool dispatch raced the session actor's background
    /// `get_tool_registrations` handshake.
    init_done: Notify,
    /// Terminal cancellation for client eviction. Unlike `state = Empty`, the
    /// token is visible to the task that temporarily owns a transport while
    /// `state` is `Initializing`, allowing shutdown to drop an in-flight stdio
    /// `serve` future immediately and preventing the handshake from publishing
    /// `Ready` after eviction.
    shutdown_token: tokio_util::sync::CancellationToken,
    startup_timeout_sec: u64,
    tool_timeout_sec: u64,
    /// Per-tool timeout overrides in seconds. Looked up by tool name;
    /// falls back to `tool_timeout_sec` when a tool isn't listed.
    tool_timeouts: HashMap<ToolName, u64>,
    /// See [`McpServerMetaConfig::expose_image_base64`].
    expose_image_base64: bool,
    /// Shared `AuthorizationManager` for OAuth-enabled servers. `AuthClient`
    /// inside the transport holds a clone of this Arc so token updates are
    /// visible to both the transport and the re-auth path.
    auth_manager: Option<Arc<tokio::sync::Mutex<rmcp::transport::auth::AuthorizationManager>>>,
    /// Stored for OAuth clients so we can rebuild the transport after re-auth.
    http_config: Option<HttpConfig>,
    /// BYO OAuth config for the full browser flow fallback (when refresh fails).
    byo_oauth_config: Option<McpOAuthConfig>,
    /// Rate limit on this server's reconnect warnings; passed to each HTTP
    /// transport so rebuilds keep the limit.
    warn_budget: crate::mcp_http_client::WarnBudget,
    /// The transport to rebuild on a dead connection — see
    /// [`McpClient::reset_transport`]. `None` for transports that can't
    /// reconnect, e.g. Stdio (whose child process is consumed by the
    /// handshake and can't be restarted from here).
    reconnect: Option<PendingTransport>,
    /// Event sink for transport-closed pollers, server-pushed
    /// `tools/list_changed` / `resources/list_changed` notifications,
    /// and handshake failures.
    ///
    /// The slot is `Some` after [`Self::set_event_tx`] is called and
    /// `None` otherwise. The `Arc<Mutex<...>>` is **shared with
    /// [`EchoAgentClientHandler`]** constructed by
    /// [`Self::make_client_handler`]: the handler holds a clone of
    /// the same Arc and reads through it on every notification.
    /// Snapshotting the slot at handshake time instead would mean any
    /// session that wired `notify_tx` post-handshake silently lost
    /// every `tools/list_changed` and `resources/list_changed` for the
    /// life of the connection.
    ///
    /// `None` in three cases:
    /// 1. Test stubs and standalone-pool fixtures that don't need
    ///    cross-component event flow.
    /// 2. Subagent / shared-pool snapshots — only the **parent** session
    ///    is the owner of these events. A subagent that inherits a
    ///    shared `Arc<McpClient>` reads tools through it but does not
    ///    install its own dispatcher; the parent's
    ///    [`crate::liveness::TransportLivenessHandle`] already covers it.
    /// 3. Brand-new clients before the session's per-server task has
    ///    called [`Self::set_event_tx`].
    ///
    /// `parking_lot::Mutex` is sufficient (and lighter than the previous
    /// `tokio::sync::Mutex`): the lock is never held across an
    /// `.await`, and the handler's `emit` path is short and
    /// allocation-free.
    notify_tx: SharedEventTx,
    elicitation_tx: crate::elicitation::SharedElicitationTx,
    /// RAII handle for the per-client transport-liveness poller.
    ///
    /// `Some` after [`Self::arm_liveness_watcher`] succeeds; `None`
    /// initially. The slot is also cleared by the poller itself when
    /// it exits (whether on `TransportClosed` or because the state
    /// machine drifted out of `Ready` during a re-handshake — see
    /// [`crate::liveness::spawn_transport_liveness`]) so subsequent
    /// `arm_liveness_watcher` calls aren't silently blocked by a
    /// dead-but-still-present handle.
    ///
    /// `parking_lot::Mutex` is sufficient because the lock is only ever
    /// held for the duration of a slot swap. The poller task uses an
    /// internal `Arc` clone of this mutex (the same memory) so it
    /// can clear the slot before `break`.
    liveness_handle: Arc<parking_lot::Mutex<Option<crate::liveness::TransportLivenessHandle>>>,
}

/// Shared sender slot type — the same Arc lives on the [`McpClient`]
/// and the [`EchoAgentClientHandler`] it constructs during
/// [`McpClient::try_handshake`]. Mutating the slot via
/// [`McpClient::set_event_tx`] is observed by the live rmcp service
/// loop on the next notification, so there's no "snapshot at
/// handshake" hazard.
pub type SharedEventTx =
    Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::UnboundedSender<McpClientEvent>>>>;

/// External-config overrides for an MCP server, surfaced to echo-agent-mcp
/// from whatever loader the host crate uses (e.g. the host's `config.toml` parser).
///
/// All fields are pre-precedence: [`McpClient::load_timeouts`] (and
/// [`McpClient::load_expose_image_base64`]) still apply the
/// `_meta > overrides > default` precedence on top. Owning these types here
/// keeps MCP transport state free of the host's TOML schema.
///
/// Name retained for call-site stability; struct now carries non-timeout
/// config too (e.g. [`Self::expose_image_base64`]).
#[derive(Default, Debug, Clone)]
pub struct McpClientTimeoutOverrides {
    /// Server startup timeout in seconds.
    pub startup_timeout_sec: Option<u64>,
    /// Default per-tool timeout in seconds (used when a tool has no entry in `tool_timeouts`).
    pub tool_timeout_sec: Option<u64>,
    /// Per-tool timeout overrides in seconds, keyed by tool name.
    pub tool_timeouts: Option<HashMap<String, u64>>,
    /// See [`McpServerMetaConfig::expose_image_base64`].
    pub expose_image_base64: Option<bool>,
}

impl McpClient {
    fn load_timeouts(
        overrides: Option<&McpClientTimeoutOverrides>,
        meta_config: Option<&McpServerMetaConfig>,
    ) -> (u64, u64, HashMap<ToolName, u64>) {
        // _meta > overrides > default; env / config / requirements / remote are
        // resolved by the shell and injected via `overrides.startup_timeout_sec`.
        let startup = meta_config
            .and_then(|mc| mc.startup_timeout_ms)
            .map(|ms| ms.div_ceil(1000))
            .or_else(|| overrides.and_then(|o| o.startup_timeout_sec))
            .unwrap_or(DEFAULT_STARTUP_TIMEOUT_SECS);

        let tool = meta_config
            .and_then(|mc| mc.tool_timeout_ms)
            .map(|ms| ms.div_ceil(1000))
            .or_else(|| overrides.and_then(|o| o.tool_timeout_sec))
            .unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS);

        // Per-tool overrides: external base, _meta overrides on top.
        // Precedence: _meta per-tool > overrides per-tool > (falls back to server-level `tool`)
        let mut tool_timeouts = HashMap::new();

        // Layer 1: overrides (already in seconds)
        if let Some(o) = overrides
            && let Some(ref tt) = o.tool_timeouts
        {
            tool_timeouts.extend(tt.iter().map(|(k, v)| (k.clone(), *v)));
        }

        // Layer 2: _meta tool_timeouts_ms (milliseconds → seconds), overrides external config
        if let Some(mc) = meta_config
            && let Some(ref tt) = mc.tool_timeouts_ms
        {
            for (k, v) in tt {
                tool_timeouts.insert(k.clone(), v.div_ceil(1000));
            }
        }

        (startup, tool, tool_timeouts)
    }

    /// `_meta > overrides > default(false)`, mirroring [`Self::load_timeouts`].
    fn load_expose_image_base64(
        overrides: Option<&McpClientTimeoutOverrides>,
        meta_config: Option<&McpServerMetaConfig>,
    ) -> bool {
        meta_config
            .and_then(|mc| mc.expose_image_base64)
            .or_else(|| overrides.and_then(|o| o.expose_image_base64))
            .unwrap_or(false)
    }

    /// The ONLY place that writes the `McpClient { .. }` struct literal.
    /// Every constructor funnels through here so adding a field touches one
    /// site. `reconnect` is snapshotted from the transport before it is
    /// moved into [`ClientState::Pending`] (`None` for non-reconnectable
    /// transports like Stdio — see [`restorable_transport`]).
    #[allow(clippy::too_many_arguments)]
    fn new_with_transport(
        server_name: String,
        transport: PendingTransport,
        overrides: Option<&McpClientTimeoutOverrides>,
        meta_config: Option<&McpServerMetaConfig>,
        auth_manager: Option<Arc<tokio::sync::Mutex<rmcp::transport::auth::AuthorizationManager>>>,
        http_config: Option<HttpConfig>,
        byo_oauth_config: Option<McpOAuthConfig>,
    ) -> Self {
        let reconnect = restorable_transport(&transport);
        let (startup_timeout_sec, tool_timeout_sec, tool_timeouts) =
            Self::load_timeouts(overrides, meta_config);
        let expose_image_base64 = Self::load_expose_image_base64(overrides, meta_config);
        Self {
            client_id: next_client_id(),
            server_name,
            state: Mutex::new(ClientState::Pending(transport)),
            init_done: Notify::new(),
            shutdown_token: tokio_util::sync::CancellationToken::new(),
            startup_timeout_sec,
            tool_timeout_sec,
            tool_timeouts,
            expose_image_base64,
            auth_manager,
            http_config,
            byo_oauth_config,
            warn_budget: crate::mcp_http_client::WarnBudget::default(),
            reconnect,
            notify_tx: Arc::new(parking_lot::Mutex::new(None)),
            elicitation_tx: Arc::new(parking_lot::Mutex::new(None)),
            liveness_handle: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    pub fn new_http_auth(
        server_name: String,
        config: HttpConfig,
        auth_manager: Arc<tokio::sync::Mutex<rmcp::transport::auth::AuthorizationManager>>,
        byo_oauth_config: Option<McpOAuthConfig>,
        overrides: Option<&McpClientTimeoutOverrides>,
        meta_config: Option<&McpServerMetaConfig>,
    ) -> Self {
        Self::new_with_transport(
            server_name,
            PendingTransport::HttpAuth {
                config: config.clone(),
                auth_manager: auth_manager.clone(),
            },
            overrides,
            meta_config,
            Some(auth_manager),
            Some(config),
            byo_oauth_config,
        )
    }

    pub fn has_auth(&self) -> bool {
        self.auth_manager.is_some()
    }

    /// Try to recover tokens from disk or via refresh — no browser flow.
    ///
    /// Returns true if valid tokens were found (from another session/process
    /// writing to the credential store, or a successful token refresh).
    /// Used by `retry_auth_required_servers` on overlay refresh.
    pub async fn try_reauth_from_disk(&self) -> bool {
        let (Some(auth_mgr), Some(config)) = (&self.auth_manager, &self.http_config) else {
            return false;
        };

        // Token-changed gate: rmcp's `initialize_from_store` returns Ok(true)
        // for any disk-resident creds regardless of expiry, so without
        // comparing against the in-memory token we'd claim "fresh tokens from
        // disk" on the same stale token that triggered this retry. The
        // downstream handshake would catch it, but the log line would lie
        // during incident debugging — and the divergence from `force_reauth`'s
        // gate is the exact invariant drift we just fixed there.
        {
            use oauth2::TokenResponse as _;
            let mut mgr = auth_mgr.lock().await;
            let token_before = mgr
                .get_credentials()
                .await
                .ok()
                .and_then(|(_, tok)| tok)
                .map(|t| t.access_token().secret().to_string());
            if let Ok(true) = mgr.initialize_from_store().await {
                let token_after = mgr
                    .get_credentials()
                    .await
                    .ok()
                    .and_then(|(_, tok)| tok)
                    .map(|t| t.access_token().secret().to_string());
                if token_after.is_some() && token_after != token_before {
                    tracing::info!(
                        server = self.server_name.as_str(),
                        "Loaded fresh tokens from disk"
                    );
                    drop(mgr);
                    self.replace_state(ClientState::Pending(PendingTransport::HttpAuth {
                        config: config.clone(),
                        auth_manager: auth_mgr.clone(),
                    }))
                    .await;
                    return true;
                }
            }
        }

        let refresh_ok = {
            let mgr = auth_mgr.lock().await;
            mgr.refresh_token().await.is_ok()
        };

        if refresh_ok {
            self.replace_state(ClientState::Pending(PendingTransport::HttpAuth {
                config: config.clone(),
                auth_manager: auth_mgr.clone(),
            }))
            .await;
            return true;
        }

        false
    }

    /// Force token acquisition and reset the transport so the next
    /// `ensure_initialized` rebuilds it with the fresh token.
    ///
    /// Tries in order:
    /// 1. Reload from disk (picks up tokens from background auth task)
    /// 2. Refresh via refresh_token grant
    /// 3. Full browser-based OAuth flow — unless the refresh failure was a
    ///    pure network failure ([`mcp_refresh_failure_is_transient`]): the
    ///    stored refresh token is then still presumed valid, and opening a
    ///    browser tab / re-running DCR for a Wi-Fi blip right after
    ///    wake-from-sleep is both useless (the IdP is unreachable for the
    ///    browser too) and destructive (it discards a working credential).
    pub async fn force_reauth(&self, force: bool) -> bool {
        let (Some(auth_mgr), Some(config)) = (&self.auth_manager, &self.http_config) else {
            return false;
        };

        // Check if another process/session wrote *fresh* tokens to disk. We
        // must compare against the token we already had in memory — rmcp's
        // `initialize_from_store` returns Ok(true) for any disk-resident
        // credentials regardless of expiry, so without the token-changed
        // check we'd short-circuit on the same stale token that triggered
        // this re-auth in the first place (real bug: pressing the auth
        // shortcut on a server with an expired bearer + no refresh_token
        // would no-op and then 401 on the next handshake).
        //
        // Hold a single lock guard across `token_before` → `initialize_from_store`
        // → `token_after` so the comparison's invariant ("snapshot, reload,
        // re-read") can't be torn by an interleaved mutation.
        {
            use oauth2::TokenResponse as _;
            let mut mgr = auth_mgr.lock().await;
            let token_before = mgr
                .get_credentials()
                .await
                .ok()
                .and_then(|(_, tok)| tok)
                .map(|t| t.access_token().secret().to_string());
            if let Ok(true) = mgr.initialize_from_store().await {
                let token_after = mgr
                    .get_credentials()
                    .await
                    .ok()
                    .and_then(|(_, tok)| tok)
                    .map(|t| t.access_token().secret().to_string());
                if token_after.is_some() && token_after != token_before {
                    tracing::info!(
                        server = self.server_name.as_str(),
                        "Loaded fresh tokens from disk (background auth or other process)"
                    );
                    drop(mgr);
                    self.replace_state(ClientState::Pending(PendingTransport::HttpAuth {
                        config: config.clone(),
                        auth_manager: auth_mgr.clone(),
                    }))
                    .await;
                    return true;
                }
            }
        }

        // Try token refresh.
        let refresh_result = {
            let mgr = auth_mgr.lock().await;
            mgr.refresh_token().await
        };

        match refresh_result {
            Ok(_) => {
                tracing::info!(
                    server = self.server_name.as_str(),
                    "Token refreshed successfully (no browser)"
                );
                self.replace_state(ClientState::Pending(PendingTransport::HttpAuth {
                    config: config.clone(),
                    auth_manager: auth_mgr.clone(),
                }))
                .await;
                return true;
            }
            // Transient (network never reached the IdP): fail the attempt
            // instead of discarding a presumed-good credential — the retry
            // paths re-run the refresh once the network is back. An explicit
            // user trigger (`force`) still opens the browser.
            Err(ref e) if !force && mcp_refresh_failure_is_transient(e) => {
                tracing::warn!(
                    server = self.server_name.as_str(),
                    error = %e,
                    "Token refresh failed transiently (network); skipping browser escalation"
                );
                return false;
            }
            Err(e) => {
                tracing::info!(
                    server = self.server_name.as_str(),
                    error = %e,
                    "Token refresh failed terminally; falling back to browser auth"
                );
            }
        }

        // Full browser-based OAuth flow.
        {
            tracing::info!(
                server = self.server_name.as_str(),
                "Falling back to browser auth"
            );
            if let Err(e) = crate::oauth::authenticate_mcp_server_dedup(
                &self.server_name,
                &config.url,
                auth_mgr,
                self.byo_oauth_config.as_ref(),
                force,
            )
            .await
            {
                tracing::warn!(
                    server = self.server_name.as_str(),
                    %e,
                    "Full re-authentication failed"
                );
                return false;
            }
        }

        self.replace_state(ClientState::Pending(PendingTransport::HttpAuth {
            config: config.clone(),
            auth_manager: auth_mgr.clone(),
        }))
        .await;
        true
    }

    /// Reset the transport so the next `ensure_initialized` rebuilds it with a
    /// fresh connection.
    ///
    /// Called when a tool call fails with a transport error (`TransportClosed`,
    /// `TransportSend`) — the underlying connection is dead but the server's
    /// addressing (URL/headers for HTTP, `server_id`/invoker for ACP) is still
    /// valid.
    ///
    /// Returns `true` if the transport was reset: HTTP/HttpAuth/ACP rebuild
    /// from the `reconnect` snapshot taken at construction. Returns `false`
    /// for clients whose `reconnect` is `None` (e.g. Stdio — dead child
    /// processes can't be restarted from here).
    async fn reset_transport(&self) -> bool {
        let Some(t) = self.reconnect.as_ref().and_then(restorable_transport) else {
            return false;
        };
        self.replace_state(ClientState::Pending(t)).await;
        tracing::info!(
            server = %self.server_name,
            "Reset transport for reconnect after transport failure"
        );
        true
    }

    /// `true` if this client has an HTTP/SSE transport. The explicit predicate
    /// for recovery gates (the proactive path only recovers HTTP clients).
    pub fn is_http(&self) -> bool {
        self.http_config.is_some()
    }

    /// `true` for an in-process SDK client reached over the ACP reverse channel
    /// (rather than HTTP/stdio). Gates liveness watching — see
    /// [`Self::arm_liveness_watcher`].
    pub fn is_acp(&self) -> bool {
        matches!(self.reconnect, Some(PendingTransport::Acp { .. }))
    }

    /// Recover a dead transport in place: reset → re-handshake → re-arm the
    /// liveness watcher. Returns the live [`McpService`].
    ///
    /// The single recovery path for both the proactive HTTP recovery
    /// (`SessionActor::reset_http_client`, gated on [`Self::is_http`]) and the
    /// lazy `try_call_tool` retry. Rebuilds from the `reconnect` snapshot, so it
    /// covers HTTP/HttpAuth/ACP; `arm_liveness_watcher` self-gates for ACP.
    ///
    /// `Err` for a client with no restorable transport (e.g. Stdio — its child
    /// was consumed by the handshake).
    pub async fn recover(self: &Arc<Self>) -> Result<McpService, McpError> {
        // Coalesce concurrent recoveries: reset only when Ready; if already
        // non-Ready a recovery is in flight, so join its single-flight
        // ensure_initialized instead of racing a reset.
        if matches!(self.state_kind().await, ClientStateKind::Ready)
            && !self.reset_transport().await
        {
            return Err(McpError::ClientError(format!(
                "MCP client {} has no transport to recover",
                self.server_name,
            )));
        }
        let service = self.ensure_initialized().await?;
        // Re-arm liveness so the next close is detected again. A `false` return
        // with a wired sender is unexpected only for watched (non-ACP) clients.
        if !self
            .arm_liveness_watcher(crate::liveness::DEFAULT_POLL_INTERVAL)
            .await
            && !self.is_acp()
            && self.event_tx_clone().is_some()
        {
            tracing::warn!(
                server = %self.server_name,
                "recovery: liveness watcher not re-armed despite a wired event sender",
            );
        }
        Ok(service)
    }

    /// Replace [`Self::state`] under the lock and wake any
    /// [`Self::ensure_initialized`] callers parked on [`Self::init_done`]
    /// so they re-check the new state on their next loop iteration.
    ///
    /// Use this for *external* state transitions that need to invalidate
    /// in-flight waits (re-auth completions, transport resets) so a parked
    /// waiter doesn't sit on a stale [`ClientState::Initializing`] view of
    /// the world. `ensure_initialized` itself doesn't go through this
    /// helper because it already mints the new state under the lock it's
    /// holding and notifies waiters once at the end of the handshake
    /// attempt.
    async fn replace_state(&self, new_state: ClientState) {
        {
            let mut guard = self.state.lock().await;
            *guard = new_state;
        }
        self.init_done.notify_waiters();
    }

    pub fn new_stdio(
        server_name: String,
        transport: SafeTokioChildProcess,
        overrides: Option<&McpClientTimeoutOverrides>,
        meta_config: Option<&McpServerMetaConfig>,
    ) -> Self {
        Self::new_with_transport(
            server_name,
            PendingTransport::Stdio(Box::new(transport)),
            overrides,
            meta_config,
            None,
            None,
            None,
        )
    }

    /// Build a client for an in-process SDK MCP server reached over the ACP reverse
    /// channel. `server_id` is the id the agent echoes back in `echo.agent/mcp/sdk_call`; the
    /// `invoker` performs the reverse request. Same downstream path as HTTP/stdio.
    pub fn new_acp(
        server_name: String,
        server_id: String,
        invoker: Arc<dyn crate::acp_transport::AcpReverseInvoker>,
        overrides: Option<&McpClientTimeoutOverrides>,
        meta_config: Option<&McpServerMetaConfig>,
    ) -> Self {
        Self::new_with_transport(
            server_name,
            PendingTransport::Acp { server_id, invoker },
            overrides,
            meta_config,
            None,
            None,
            None,
        )
    }

    pub fn new_http(
        server_name: String,
        config: HttpConfig,
        overrides: Option<&McpClientTimeoutOverrides>,
        meta_config: Option<&McpServerMetaConfig>,
    ) -> Self {
        Self::new_with_transport(
            server_name,
            PendingTransport::Http(config.clone()),
            overrides,
            meta_config,
            None,
            Some(config),
            None,
        )
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Unique identity of this client *instance*. Two clients for the
    /// same server name (e.g. a dead client and its replacement after
    /// a config remove+re-add) have different ids. Carried on
    /// [`McpClientEvent::TransportClosed`] so consumers can tell a
    /// death event for the current client from a stale predecessor's.
    pub fn client_id(&self) -> u64 {
        self.client_id
    }

    pub fn startup_timeout_sec(&self) -> u64 {
        self.startup_timeout_sec
    }

    pub fn tool_timeout_sec(&self) -> u64 {
        self.tool_timeout_sec
    }

    /// See [`McpServerMetaConfig::expose_image_base64`].
    pub fn expose_image_base64(&self) -> bool {
        self.expose_image_base64
    }

    /// Resolve the timeout for a specific tool.
    ///
    /// Precedence (highest → lowest):
    /// 1. `_meta.mcpConfig.<server>.toolTimeoutsMs.<tool>`
    /// 2. `config.toml [mcp_servers.<server>].tool_timeouts.<tool>`
    /// 3. `_meta.mcpConfig.<server>.toolTimeoutMs`
    /// 4. `config.toml [mcp_servers.<server>].tool_timeout_sec`
    /// 5. Default (60s)
    ///
    /// Steps 1–2 are already merged into `self.tool_timeouts` at construction;
    /// steps 3–5 are already resolved into `self.tool_timeout_sec`.
    pub fn tool_timeout_for(&self, tool_name: &str) -> u64 {
        self.tool_timeouts
            .get(tool_name)
            .copied()
            .unwrap_or(self.tool_timeout_sec)
    }

    /// Drive the MCP handshake to completion (or return the cached
    /// service if one is already established), with single-flight
    /// semantics that are safe under arbitrary concurrent callers.
    ///
    /// ## Concurrency contract
    ///
    /// At most one task at a time runs [`Self::try_handshake`]; that task
    /// holds the transport and observes [`ClientState::Initializing`].
    /// Other concurrent callers park on [`Self::init_done`] (with a
    /// bounded timeout) instead of issuing parallel handshakes or
    /// failing immediately. When the holder publishes a result, all
    /// parked waiters re-check `state` and either:
    ///
    /// - return the freshly-stored [`McpService`] (handshake succeeded),
    /// - take ownership of the freshly-restored transport and run their
    ///   own handshake (handshake failed but transport is restorable),
    /// - or surface the error (Stdio handshake failed → no restorable
    ///   transport → [`ClientState::Empty`]).
    ///
    /// This replaces the pre-fix behavior where concurrent callers got
    /// an immediate `McpError::ClientError("MCP client already
    /// initializing")` — surfaced inside model-visible tool results
    /// whenever the model's first tool call landed inside the session
    /// actor's background `get_tool_registrations` handshake, causing
    /// repeated retries that exhausted prompt budgets without ever
    /// reaching the actual MCP server.
    ///
    /// ## Cancellation safety
    ///
    /// If the holder is dropped (parent task cancelled, panic) before
    /// publishing a result, [`InitGuard`]'s `Drop` impl best-effort
    /// restores the transport (so future callers can retry without an
    /// explicit `reset_transport`) and wakes parked waiters. The
    /// restore uses [`tokio::sync::Mutex::try_lock`] because `Drop` is
    /// synchronous; on the rare contention case the slot stays
    /// `Initializing` and the wait-timeout fallback below surfaces a
    /// clear error rather than blocking forever.
    pub async fn ensure_initialized(&self) -> Result<McpService, McpError> {
        if self.shutdown_token.is_cancelled() {
            return Err(McpError::ClientError(format!(
                "MCP client {} has been shut down",
                self.server_name
            )));
        }
        // Bound how long a parked caller waits on `init_done` before
        // surfacing an error. `try_handshake` is itself bounded by
        // `startup_timeout_sec`, so anything beyond that plus a 1 s margin
        // means the holder was dropped without restoring the transport
        // (cancellation under heavy contention) — wedging silently would
        // turn this into the exact "stuck client" failure mode the rest of
        // this rewrite is designed to eliminate.
        let inflight_wait =
            std::time::Duration::from_secs(self.startup_timeout_sec.saturating_add(1));

        // Drive the loop body until we either return directly or break
        // out with an owned `PendingTransport`. We deliberately use a
        // labelled `loop` with a `break <expr>` so the compiler proves
        // every arm of the inner match either diverges (return /
        // continue) or yields the transport — no `unreachable!()`
        // escape hatch needed.
        let pending: PendingTransport = loop {
            // Subscribe to `init_done` BEFORE inspecting `state` so a
            // wake-up fired between our state check and the wait can't be
            // lost. `tokio::sync::Notify` only delivers a permit to
            // notify-futures that exist at the time of `notify_waiters`.
            let notified = self.init_done.notified();
            tokio::pin!(notified);

            let mut guard = self.state.lock().await;
            // Swap the current state for `Initializing` up front and
            // match on the OWNED previous value. This avoids the
            // `match-by-ref → mem::replace → re-match → unreachable!()`
            // dance — the compiler can bind `ClientState::Pending(t)`
            // directly from an owned value with no irrefutable-let
            // hole. Non-Pending arms restore their original variant
            // before falling through; the lock is held the entire
            // window so the brief `Initializing` placeholder is
            // invisible to other callers. Cost is one trivial unit-
            // variant write per non-Pending call (plus an `Arc::clone`
            // on the Ready path) — negligible.
            match std::mem::replace(&mut *guard, ClientState::Initializing) {
                ClientState::Ready {
                    service,
                    _connected,
                } => {
                    let ready = service.clone();
                    *guard = ClientState::Ready {
                        service,
                        _connected,
                    };
                    return Ok(ready);
                }
                ClientState::Empty => {
                    *guard = ClientState::Empty;
                    return Err(McpError::ClientError(format!(
                        "MCP client {} has no transport configured",
                        self.server_name,
                    )));
                }
                ClientState::Initializing => {
                    // Another caller already owns the slot. Restore the
                    // placeholder we just swapped in (semantically a
                    // no-op since `Initializing` is a unit variant),
                    // drop the lock, park on `init_done`.
                    *guard = ClientState::Initializing;
                    drop(guard);
                    tokio::select! {
                        _ = self.shutdown_token.cancelled() => {
                            return Err(McpError::ClientError(format!(
                                "MCP client {} has been shut down",
                                self.server_name,
                            )));
                        }
                        waited = tokio::time::timeout(inflight_wait, notified.as_mut()) => match waited {
                            Ok(()) => continue,
                            Err(_) => {
                                return Err(McpError::ClientError(format!(
                                    "MCP client {} init still in progress after {}s",
                                    self.server_name,
                                    inflight_wait.as_secs(),
                                )));
                            }
                        }
                    }
                }
                // The single arm that KEEPS the `Initializing`
                // placeholder we swapped in — this caller becomes the
                // single-flight handshake holder for the duration of
                // `try_handshake` below.
                ClientState::Pending(transport) => break transport,
            }
        };

        // Lock released. Run the handshake outside the lock so other
        // callers can park on `init_done` instead of stalling on
        // `state.lock()`.

        // Clone the transport's restorable handle twice — once for
        // the failure-path retry below, once for the drop guard.
        // `PendingTransport` is intentionally not `Clone` (Stdio's
        // `TokioChildProcess` is unique), so the helper returns
        // `None` for Stdio (whose handshake failures cannot be
        // recovered without a fresh spawn).
        let restore = restorable_transport(&pending);
        let restore_for_guard = restorable_transport(&pending);

        // Drop guard: if `try_handshake` panics or is cancelled before
        // we publish a result, restore `Pending(restore)` so other
        // callers don't stall on `Initializing` forever. Disarm via
        // `disarm()` immediately before storing the real result.
        let mut init_guard = InitGuard {
            state: &self.state,
            init_done: &self.init_done,
            restore: restore_for_guard,
        };

        let handshake_start = std::time::Instant::now();
        let mut result = tokio::select! {
            biased;
            _ = self.shutdown_token.cancelled() => Err(McpError::ClientError(format!(
                "MCP client {} was shut down during initialization",
                self.server_name,
            ))),
            result = self.try_handshake(pending) => result,
        };

        let handshake_elapsed = handshake_start.elapsed().as_micros() as u64;
        tracing::info!(target: echo_agent_telemetry::instrumentation::TARGET, event = "timing", name = "mcp_try_handshake", elapsed_us = handshake_elapsed);
        // On handshake failure, if we have an auth_manager, try
        // refreshing the token and retrying once. Handles expired
        // access tokens loaded from disk — the handshake fails at the
        // transport layer before rmcp's transparent 401 refresh can
        // kick in. We attempt refresh on any failure (not just auth
        // errors) because the cost is low and error strings from
        // different MCP servers are not reliable to match.
        if !self.shutdown_token.is_cancelled()
            && result.is_err()
            && let (Some(auth_mgr), Some(config)) = (&self.auth_manager, &self.http_config)
        {
            tracing::info!(
                server = %self.server_name,
                "Handshake failed, attempting token refresh and retry"
            );
            let refresh_ok = {
                let mgr = auth_mgr.lock().await;
                mgr.refresh_token().await.is_ok()
            };
            if refresh_ok {
                let retry_transport = PendingTransport::HttpAuth {
                    config: config.clone(),
                    auth_manager: auth_mgr.clone(),
                };
                result = tokio::select! {
                    biased;
                    _ = self.shutdown_token.cancelled() => Err(McpError::ClientError(format!(
                        "MCP client {} was shut down during initialization",
                        self.server_name,
                    ))),
                    result = self.try_handshake(retry_transport) => result,
                };
            }
        }

        // Disarm before publishing the result so the drop guard
        // doesn't double-restore on the success path or fight with
        // the failure-path assignment below.
        init_guard.disarm();

        // Snapshot the event sender before we commit Ready/Pending/Empty
        // under the lock. We want to emit `HandshakeFailed` (on `Err`) or
        // signal the dispatcher to set status=ready (on `Ok`) AFTER
        // releasing the state lock, so a `state.lock().await` inside the
        // dispatcher (should one ever exist — none today) can't deadlock.
        //
        // The snapshot reads through the SHARED `Arc<Mutex<...>>`
        // slot. If the per-server task wired [`Self::set_event_tx`]
        // BEFORE invoking `get_tool_registrations` (the pattern in
        // `acp_session.rs`), this snapshot picks up the sender even
        // for the very first handshake.
        let event_tx = self.event_tx_clone();

        let outcome = {
            let mut guard = self.state.lock().await;
            if self.shutdown_token.is_cancelled() {
                // `shutdown_transport` already installed Empty. Do not let a
                // handshake that won a close race resurrect the transport.
                *guard = ClientState::Empty;
                if let Ok(service) = result {
                    drop(service);
                }
                Err(McpError::ClientError(format!(
                    "MCP client {} has been shut down",
                    self.server_name,
                )))
            } else {
                match result {
                    Ok(service) => {
                        let service = Arc::new(service);
                        *guard = ClientState::Ready {
                            service: service.clone(),
                            _connected: echo_agent_telemetry::activity::MCP_SERVERS_CONNECTED
                                .enter(),
                        };
                        tracing::info!(
                            server = %self.server_name,
                            "MCP server initialized successfully"
                        );
                        Ok(service)
                    }
                    Err(e) => {
                        *guard = match restore {
                            Some(transport) => ClientState::Pending(transport),
                            None => ClientState::Empty,
                        };
                        tracing::warn!(
                            server = %self.server_name,
                            error = %e,
                            "MCP server init failed"
                        );
                        Err(e)
                    }
                }
            }
        };
        // Wake parked callers AFTER releasing the state lock so they
        // observe the freshly-stored Ready/Pending/Empty value.
        self.init_done.notify_waiters();

        // Notify the session-actor StatusDispatcher of the handshake
        // outcome, AFTER releasing the state lock. Best-effort: if the
        // receiver is gone (dispatcher torn down, subagent without
        // wiring) the send fails silently. The dispatcher is the only
        // path that turns these into ACP pushes — see the
        // `client_event_tx` field on `McpState`.
        if let Some(tx) = &event_tx {
            match &outcome {
                Ok(_) => {
                    let _ = tx.send(McpClientEvent::Ready {
                        server: self.server_name.clone(),
                    });
                }
                Err(e) => {
                    let _ = tx.send(McpClientEvent::HandshakeFailed {
                        server: self.server_name.clone(),
                        reason: e.to_string(),
                    });
                }
            }
        }
        outcome
    }

    /// Run the MCP handshake (no lock held).
    async fn try_handshake(
        &self,
        pending: PendingTransport,
    ) -> Result<rmcp::service::RunningService<RoleClient, EchoAgentClientHandler>, McpError> {
        let timeout = std::time::Duration::from_secs(self.startup_timeout_sec);
        let name = &self.server_name;

        match pending {
            PendingTransport::Stdio(process) => {
                let handler = self.make_client_handler();
                tokio::time::timeout(timeout, handler.serve(*process))
                    .await
                    .map_err(|_| McpError::timeout(name, timeout))?
                    .map_err(|e| McpError::HandshakeFailed {
                        server: name.to_string(),
                        source: Box::new(e),
                    })
            }
            PendingTransport::Http(config) => {
                let transport =
                    Self::build_http_transport(&config, name, self.warn_budget.clone())?;
                let handler = self.make_client_handler();
                tokio::time::timeout(timeout, handler.serve(transport))
                    .await
                    .map_err(|_| McpError::timeout(name, timeout))?
                    .map_err(|e| McpError::HandshakeFailed {
                        server: name.to_string(),
                        source: Box::new(e),
                    })
            }
            PendingTransport::HttpAuth {
                config,
                auth_manager,
            } => {
                // Authorization is injected per-request by `AuthClient`, never
                // carried in `default_headers`.
                let mut headers = parse_config_headers(
                    name,
                    "oauth-transport",
                    config
                        .headers
                        .iter()
                        .filter(|(key, _)| !key.eq_ignore_ascii_case("Authorization"))
                        .map(|(key, value)| (key.as_str(), value.as_str())),
                );
                apply_user_agent_policy(&mut headers, name, &config.url);
                // reqwest 0.13; the policy chokepoint is typed for 0.12 and cannot wrap this builder.
                #[allow(clippy::disallowed_methods)]
                let http_client = with_extra_root_certificates(
                    reqwest::Client::builder().default_headers(headers),
                )
                .build()
                .map_err(|e| McpError::ClientError(format!("Failed to build HTTP client: {e}")))?;
                // `AuthClient::new` wants an owned manager, but ours is shared
                // (`Arc`) with the OAuth flow; the struct is non_exhaustive, so
                // build with a throwaway manager and swap in the shared one.
                let placeholder_manager =
                    rmcp::transport::auth::AuthorizationManager::new(config.url.as_str())
                        .await
                        .map_err(|e| {
                            McpError::ClientError(format!("Failed to build OAuth client: {e}"))
                        })?;
                let mut auth_client =
                    rmcp::transport::auth::AuthClient::new(http_client, placeholder_manager);
                auth_client.auth_manager = auth_manager.clone();
                let mcp_http_client = crate::mcp_http_client::McpHttpClient::new(
                    auth_client,
                    name.as_str(),
                    self.warn_budget.clone(),
                );
                let transport_config =
                    StreamableHttpClientTransportConfig::with_uri(config.url.as_str());
                let transport =
                    StreamableHttpClientTransport::with_client(mcp_http_client, transport_config);
                let handler = self.make_client_handler();
                tokio::time::timeout(timeout, handler.serve(transport))
                    .await
                    .map_err(|_| McpError::timeout(name, timeout))?
                    .map_err(|e| McpError::HandshakeFailed {
                        server: name.to_string(),
                        source: Box::new(e),
                    })
            }
            PendingTransport::Acp { server_id, invoker } => {
                // Per-reverse-call backstop on `echo.agent/mcp/sdk_call`: the larger of the
                // startup and tool timeouts, so it never undercuts the real outer bound
                // (the handshake `initialize` is bounded by the serve `timeout` below;
                // tool calls by `tool_timeout_for` in `try_call_tool`). The bridge
                // forwards raw JSON-RPC without the tool name, so per-TOOL overrides
                // aren't applied here in v1; the HTTP path still honors them.
                let invoke_timeout = std::time::Duration::from_secs(
                    self.startup_timeout_sec.max(self.tool_timeout_sec),
                );
                let transport =
                    crate::acp_transport::acp_bridge_transport(server_id, invoker, invoke_timeout);
                let handler = self.make_client_handler();
                tokio::time::timeout(timeout, handler.serve(transport))
                    .await
                    .map_err(|_| McpError::timeout(name, timeout))?
                    .map_err(|e| McpError::HandshakeFailed {
                        server: name.to_string(),
                        source: Box::new(e),
                    })
            }
        }
    }

    fn make_client_info(server_name: &str, advertise_elicitation: bool) -> ClientInfo {
        use rmcp::model::{
            ElicitationCapability, FormElicitationCapability, UrlElicitationCapability,
        };

        let mut extensions = rmcp::model::ExtensionCapabilities::new();
        extensions.insert(
            "io.modelcontextprotocol/ui".to_string(),
            serde_json::from_value(serde_json::json!({
                "mimeTypes": ["text/html;profile=mcp-app"]
            }))
            .unwrap_or_default(),
        );
        let mut capabilities = ClientCapabilities::default();
        capabilities.extensions = Some(extensions);
        if advertise_elicitation {
            capabilities.elicitation = Some(
                ElicitationCapability::new()
                    .with_form(FormElicitationCapability::new().with_schema_validation(true))
                    .with_url(UrlElicitationCapability::new()),
            );
        }
        ClientInfo::new(
            capabilities,
            Implementation::new(
                format!("echo-agent-shell-{server_name}"),
                echo_agent_version::VERSION.to_string(),
            ),
        )
        // This pin currently equals rmcp 2.1 LATEST, but the explicit setter
        // must remain so a future rmcp bump cannot silently move the wire
        // (including to 2026-07-28).
        .with_protocol_version(rmcp::model::ProtocolVersion::V_2025_11_25)
    }

    /// Build the [`EchoAgentClientHandler`] that drives `client.serve(...)`.
    ///
    /// The handler holds a **clone of `Arc<Mutex<Option<Sender>>>`**,
    /// not a snapshot — so any subsequent call to
    fn make_client_handler(&self) -> EchoAgentClientHandler {
        EchoAgentClientHandler {
            info: Self::make_client_info(
                &self.server_name,
                !self.is_acp() && self.elicitation_tx.lock().is_some(),
            ),
            server_name: self.server_name.clone(),
            notify_tx: Arc::clone(&self.notify_tx),
            elicitation_tx: Arc::clone(&self.elicitation_tx),
        }
    }

    /// Wire a sender for [`McpClientEvent`]s emitted by this client.
    ///
    /// Mutates the shared slot synchronously. All previously-cloned
    /// references (the [`EchoAgentClientHandler`] handed to
    /// `client.serve`, the [`crate::liveness::spawn_transport_liveness`]
    /// task) read through the same Arc, so this is observed
    /// session-wide on the next event.
    pub fn set_event_tx(&self, tx: Option<tokio::sync::mpsc::UnboundedSender<McpClientEvent>>) {
        *self.notify_tx.lock() = tx;
    }

    pub fn set_elicitation_tx(&self, tx: Option<crate::elicitation::ElicitationInbox>) {
        *self.elicitation_tx.lock() = tx;
    }

    /// Snapshot the current event sender, if any.
    ///
    /// Used by [`crate::liveness::spawn_transport_liveness`] (which
    /// captures a `Sender` clone at spawn time) and by
    /// [`Self::ensure_initialized`]'s post-handshake emit. Synchronous
    /// because the shared slot is a `parking_lot::Mutex`.
    pub fn event_tx_clone(&self) -> Option<tokio::sync::mpsc::UnboundedSender<McpClientEvent>> {
        self.notify_tx.lock().clone()
    }

    /// Install or replace this client's transport-liveness handle.
    /// Dropping the previous handle (if any) cancels its task; the
    /// new handle starts polling on its own schedule. Pass `None` to
    /// stop watching without installing a new one.
    ///
    /// Synchronous: the slot is a `parking_lot::Mutex`. The poller
    /// task uses an `Arc` clone of this same mutex so it can clear the
    /// slot from inside the task before exiting.
    pub fn set_liveness_handle(&self, handle: Option<crate::liveness::TransportLivenessHandle>) {
        *self.liveness_handle.lock() = handle;
    }

    /// Active transport teardown path. Drops the rmcp
    /// [`RunningService`] so the underlying transport (and stdio child
    /// process, for [`crate::servers::SafeTokioChildProcess`]) closes
    /// **immediately**, instead of waiting for the
    /// `Arc<McpClient>` count to hit zero and the
    /// [`McpClient`] → [`SafeTokioChildProcess`] Drop chain to fire.
    ///
    /// # Why this exists
    ///
    /// `OwnedClients::clear/remove/insert` previously only cancelled the
    /// liveness watcher and relied on Arc-release for reaping. Any
    /// external `Arc<McpClient>` (e.g. a `SharedMcpPool` snapshot, a
    /// stale liveness task reference, `McpState::shared_clients`)
    /// kept the count > 0 and orphaned the stdio child permanently —
    /// which is exactly the symptom observed in production where every
    /// new conversation left another `npx`/`node` process in Task
    /// Manager.
    ///
    /// # Idempotency
    ///
    /// Safe to call multiple times. Subsequent calls observe
    /// [`ClientState::Empty`] and return without effect.
    ///
    /// # Sync safety
    ///
    /// Takes the state lock briefly to swap `Ready → Empty`, then drops
    /// the service *outside* the lock. The drop runs
    /// [`SafeTokioChildProcess::drop`] which SIGKILLs the process group
    /// synchronously.
    pub async fn shutdown_transport(&self) {
        // Cancel first so an Initializing holder drops its `try_handshake`
        // future (and therefore its owned stdio transport) while we wait for
        // the state lock. CancellationToken is sticky, so no later caller can
        // restart this evicted client.
        self.shutdown_token.cancel();
        self.init_done.notify_waiters();
        // Atomically swap `state` to `Empty` and pull out whatever was
        // holding the live transport. The lock is released BEFORE the
        // drop so any transport-Close callbacks (Stdio::drop spawns a
        // kill task, Http may issue cancel RPCs) run without holding
        // the state mutex.
        //
        // Covers all three "has a live stdio child" cases:
        // 1. `Ready` — the rmcp RunningService holds the live transport
        //    (passed through by `client.serve` during handshake).
        //    Dropping the Arc tears down the service loop, which drops
        //    the transport and fires `kill_process_group` +
        //    `child.kill()` synchronously.
        // 2. `Pending(Stdio(_))` — the only state where a fresh,
        //    not-yet-handedshake stdio child is alive. The handshake
        //    consumes the transport out of Pending on its way to Ready,
        //    so until then the live PID lives here. Dropping the boxed
        //    `SafeTokioChildProcess` reaps the process group.
        //    (Non-Stdio Pending variants hold cheap config + Arc auth
        //    managers; no live PIDs to reap, so the drop is a no-op.)
        // 3. `Initializing` — the cancellation token above interrupts the
        //    handshake future, dropping its owned transport. Its publication
        //    path observes the sticky cancellation and preserves `Empty`.
        let prev = {
            let mut guard = self.state.lock().await;
            std::mem::replace(&mut *guard, ClientState::Empty)
        };
        match prev {
            ClientState::Ready { service, .. } => drop(service),
            ClientState::Pending(pending) => drop(pending),
            ClientState::Initializing | ClientState::Empty => {}
        }
    }

    /// Arm the per-client transport-liveness watcher.
    ///
    /// Idempotent and gated:
    /// - Returns `false` for in-process SDK ([`Self::is_acp`]) clients: the
    ///   watcher's only output is `TransportClosed`, which the dispatcher can't
    ///   recover for ACP (not in `configs`), so it would evict the client. ACP
    ///   recovers lazily via [`Self::reset_transport`] instead. Gated here so no
    ///   caller can forget it.
    /// - Returns `false` if there's no `notify_tx` wired (subagent
    ///   snapshot or pre-dispatcher state) — nothing to do.
    /// - Returns `false` if the client isn't `Ready` — armed pollers
    ///   would just exit silently on their first poll, but skipping
    ///   the spawn entirely is cheaper.
    /// - Returns `false` if a live handle is already installed.
    /// - Otherwise spawns the poller and stores the handle.
    ///
    /// **TOCTOU note**: the state check is performed before the
    /// liveness lock is acquired. A concurrent re-handshake could move
    /// the state to `Initializing` between the check and the spawn.
    /// This is benign — the poller's first tick observes the
    /// non-`Ready` state and exits silently without emitting. So the
    /// worst case under TOCTOU is "the poller starts and immediately
    /// stops"; it never produces a spurious `TransportClosed`.
    ///
    /// Lifecycle: when the watcher emits `TransportClosed` it clears
    /// the slot itself; the next `arm_liveness_watcher` call can
    /// install a fresh handle without a manual
    /// [`Self::set_liveness_handle`] reset.
    pub async fn arm_liveness_watcher(
        self: &Arc<Self>,
        poll_interval: std::time::Duration,
    ) -> bool {
        if self.is_acp() {
            return false;
        }
        let Some(event_tx) = self.event_tx_clone() else {
            return false;
        };
        if !matches!(self.state_kind().await, ClientStateKind::Ready) {
            return false;
        }
        let mut slot = self.liveness_handle.lock();
        if slot.is_some() {
            return false;
        }
        let handle = crate::liveness::spawn_transport_liveness(
            self.server_name.clone(),
            Arc::clone(self),
            poll_interval,
            event_tx,
            Arc::clone(&self.liveness_handle),
        );
        *slot = Some(handle);
        true
    }

    fn build_http_transport(
        config: &HttpConfig,
        server_name: &str,
        warn_budget: crate::mcp_http_client::WarnBudget,
    ) -> Result<
        StreamableHttpClientTransport<crate::mcp_http_client::McpHttpClient<reqwest::Client>>,
        McpError,
    > {
        let mut headers = parse_config_headers(
            server_name,
            "transport",
            config
                .headers
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
        apply_user_agent_policy(&mut headers, server_name, &config.url);
        // reqwest 0.13; the policy chokepoint is typed for 0.12 and cannot wrap this builder.
        #[allow(clippy::disallowed_methods)]
        let client =
            with_extra_root_certificates(reqwest::Client::builder().default_headers(headers))
                .build()
                .map_err(|e| McpError::ClientError(format!("Failed to build HTTP client: {e}")))?;
        let mcp_http_client =
            crate::mcp_http_client::McpHttpClient::new(client, server_name, warn_budget);
        let transport_config = StreamableHttpClientTransportConfig::with_uri(config.url.as_str());
        Ok(StreamableHttpClientTransport::with_client(
            mcp_http_client,
            transport_config,
        ))
    }

    /// Cheap, non-blocking liveness predicate.
    ///
    /// Inspects the current [`ClientState`] under the state mutex only —
    /// it MUST NOT call [`Self::ensure_initialized`] or any other path
    /// that can trigger a network round-trip. The previous implementation
    /// went through `ensure_initialized`, which could block UI callers
    /// (e.g. an MCP status modal) for up to `startup_timeout_sec` seconds
    /// on a dead stdio server.
    ///
    /// Semantics:
    /// - `Ready(service)` with an open transport → `true`.
    /// - `Ready(service)` whose receiver-side has been dropped (typically
    ///   because the rmcp service loop terminated) → `false`. rmcp 2.1
    ///   `Peer::is_transport_closed` reports `self.tx.is_closed()` at
    ///   `service.rs:703-705`; `RunningService` derefs to `Peer` at
    ///   `service.rs:716-722`.
    /// - Any other variant (`Empty`, `Pending`, `Initializing`) →
    ///   `false`.
    ///
    /// HTTP idle caveat: for [`StreamableHttpClientTransport`] the rmcp
    /// service loop only terminates on an outgoing send failure or an
    /// explicit shutdown. A long-idle HTTP server therefore keeps
    /// `is_transport_closed()` returning `false`, and this method
    /// continues to report `true`. That is the desired semantics — a
    /// liveness probe would belong in a separate watcher, not here.
    pub async fn is_healthy(&self) -> bool {
        let guard = self.state.lock().await;
        match &*guard {
            ClientState::Ready { service, .. } => !service.is_transport_closed(),
            _ => false,
        }
    }

    /// Atomic classification for the liveness watcher.
    ///
    /// Reads `state` once and projects onto
    /// [`LivenessCheck`]: distinguishes "transport actually closed"
    /// (emit + exit) from "state moved out of `Ready`" (exit
    /// silently). The watcher depends on this distinction: a plain
    /// `is_healthy`-based predicate cannot tell the cases apart and
    /// would false-fire `TransportClosed` on re-handshake transitions.
    pub async fn liveness_check(&self) -> LivenessCheck {
        let guard = self.state.lock().await;
        match &*guard {
            ClientState::Ready { service, .. } => {
                if service.is_transport_closed() {
                    LivenessCheck::TransportClosed
                } else {
                    LivenessCheck::Healthy
                }
            }
            _ => LivenessCheck::Transient,
        }
    }

    /// State-machine snapshot for diagnostics and downstream UI.
    ///
    /// Like [`Self::is_healthy`], this is a cheap state inspection
    /// (no handshake, no network I/O). Maps [`ClientState`] onto a
    /// `Copy` enum so callers can match without holding a reference to
    /// the inner [`McpService`] / [`PendingTransport`].
    pub async fn state_kind(&self) -> ClientStateKind {
        let guard = self.state.lock().await;
        match &*guard {
            ClientState::Empty => ClientStateKind::Empty,
            ClientState::Pending(_) => ClientStateKind::Pending,
            ClientState::Initializing => ClientStateKind::Initializing,
            ClientState::Ready { .. } => ClientStateKind::Ready,
        }
    }

    /// Materialize this server's tool descriptors as JSON files under
    /// `<server_dir>/tools/`.
    ///
    /// The model reads these before issuing an MCP tool call.
    /// Each tool becomes `<server_dir>/tools/<sanitized_tool_name>.json`
    /// with `{name, description, inputSchema}`. Resources are intentionally
    /// not materialized: this harness exposes only MCP tool calls, so resource
    /// descriptors would advertise MCP-resource tools the model can't use.
    ///
    /// Best-effort: errors writing individual descriptors are logged but
    /// don't abort the materialization. Returns the number of files written.
    pub async fn materialize_descriptors(
        &self,
        server_dir: &std::path::Path,
    ) -> Result<usize, McpError> {
        let mcp_service = self.ensure_initialized().await?;

        // Collect descriptors via the async MCP API, then defer all filesystem
        // work to a single `spawn_blocking` so the executor is never blocked on
        // `std::fs` (this runs on every MCP tool-set change, not just startup).
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let result = mcp_service
                .list_tools(Some(
                    PaginatedRequestParams::default().with_cursor(cursor.clone()),
                ))
                .await?;
            for tool in result.tools {
                let descriptor = serde_json::json!({
                    "name": tool.name.as_ref(),
                    "description": tool.description.as_deref(),
                    "inputSchema": tool.input_schema.as_ref(),
                });
                match serde_json::to_vec_pretty(&descriptor) {
                    Ok(bytes) => files.push((
                        format!("{}.json", sanitize_descriptor_segment(tool.name.as_ref())),
                        bytes,
                    )),
                    Err(e) => tracing::warn!(
                        tool = %tool.name.as_ref(),
                        error = %e,
                        "failed to serialize MCP tool descriptor",
                    ),
                }
            }
            match result.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        // Write each descriptor atomically (temp file + rename) so a concurrent
        // reader never sees a half-written JSON and overlapping writers converge
        // without a lock.
        let tools_dir = server_dir.join("tools");
        tokio::task::spawn_blocking(move || -> Result<usize, McpError> {
            std::fs::create_dir_all(&tools_dir).map_err(|e| {
                McpError::ClientError(format!(
                    "failed to create MCP tools descriptor dir {}: {e}",
                    tools_dir.display()
                ))
            })?;
            let mut written = 0usize;
            for (file_name, bytes) in files {
                let path = tools_dir.join(&file_name);
                let write_result =
                    tempfile::NamedTempFile::new_in(&tools_dir).and_then(|mut tmp| {
                        std::io::Write::write_all(&mut tmp, &bytes)?;
                        tmp.persist(&path).map_err(|e| e.error)
                    });
                match write_result {
                    Ok(_) => written += 1,
                    Err(e) => tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to write MCP tool descriptor",
                    ),
                }
            }
            Ok(written)
        })
        .await
        .map_err(|e| McpError::ClientError(format!("descriptor write task panicked: {e}")))?
    }

    /// Read the server's `instructions` from the MCP initialize handshake.
    /// Returns `None` if the client isn't ready yet.
    pub async fn server_instructions(&self) -> Option<String> {
        let guard = self.state.lock().await;
        if let ClientState::Ready { service, .. } = &*guard {
            service
                .peer_info()?
                .instructions
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(String::from)
        } else {
            None
        }
    }

    // Server icons stay on peer_info (handshake) and are re-read here; tool
    // icons are snapshotted into McpState at registration because tools/list
    // is not re-fetched for every status build.
    pub async fn server_icons(&self) -> Vec<McpIcon> {
        let guard = self.state.lock().await;
        match &*guard {
            ClientState::Ready { service, .. } => service
                .peer_info()
                .map(|info| McpIcon::from_rmcp_list(info.server_info.icons.clone()))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    pub async fn get_tool_registrations(
        &self,
        mcp_state: Arc<Mutex<McpState>>,
    ) -> Result<Vec<McpToolRegistration>, McpError> {
        let _ensure_init_timer =
            echo_agent_telemetry::instrumentation::timer("mcp_ensure_initialized");
        let mcp_service = self.ensure_initialized().await?;

        let mut all_tools = Vec::new();
        let mut cursor: Option<String> = None;

        let _list_tools_timer = echo_agent_telemetry::instrumentation::timer("mcp_list_tools");
        loop {
            let list_tools_result = mcp_service
                .list_tools(Some(
                    PaginatedRequestParams::default().with_cursor(cursor.clone()),
                ))
                .await?;

            all_tools.extend(list_tools_result.tools);

            match list_tools_result.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        let registrations: Vec<_> = all_tools
            .into_iter()
            .filter_map(|tool| {
                let meta: Option<serde_json::Value> = tool
                    .meta
                    .as_ref()
                    .and_then(|m| serde_json::to_value(m).ok());

                let name = tool.name.to_string();
                let description = tool.description.map(|d| d.to_string()).unwrap_or_default();
                let mut schema = serde_json::to_value(tool.input_schema.as_ref())
                    .unwrap_or_else(|_| serde_json::json!({"type": "object"}));
                // Ensure the schema has "type": "object" — some MCP servers
                // (e.g., VSCode) send `inputSchema: {}` for tools with no
                // parameters. Azure's OpenAI API rejects schemas without a
                // `type` field with: 'schema must be a JSON Schema of type:
                // "object", got type: "None"'.
                if let Some(obj) = schema.as_object_mut() {
                    obj.entry("type")
                        .or_insert_with(|| serde_json::json!("object"));
                    obj.entry("properties")
                        .or_insert_with(|| serde_json::json!({}));
                }

                let icons = McpIcon::from_rmcp_list(tool.icons);
                let mcp_tool = McpTool {
                    name,
                    description,
                    server_name: self.server_name.clone(),
                    mcp_state: Arc::clone(&mcp_state),
                    schema,
                    meta,
                };
                // Invalid tools (bad names) return None and are skipped
                mcp_tool.into_registration().map(|mut reg| {
                    reg.icons = icons;
                    reg
                })
            })
            .collect();

        // Warn about tool_timeouts keys that don't match any discovered tool.
        // This catches typos like `creat_issue` instead of `create_issue`.
        if !self.tool_timeouts.is_empty() {
            // Registration names are qualified ("server__tool"); tool_timeouts
            // keys are raw tool names. Strip the server prefix for comparison.
            let prefix = format!("{}{}", self.server_name, MCP_TOOL_NAME_DELIMITER);
            let raw_names: Vec<&str> = registrations
                .iter()
                .map(|r| r.name.strip_prefix(prefix.as_str()).unwrap_or(&r.name))
                .collect();
            let discovered: std::collections::HashSet<&str> = raw_names.iter().copied().collect();
            for key in self.tool_timeouts.keys() {
                if !discovered.contains(key.as_str()) {
                    tracing::info!(
                        server = %self.server_name,
                        tool_timeout_key = %key,
                        "tool_timeouts entry '{}' does not match any tool exposed by MCP server '{}' \
                         (available: {}). The per-tool timeout will have no effect — check for typos.",
                        key,
                        self.server_name,
                        raw_names.join(", "),
                    );
                }
            }
        }

        Ok(registrations)
    }

    /// Call a tool directly on the MCP server (for testing/debugging).
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<rmcp::model::CallToolResult, McpError> {
        let mcp_service = self.ensure_initialized().await?;
        let result = mcp_service
            .call_tool({
                let mut params = CallToolRequestParams::new(tool_name.to_string());
                params.arguments = arguments.as_object().cloned();
                params
            })
            .await?;
        Ok(result)
    }
}

fn contains_session_placeholder(value: &str) -> bool {
    value.contains("{{session_id}}") || value.contains("${session_id}")
}

/// Sanitize an MCP server name into a safe filename component.
fn sanitize_mcp_log_filename(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .take(96)
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() => c,
            '.' | '_' | '-' => c,
            _ => '_',
        })
        .collect();
    if sanitized.is_empty() {
        "server".into()
    } else {
        sanitized
    }
}

/// Copy an MCP server's stderr to `~/.echo-agent/logs/mcp/<server>.stderr.log`
/// in a background task. Truncated per spawn.
fn drain_mcp_stderr_to_log(server_name: &str, mut stderr: tokio::process::ChildStderr) {
    let log_dir = echo_agent_config::echo_agent_home()
        .join("logs")
        .join("mcp");
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        tracing::warn!("MCP stderr drain: failed to create log dir: {e}");
        return;
    }
    let log_path = log_dir.join(format!(
        "{}.stderr.log",
        sanitize_mcp_log_filename(server_name)
    ));

    let file = match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                "MCP stderr drain: failed to open {}: {e}",
                log_path.display()
            );
            return;
        }
    };
    let mut file = tokio::fs::File::from_std(file);
    let server_name = server_name.to_string();
    tokio::spawn(async move {
        if let Err(e) = tokio::io::copy(&mut stderr, &mut file).await {
            tracing::warn!("MCP stderr drain '{server_name}': {e}");
        }
    });
}

fn expand_session_id_headers(
    headers: Vec<acp::HttpHeader>,
    session_id: Option<&str>,
) -> Vec<(String, String)> {
    headers
        .into_iter()
        .filter_map(|header| {
            let value = header.value;
            if let Some(session_id) = session_id {
                let expanded = value
                    .replace("{{session_id}}", session_id)
                    .replace("${session_id}", session_id);
                Some((header.name, expanded))
            } else if contains_session_placeholder(&value) {
                None
            } else {
                Some((header.name, value))
            }
        })
        .collect()
}

/// Decide the actual (program, args) to spawn for a stdio MCP server.
///
/// On Windows, npm ships launchers like `npx`/`npm`/`pnpm`/`yarn` as `.cmd`
/// batch shims (there is no `npx.exe`). `CreateProcessW` only appends `.exe`
/// and ignores `PATHEXT`, so `Command::new("npx")` fails with "file not
/// found". We resolve the bare name on `PATH` (honoring `PATHEXT`, via the
/// `resolve` closure) so std spawns the real launcher path (e.g. `npx.cmd`) —
/// std then runs `.cmd`/`.bat` through `cmd.exe` with hardened arg escaping. On
/// non-Windows we never touch the command (verified working). A command
/// containing a path separator is used as-is. The resolved path is returned as
/// an `OsString` so it reaches `Command::new` without a lossy UTF-8 round-trip.
fn plan_stdio_spawn(
    command: &str,
    args: &[String],
    is_windows: bool,
    resolve: impl Fn(&str) -> Option<std::path::PathBuf>,
) -> (OsString, Vec<String>) {
    if is_windows
        && !command.contains('/')
        && !command.contains('\\')
        && let Some(resolved) = resolve(command)
    {
        return (resolved.into_os_string(), args.to_vec());
    }
    (OsString::from(command), args.to_vec())
}

fn is_figma_mcp(server_name: &str, url: &str) -> bool {
    if server_name.eq_ignore_ascii_case("figma") {
        return true;
    }
    // Legacy direct managed name (`echo_agent_com_figma`); newer clients use gateway tools (`managed_mcp_gateway_tools_enabled`).
    const MANAGED_PREFIX: &str = "echo_agent_com_";
    if let (Some(prefix), Some(rest)) = (
        server_name.get(..MANAGED_PREFIX.len()),
        server_name.get(MANAGED_PREFIX.len()..),
    ) && prefix.eq_ignore_ascii_case(MANAGED_PREFIX)
        && rest.eq_ignore_ascii_case("figma")
    {
        return true;
    }
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .is_some_and(|h| h == "figma.com" || h.ends_with(".figma.com"))
}

fn ensure_figma_user_agent(headers: &mut reqwest::header::HeaderMap, server_name: &str, url: &str) {
    if !is_figma_mcp(server_name, url) {
        return;
    }
    if headers.contains_key(reqwest::header::USER_AGENT) {
        return;
    }
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static("echo-agent-cli"),
    );
}

static DEFAULT_USER_AGENT: LazyLock<reqwest::header::HeaderValue> = LazyLock::new(|| {
    format!("echo-agent-cli/{}", echo_agent_version::VERSION)
        .parse()
        .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("echo-agent-cli"))
});

fn ensure_default_user_agent(headers: &mut reqwest::header::HeaderMap) {
    if headers.contains_key(reqwest::header::USER_AGENT) {
        return;
    }
    headers.insert(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT.clone());
}

/// Figma keeps its pinned bare `echo-agent-cli` attribution token; every other server
/// gets the versioned default. A `User-Agent` already in the map always wins.
fn apply_user_agent_policy(headers: &mut reqwest::header::HeaderMap, server_name: &str, url: &str) {
    ensure_figma_user_agent(headers, server_name, url);
    ensure_default_user_agent(headers);
}

/// Configured header pairs → `HeaderMap` for every MCP streamable-HTTP request
/// path. Invalid pairs are warned and skipped; for duplicate names the last
/// valid value wins (`HeaderMap::insert`). `stage` disambiguates the warning:
/// one bad configured pair is reported by both the anonymous probe and the
/// transport build on a single connection attempt.
fn parse_config_headers<'a>(
    server_name: &str,
    stage: &'static str,
    pairs: impl Iterator<Item = (&'a str, &'a str)>,
) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    for (key, value) in pairs {
        match (
            reqwest::header::HeaderName::from_bytes(key.as_bytes()),
            value.parse::<reqwest::header::HeaderValue>(),
        ) {
            (Ok(name), Ok(val)) => {
                headers.insert(name, val);
            }
            _ => {
                tracing::warn!(
                    server = server_name,
                    stage,
                    "Skipping invalid MCP HTTP header: {key}"
                );
            }
        }
    }
    headers
}

fn stdio_path_override(env: &[acp::EnvVariable]) -> Option<&str> {
    env.iter()
        .find(|e| e.name.eq_ignore_ascii_case("PATH"))
        .map(|e| e.value.as_str())
}

fn apply_stdio_env(cmd: &mut Command, env: &[acp::EnvVariable], session_id: Option<&str>) {
    for env_variable in env {
        cmd.env(&env_variable.name, &env_variable.value);
    }
    if let Some(session_id) = session_id {
        cmd.env("ECHO_AGENT_SESSION_ID", session_id);
    }
}

/// Borrowed cross-cutting spawn context whose `scope`, when set, enrolls the stdio child for session-close reaping.
pub struct McpSpawnCtx<'a> {
    pub(crate) session_id: Option<&'a str>,
    pub(crate) event_writer: &'a echo_agent_session_events::EventWriter,
    pub(crate) mode: OauthInteractivity,
    pub(crate) scope: Option<&'a ProcessScope>,
}

impl<'a> McpSpawnCtx<'a> {
    pub fn for_session(
        session_id: &'a str,
        event_writer: &'a echo_agent_session_events::EventWriter,
        mode: OauthInteractivity,
        scope: Option<&'a ProcessScope>,
    ) -> Self {
        Self {
            session_id: Some(session_id),
            event_writer,
            mode,
            scope,
        }
    }

    pub fn session_less(event_writer: &'a echo_agent_session_events::EventWriter) -> Self {
        Self {
            session_id: None,
            event_writer,
            mode: OauthInteractivity::Interactive,
            scope: None,
        }
    }
}

pub async fn start_mcp_server(
    mcp_server: acp::McpServer,
    overrides: Option<&McpClientTimeoutOverrides>,
    meta_config: Option<&McpServerMetaConfig>,
    byo_config: Option<&McpOAuthConfig>,
    ctx: &McpSpawnCtx<'_>,
) -> Result<McpClient, McpError> {
    let _per_server_timer = echo_agent_telemetry::instrumentation::timer("mcp_start_one_server");
    match mcp_server {
        acp::McpServer::Stdio(acp::McpServerStdio {
            name,
            command,
            args,
            env,
            ..
        }) => {
            if let Some(mc) = meta_config {
                tracing::info!(server = %name, ?mc, "MCP stdio: meta config override");
            }

            let (startup_timeout, _, _) = McpClient::load_timeouts(overrides, meta_config);
            let command_str = command.to_string_lossy().into_owned();
            let spawn_start = std::time::Instant::now();
            let _stdio_spawn_timer =
                echo_agent_telemetry::instrumentation::timer("mcp_stdio_spawn");
            let path_override = stdio_path_override(&env);
            let (program, spawn_args) = plan_stdio_spawn(&command_str, &args, cfg!(windows), |c| {
                if let Some(path) = path_override
                    && let Ok(cwd) = std::env::current_dir()
                {
                    which::which_in(c, Some(path), cwd).ok()
                } else {
                    which::which(c).ok()
                }
            });
            let mut cmd = Command::new(&program);
            cmd.kill_on_drop(true).args(&spawn_args);
            apply_stdio_env(&mut cmd, &env, ctx.session_id);
            echo_agent_tools::util::detach_command(&mut cmd);

            let (transport, stderr_handle) = SafeTokioChildProcess::spawn(
                cmd,
                ctx.scope,
                name.clone(),
                ctx.event_writer.clone(),
            )
            .await
            .map_err(|e| {
                tracing::error!("Failed to spawn MCP server '{}': {}", name, e);
                echo_agent_telemetry::session_ctx::log_event(
                    echo_agent_telemetry::events::McpServerFailed {
                        server_name: name.clone(),
                        error_type: echo_agent_telemetry::events::McpErrorType::SpawnFailed,
                        duration_ms: spawn_start.elapsed().as_millis() as u64,
                        timeout_sec: startup_timeout,
                    },
                );
                McpError::SpawnFailed {
                    server: name.clone(),
                    source: e,
                }
            })?;

            tracing::debug!("MCP server '{}' spawned: PID={:?}", name, transport.id());

            if let Some(stderr) = stderr_handle {
                drain_mcp_stderr_to_log(&name, stderr);
            }

            Ok(McpClient::new_stdio(
                name.clone(),
                transport,
                overrides,
                meta_config,
            ))
        }
        acp::McpServer::Http(acp::McpServerHttp {
            name, url, headers, ..
        })
        | acp::McpServer::Sse(acp::McpServerSse {
            name, url, headers, ..
        }) => {
            if let Some(mc) = meta_config {
                tracing::info!(server = %name, %url, ?mc, "MCP http: meta config override");
            }

            let headers = expand_session_id_headers(headers, ctx.session_id);
            let http_config = HttpConfig {
                url: url.clone(),
                headers,
            };

            let has_existing_auth = http_config
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("authorization"));

            let auth_prep = if has_existing_auth {
                tracing::debug!(
                    server = %name,
                    "Skipping OAuth discovery: server already has Authorization header"
                );
                HttpOauthPrep::NoOauthSupport
            } else {
                resolve_http_oauth_prep(
                    &name,
                    &url,
                    &http_config.headers,
                    ctx,
                    OAUTH_DISCOVERY_TIMEOUT,
                )
                .await
            };
            match auth_prep {
                HttpOauthPrep::ManagerReady(auth_mgr) => Ok(McpClient::new_http_auth(
                    name.clone(),
                    http_config,
                    auth_mgr,
                    byo_config.cloned(),
                    overrides,
                    meta_config,
                )),
                HttpOauthPrep::NoOauthSupport => Ok(McpClient::new_http(
                    name.clone(),
                    http_config,
                    overrides,
                    meta_config,
                )),
                // Avoid starting an unauthenticated HTTP worker that fatals on server OAuth challenge.
                HttpOauthPrep::NeedsInteractiveLogin => Err(McpError::AuthRequired {
                    server: name.clone(),
                }),
            }
        }
        // TODO(acp-0.10): `McpServer` is #[non_exhaustive]; reject unknown transports.
        other => Err(McpError::ClientError(format!(
            "unsupported MCP server transport: {other:?}"
        ))),
    }
}

pub async fn start_mcp_servers(
    mcp_servers: Vec<acp::McpServer>,
    overrides_map: &HashMap<String, McpClientTimeoutOverrides>,
    meta_config_map: &McpMetaConfigMap,
    oauth_config_map: &crate::oauth_config::McpOAuthConfigMap,
    ctx: &McpSpawnCtx<'_>,
) -> Vec<Result<McpClient, McpError>> {
    let _mcp_start_timer = echo_agent_telemetry::instrumentation::timer("mcp_start_servers");

    if !meta_config_map.is_empty() {
        tracing::info!(
            count = mcp_servers.len(),
            overrides = ?meta_config_map.keys().collect::<Vec<_>>(),
            "Starting MCP servers with meta config"
        );
    }

    futures::stream::iter(mcp_servers)
        .map(|server| {
            let server_name = mcp_server_name(&server);
            let overrides = overrides_map.get(server_name);
            let mc = meta_config_map.get(server_name);
            let byo = oauth_config_map.get(server_name);
            start_mcp_server(server, overrides, mc, byo, ctx)
        })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await
}

/// Extract the name from an MCP server enum variant.
pub fn mcp_server_name(server: &acp::McpServer) -> &str {
    match server {
        acp::McpServer::Stdio(stdio) => &stdio.name,
        acp::McpServer::Http(http) => &http.name,
        acp::McpServer::Sse(sse) => &sse.name,
        // TODO(acp-0.10): `McpServer` is #[non_exhaustive].
        _ => "unknown",
    }
}

pub fn mcp_transport_str(server: &acp::McpServer) -> &'static str {
    match server {
        acp::McpServer::Stdio(_) => "stdio",
        acp::McpServer::Http(_) => "http",
        acp::McpServer::Sse(_) => "sse",
        // TODO(acp-0.10): `McpServer` is #[non_exhaustive].
        _ => "unknown",
    }
}

pub fn mcp_target_str(server: &acp::McpServer) -> String {
    match server {
        acp::McpServer::Stdio(acp::McpServerStdio { command, args, .. }) => {
            let cmd = command.to_string_lossy();
            if args.is_empty() {
                cmd.to_string()
            } else {
                format!("{} {}", cmd, args.join(" "))
            }
        }
        acp::McpServer::Http(acp::McpServerHttp { url, .. })
        | acp::McpServer::Sse(acp::McpServerSse { url, .. }) => url.clone(),
        // TODO(acp-0.10): `McpServer` is #[non_exhaustive].
        _ => String::new(),
    }
}

impl McpClient {
    /// Minimal stub for unit tests in dependent crates. Hidden from rustdoc;
    /// not gated behind `#[cfg(test)]` so cross-crate test code can construct
    /// it without the host crate enabling a feature.
    #[doc(hidden)]
    pub fn stub(name: &str) -> Self {
        // Route through the single constructor (so new fields never need
        // touching here), then downgrade to the no-transport placeholder:
        // `Empty` state makes `ensure_initialized` error, and `reconnect =
        // None` makes `reset_transport` return false — i.e. a client that
        // can't reconnect, like a dead Stdio child. Overrides preserve the
        // historical stub timeouts (10s startup / 60s tool).
        let overrides = McpClientTimeoutOverrides {
            startup_timeout_sec: Some(10),
            tool_timeout_sec: Some(60),
            ..Default::default()
        };
        let mut client = Self::new_with_transport(
            name.to_string(),
            PendingTransport::Http(HttpConfig {
                url: String::new(),
                headers: Vec::new(),
            }),
            Some(&overrides),
            None,
            None,
            None,
            None,
        );
        *client.state.get_mut() = ClientState::Empty;
        client.reconnect = None;
        client
    }
}

/// rmcp [`ClientHandler`] used by all MCP transports.
///
/// Replaces the previous bare [`ClientInfo`] handler at the three
/// `client.serve(...)` call sites in [`McpClient::try_handshake`].
/// Plumbs server-pushed notifications through an
/// [`tokio::sync::mpsc::UnboundedSender<McpClientEvent>`] so the
/// session-actor dispatcher can fan them out as ACP
/// `echo.agent/mcp/server_status` events.
///
/// ## RPIT, not `#[async_trait]`
///
/// rmcp 2.1's [`ClientHandler`] declares its async methods as
/// return-position `impl Future` (see
/// `~/.cargo/registry/src/.../rmcp-2.1.0/src/handler/client.rs`,
/// lines 202–217). Applying `#[async_trait]` here would produce
/// methods whose signature mismatches the trait, and the impl would
/// not satisfy the bound. The macro path is also unnecessary — the
/// trait already supports `async fn` syntax indirectly via
/// `impl Future<Output = ()> + Send + '_`, which is what we mirror.
///
/// Future contributor reading this: do **not** add `#[async_trait]`.
/// The methods below intentionally return `impl Future` directly.
///
/// ## Notification routing
///
/// `on_tool_list_changed` / `on_resource_list_changed` push an
/// [`McpClientEvent`] into [`Self::notify_tx`]. If the receiver has
/// been dropped (subagent teardown, session shutdown, or the field
/// was `None` to begin with — see [`McpClient::notify_tx`] doc), the
/// send fails silently; rmcp must not see an error from a
/// notification handler or the service loop tears down.
#[derive(Debug)]
pub struct EchoAgentClientHandler {
    /// Static `ClientInfo` returned by [`Self::get_info`]; built once
    /// at handshake time and stored to avoid re-allocating per call.
    info: ClientInfo,
    /// MCP server name this handler is bound to. Cloned into emitted
    /// events so the dispatcher can route per-server.
    server_name: McpServerName,
    /// **Shared** event sink — the same Arc lives on the owning
    /// [`McpClient`]. Mutating the slot via [`McpClient::set_event_tx`]
    /// is observed here on the next read, so wiring the sender
    /// post-handshake is supported without restarting the rmcp
    /// service loop.
    notify_tx: SharedEventTx,
    elicitation_tx: crate::elicitation::SharedElicitationTx,
}

impl EchoAgentClientHandler {
    /// Best-effort event emit. Reads the shared `notify_tx` slot on
    /// every call (so the handler picks up any post-handshake wiring
    /// done by [`McpClient::set_event_tx`]). Drops the send error: if
    /// the receiver is gone, the consumer has shut down and there's
    /// nothing useful to do here. Splitting this out keeps the trait
    /// methods short.
    fn emit(&self, ev: McpClientEvent) {
        let sender = self.notify_tx.lock().clone();
        if let Some(tx) = sender {
            let _ = tx.send(ev);
        }
    }
}

impl ClientHandler for EchoAgentClientHandler {
    // NOTE: `async fn` here is sugar for the trait's
    // `-> impl Future<Output = ()> + Send + '_`. We INTENTIONALLY do
    // not use `#[async_trait]` — rmcp 2.1's `ClientHandler` declares
    // its notification methods as return-position `impl Future`, and
    // async_trait would produce a different (incompatible) signature.
    // See the [`EchoAgentClientHandler`] doc-comment for the full RPIT
    // contract.
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.emit(McpClientEvent::ToolsChanged {
            server: self.server_name.clone(),
        });
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.emit(McpClientEvent::ResourcesChanged {
            server: self.server_name.clone(),
        });
    }

    async fn create_elicitation(
        &self,
        request: rmcp::model::ElicitRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<rmcp::model::ElicitResult, rmcp::ErrorData> {
        tracing::info!(
            server = %self.server_name,
            "MCP elicitation/create received"
        );
        // `context.ct` fires when the server cancels this request
        // (`notifications/cancelled`). Dropping the bridge future closes the
        // job's response channel, which the shell coordinator observes to
        // tear down the HITL card — otherwise the popup would outlive the
        // abandoned request and answer into the void.
        let bridged =
            crate::elicitation::bridge_elicit(&self.elicitation_tx, &self.server_name, request);
        tokio::select! {
            result = bridged => Ok(result),
            _ = context.ct.cancelled() => {
                tracing::info!(
                    server = %self.server_name,
                    "elicitation/create cancelled by server; abandoning HITL bridge"
                );
                Ok(crate::elicitation::cancel_result())
            }
        }
    }

    async fn on_url_elicitation_notification_complete(
        &self,
        params: rmcp::model::ElicitationResponseNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        if !echo_agent_tools::mcp_elicitation::chars_within(
            &params.elicitation_id,
            echo_agent_tools::mcp_elicitation::MAX_ELICIT_ID_CHARS,
        ) {
            tracing::warn!(
                server = %self.server_name,
                "oversized elicitation_id on complete; dropping"
            );
            return;
        }
        self.emit(McpClientEvent::ElicitationComplete {
            server: self.server_name.clone(),
            elicitation_id: params.elicitation_id,
        });
    }

    fn get_info(&self) -> ClientInfo {
        self.info.clone()
    }
}

#[cfg(test)]
#[path = "servers_tests.rs"]
mod tests;
