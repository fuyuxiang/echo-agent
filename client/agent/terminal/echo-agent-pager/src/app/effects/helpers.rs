#![cfg_attr(rustfmt, rustfmt::skip)]
use std::path::Path;
use agent_client_protocol as acp;
use tokio::task::JoinSet;
use echo_agent_acp::{AcpAgentTx, acp_send};
use super::actions::{PermissionModePersist, SubagentKillOutcome, TaskResult};
use super::agent::AgentId;
use crate::unified_log as ulog;
use echo_agent_runtime::sampling::error::{
    RATE_LIMITED_ERROR_CODE, error_detail_from_data, format_rate_limited_user_message,
    http_status_from_error,
};
use echo_agent_runtime::session::ExtMethodResult;
use echo_agent_runtime::session::unified_list::ListScope;
/// Floor for the session create/load RPCs.
const SESSION_RPC_FLOOR: std::time::Duration = std::time::Duration::from_secs(180);
/// Headroom over the agent-side `.envrc` budget for the rest of session setup.
const SESSION_RPC_SLACK: std::time::Duration = std::time::Duration::from_secs(50);
/// Always covers the agent-side `.envrc` budget so the backstop cannot fire
/// before the agent's own deadline. Reads `ECHO_AGENT_ENVRC_TIMEOUT_SECS` in this
/// process; the agent inherits the same environment.
pub(super) fn session_rpc_timeout() -> std::time::Duration {
    SESSION_RPC_FLOOR.max(echo_agent_workspace::envrc::loader_budget() + SESSION_RPC_SLACK)
}
/// `acp_send` bounded by [`session_rpc_timeout`]; on expiry, an error naming
/// `action` instead of an eternal spinner.
pub(super) async fn acp_send_bounded<R, T>(
    request: T,
    tx: &tokio::sync::mpsc::UnboundedSender<R>,
    action: &str,
) -> Result<T::Response, acp::Error>
where
    T: echo_agent_acp::AcpRequest,
    R: From<echo_agent_acp::AcpArgs<T>> + std::fmt::Debug,
{
    let timeout = session_rpc_timeout();
    match tokio::time::timeout(timeout, acp_send(request, tx)).await {
        Ok(result) => result,
        Err(_elapsed) => {
            Err(
                acp::Error::new(
                    acp::ErrorCode::InternalError.into(),
                    format!(
                "{action} timed out after {}s. It may still finish in the background; \
                 retrying right away can run into the same delay.",
                timeout.as_secs()
            ),
                ),
            )
        }
    }
}
/// Typed progress message for session restore.
/// Keeps the progress channel from accepting arbitrary `TaskResult` variants.
pub(crate) struct RestoreProgressMsg {
    pub agent_id: AgentId,
    pub message: String,
}
pub(super) fn log_prompt_result(
    session_id: &acp::SessionId,
    result: &Result<acp::PromptResponse, acp::Error>,
) {
    let sid = &session_id.0;
    match result {
        Ok(_) => ulog::info("agent response complete", Some(sid), None),
        Err(e) => {
            ulog::error(
                "agent response failed",
                Some(sid),
                Some(serde_json::json!({"error": e.to_string()})),
            )
        }
    }
}
/// Delay between post-install MCP-list re-probes (`Effect::RetryPluginCtaMcps`).
pub(super) const CTA_MCP_RETRY_DELAY_MS: u64 = 1000;
/// How long the CTA shows its "installed" confirmation before auto-dismissing.
pub(super) const CTA_INSTALLED_DISMISS_MS: u64 = 4000;
/// Upper bound on the off-thread clipboard-attachment probe. A wedged osascript
/// read must not pin `paste_probe_in_flight` and silently stash every later send.
pub(super) const CLIPBOARD_PROBE_TIMEOUT_SECS: u64 = 10;
/// Picker search debounce ([`Effect::DebounceSessionSearch`]):
/// long enough to coalesce a typing burst, short enough to feel live.
pub(super) const SESSION_SEARCH_DEBOUNCE_MS: u64 = 250;
/// Run the post-CTA-install `echo.agent/mcp/list` read (uncached, which also nudges
/// the shell to retry auth-required servers) and map it into a
/// `TaskResult::PluginCtaMcpsLoaded`. Shared by the immediate fetch and the
/// delayed re-probe.
pub(super) async fn fetch_plugin_cta_mcps(
    agent_id: AgentId,
    session_id: acp::SessionId,
    plugin_name: String,
    tx: AcpAgentTx,
) -> TaskResult {
    let params = serde_json::json!({
        "sessionId": session_id.0.to_string(),
        "cache": false,
    });
    let req = acp::ExtRequest::new(
        "echo.agent/mcp/list",
        serde_json::value::to_raw_value(&params)
            .expect("serialize mcp/list params")
            .into(),
    );
    let result = match acp_send(req, &tx).await {
        Ok(resp) => {
            let wrapper: serde_json::Value = serde_json::from_str(resp.0.get())
                .unwrap_or_default();
            let inner = wrapper.get("result").unwrap_or(&wrapper);
            serde_json::from_value::<
                crate::views::mcps_modal::McpsListResponse,
            >(inner.clone())
                .map(crate::views::mcps_modal::convert_list_response)
                .map_err(|_| "couldn't load server list".to_string())
        }
        Err(e) => Err(sanitize_user_error(&format!(
            "couldn't load server list: {e}"
        ))),
    };
    TaskResult::PluginCtaMcpsLoaded {
        agent_id,
        plugin_name,
        result,
    }
}
/// Convert an ACP error to a user-friendly string for display.
/// Rate-limit errors: free-usage paywall, else server detail (with API-key
/// rewrite when the body pushes personal EchoAgent), else auth-aware fallback
/// (see [`format_rate_limited_user_message`]).
/// All other errors render as the formatted request-failure banner text
/// (status headline + sanitized detail).
pub(super) fn format_acp_error(err: &acp::Error, is_api_key_auth: bool) -> String {
    if i32::from(err.code) == RATE_LIMITED_ERROR_CODE {
        let detail = err.data.as_ref().and_then(error_detail_from_data);
        return sanitize_user_error(
            &format_rate_limited_user_message(detail.as_deref(), is_api_key_auth),
        );
    }
    if err.code == acp::ErrorCode::InvalidParams && let Some(data) = &err.data
        && let Some(msg) = error_detail_from_data(data) && !msg.is_empty()
    {
        return sanitize_user_error(&msg);
    }
    let raw = err
        .data
        .as_ref()
        .and_then(error_detail_from_data)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| err.to_string());
    crate::app::error_display::format_request_failure(
            http_status_from_error(err),
            None,
            &raw,
        )
        .message()
}
/// Format a Duration for user-visible restore progress messages.
pub(super) fn format_restore_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}.{:01}s", secs, d.subsec_millis() / 100)
    }
}
/// CANONICAL wire parser for the worktree resume response. Any other code
/// consuming the `codeRestored` / `restoreSummary` / `restoreDegree` shape
/// MUST go through this function — do not re-implement.
pub(super) fn parse_worktree_restore_payload(
    result_obj: &serde_json::Value,
) -> (bool, Option<String>, Option<echo_agent_workspace::session::git::RestoreDegree>) {
    let code_restored = result_obj
        .get("codeRestored")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let restore_summary = result_obj
        .get("restoreSummary")
        .and_then(|v| v.as_str())
        .map(String::from);
    let restore_degree = result_obj
        .get("restoreDegree")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok());
    (code_restored, restore_summary, restore_degree)
}
/// CANONICAL wire parser for `LoadSessionResponse._meta.codeRestore`. Any
/// other code consuming this shape MUST go through this function — do not
/// re-implement.
pub(super) fn parse_session_load_restore_meta(
    resp_meta: Option<&acp::Meta>,
) -> (bool, Option<String>, Option<echo_agent_workspace::session::git::RestoreDegree>) {
    let code_restore = resp_meta.and_then(|m| m.get("codeRestore"));
    let code_restored = code_restore
        .and_then(|r| r.get("restored"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let restore_summary = code_restore
        .and_then(|r| r.get("summary"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let restore_degree = code_restore
        .and_then(|r| r.get("degree"))
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok());
    (code_restored, restore_summary, restore_degree)
}
/// CANONICAL wire parser for `LoadSessionResponse._meta["echo.agent/runningPromptId"]`.
///
/// Returns the session's in-flight running prompt id when the session was
/// loaded MID-turn (some other client is driving), otherwise `None`. The
/// loader adopts this id so subsequent live `session/update` deltas pass the
/// `current_prompt_id` gate (see `app/acp_handler.rs`). `pub(super)` for the
/// reconnect re-init in `event_loop.rs`, which reads the same response meta.
pub(crate) fn parse_session_load_running_prompt_id(
    resp_meta: Option<&acp::Meta>,
) -> Option<String> {
    resp_meta
        .and_then(|m| m.get("echo.agent/runningPromptId"))
        .and_then(|v| v.as_str())
        .map(String::from)
}
/// CANONICAL wire parser for the `session/new` / `session/load` response
/// `_meta[SCHEDULER_BACKGROUND_LOOPS_META_KEY]`.
///
/// Carries whether THIS session's scheduled fires run as detached background
/// subagents, as the shell resolved it when the session's actor spawned. The
/// pager stores it per session and must not re-resolve the setting: a
/// mid-session flip would then make `/loop`'s wording describe a runtime the
/// already-spawned session will never use. `None` when the shell predates the
/// key (or for gateway chat sessions, which have no local fires), leaving the
/// reader on the startup seed.
pub(crate) fn parse_session_scheduler_background_loops(
    resp_meta: Option<&acp::Meta>,
) -> Option<bool> {
    resp_meta
        .and_then(|m| {
            m.get(echo_agent_runtime::session::SCHEDULER_BACKGROUND_LOOPS_META_KEY)
        })
        .and_then(|v| v.as_bool())
}
/// Whether `raw` is (or wraps) a disk-full / ENOSPC failure.
pub(crate) fn is_disk_full_error(raw: &str) -> bool {
    raw.contains(echo_agent_fast_worktree::OUT_OF_DISK_CONTEXT)
        || raw.contains(echo_agent_fast_worktree::ENOSPC_OS_MESSAGE)
        || raw.contains("Disk quota exceeded") || raw.contains("Out of disk space")
}
/// Sanitize an error string before showing it to the user.
///
/// Strips protocol jargon (ACP, JSON-RPC) and other technical noise that would
/// be meaningless in a toast, and collapses known disk-full markers.
pub(crate) fn sanitize_user_error(raw: &str) -> String {
    if is_disk_full_error(raw) {
        return echo_agent_fast_worktree::ENOSPC_OS_MESSAGE.to_string();
    }
    static REPLACEMENTS: &[(&str, &str)] = &[
        ("cli-chat-proxy", "server"),
        ("cli_chat_proxy", "server"),
        ("inference-api", "server"),
        ("inference_api", "server"),
        ("research-api", "server"),
        ("research_api", "server"),
        ("echo-agent-code-backend", "server"),
        ("ACP error:", "error:"),
        ("ACP request failed:", "request failed:"),
        ("JSON-RPC error", "request error"),
        ("acp_send", "request"),
        ("ExtRequest", "request"),
        ("ExtNotification", "notification"),
        ("Authentication required: ", ""),
        ("Authentication failed: ", ""),
    ];
    let mut result = raw.to_string();
    for (pattern, replacement) in REPLACEMENTS {
        result = result.replace(pattern, replacement);
    }
    if result.chars().count() > 200 {
        let truncated: String = result.chars().take(180).collect();
        result = format!("{truncated}...");
    }
    result
}
/// Additive session creation flags passed from CLI → AppView → effects.
///
/// The flags map to built-in `BuiltinAgentName` profiles (`agentProfile`)
/// and, independently, gate the `ask_user_question` tool at the builder
/// (`askUserQuestion`). `--no-ask-user` always strips the tool, regardless
/// of which profile was selected.
///
/// The `askUserQuestion` column is the value the pager stamps into `_meta`;
/// `omitted` means the shell resolves the gate itself (default ON).
///
/// | plan  | subagents | ask-user | agentProfile                   | askUserQuestion    |
/// |-------|-----------|----------|--------------------------------|--------------------|
/// | false | false     | false    | `echo-agent-build` (default)         | `false`            |
/// | false | true      | false    | `echo-agent-build` (default)         | `false`            |
/// | false | false     | true     | `echo-agent-build-ask-user`          | omitted (shell gate) |
/// | false | true      | true     | `echo-agent-build-ask-user`          | omitted (shell gate) |
/// | true  | false     | false    | `echo-agent-build-plan-no-subagents` | `false`            |
/// | true  | true      | false    | `echo-agent-build-plan`              | `false`            |
/// | true  | false     | true     | `echo-agent-build-plan-no-subagents` | omitted (shell gate) |
/// | true  | true      | true     | `echo-agent-build-plan`              | omitted (shell gate) |
///
/// When [`Self::chat_mode`] is set (gateway light-frontend / `--chat`), Build
/// `agentProfile` injection is omitted (K12) and `_meta["echo.agent/session"].kind`
/// is stamped `"chat"` so the shell takes `require_gateway` / thin profile.
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionFlags {
    pub plan_mode: bool,
    pub subagents: bool,
    pub ask_user: bool,
    /// Restore code state on resume (`--restore-code`).
    /// Injected as `echo.agent/restore_code` into `LoadSession` meta, or passed
    /// as `restoreCode` in the `resume_session` ACP payload for worktrees.
    pub restore_code: Option<bool>,
    pub agent_override: Option<serde_json::Value>,
    /// Always-approve for this session (`_meta.yoloMode`).
    pub yolo_mode: bool,
    /// Auto (classifier) permission mode (`_meta.autoMode`). Mutually exclusive
    /// with `yolo_mode` on the agent; both may be set only if yolo wins at spawn.
    pub auto_mode: bool,
    /// Gateway light-frontend (`kind: "chat"`) — `--chat` / `/chat`.
    /// Mutual exclusivity with Build plan profiles: profiles are omitted and a
    /// warn is logged when plan flags are also set (K12).
    pub chat_mode: bool,
    /// Local-workspace stamp for ACP `_meta` (scrub still strips envId / Direct hub).
    #[cfg(feature = "local-workspace")]
    pub local_workspace: Option<crate::app::session_startup::LocalWorkspaceConfig>,
    /// Effective screen mode label (`ScreenMode::meta_label`), stamped into
    /// every `PromptRequest._meta.screenMode` for minimal-vs-regular usage
    /// telemetry. `None` (key omitted) only under `Default` in tests; real
    /// launches always know their mode.
    pub screen_mode_label: Option<&'static str>,
    /// Active auth is API key (not OAuth/session). Drives rate-limit copy in
    /// `format_acp_error`. Default `false` (OAuth copy) for tests.
    pub is_api_key_auth: bool,
    /// Startup resume target deferred to the worktree handler after missing
    /// local id/title resolution. Worktree failure messages append the
    /// no-match hint only when the failing target equals this value.
    pub resume_local_miss: Option<String>,
}
impl SessionFlags {
    /// Resolve the agent profile name from the flags.
    ///
    /// Returns `None` for the default `echo-agent-build` profile (no `_meta`
    /// needed; it already includes TaskTool). Chat mode never injects a
    /// Build profile (remote owns agent behavior).
    pub(super) fn agent_profile(&self) -> Option<&'static str> {
        if self.chat_mode {
            return None;
        }
        match (self.plan_mode, self.subagents, self.ask_user) {
            (true, true, _) => Some("echo-agent-build-plan"),
            (true, false, _) => Some("echo-agent-build-plan-no-subagents"),
            (false, _, true) => Some("echo-agent-build-ask-user"),
            (false, _, false) => None,
        }
    }
    /// Build the `_meta` JSON value for ACP `NewSessionRequest` / `LoadSessionRequest`.
    ///
    /// In practice always `Some`: the permission seeds (`yoloMode` /
    /// `autoMode`) are emitted unconditionally (absent key ≠ off; see the
    /// emit-site comment below). `--no-ask-user` always forces
    /// `askUserQuestion: false` into the meta, even when paired with
    /// `ECHO_AGENT_AGENT` — the env var chooses the *agent*, but the tool-strip is
    /// independent. Chat mode additionally stamps `echo.agent/session.kind`.
    pub(crate) fn to_meta(&self) -> Option<acp::Meta> {
        let mut meta = serde_json::Map::new();
        if self.chat_mode {
            if self.plan_mode || self.agent_override.is_some()
                || std::env::var("ECHO_AGENT_AGENT").ok().is_some_and(|s| !s.trim().is_empty())
            {
                tracing::warn!(
                    "chat mode active: omitting Build agentProfile (plan/agent override ignored)"
                );
            }
        } else if let Some(ref profile) = self.agent_override {
            meta.insert("agentProfile".into(), profile.clone());
        } else if std::env::var("ECHO_AGENT_AGENT").ok().is_some_and(|s| !s.trim().is_empty())
        {} else if let Some(profile) = self.agent_profile() {
            meta.insert("agentProfile".into(), serde_json::json!(profile));
        }
        if self.chat_mode {
            meta.insert("echo.agent/session".into(), serde_json::json!({ "kind": "chat" }));
            #[cfg(feature = "local-workspace")]
            if let Some(ref lw) = self.local_workspace {
                stamp_local_workspace_meta(&mut meta, lw);
            }
        }
        if !self.ask_user {
            meta.insert("askUserQuestion".into(), serde_json::json!(false));
        }
        meta.insert("yoloMode".into(), serde_json::json!(self.yolo_mode));
        meta.insert(
            "autoMode".into(),
            serde_json::json!(super::dispatch::effective_auto(
                self.yolo_mode,
                self.auto_mode
            )),
        );
        if meta.is_empty() { None } else { Some(meta) }
    }
}
/// Workspace-bind `_meta` keys **always** forbidden on chat create/load.
///
/// `echo.agent/cloud_existing_workspace` is intentionally omitted: scrub keeps it
/// iff `echo.agent/local_workspace.mode == "attach"`.
#[allow(dead_code)]
pub(super) const CHAT_FORBIDDEN_WORKSPACE_BIND_KEYS: &[&str] = &[
    "envId",
    "echo.agent/cloud_server_id",
];
/// FS-only tool ids for local existing workspace (chat attach/own).
#[cfg(feature = "local-workspace")]
pub(super) const LOCAL_WORKSPACE_FS_ONLY_TOOL_IDS: &[&str] = &[
    "workspace.fs_list",
    "workspace.fs_exists",
    "workspace.fs_read_file",
    "workspace.fs_write_file",
    "workspace.fs_delete_file",
    "workspace.put_files",
    "workspace.get_files",
];
/// Stamp `_meta["echo.agent/session"].kind = "chat"` and strip Build `agentProfile` (K12).
pub(super) fn apply_chat_kind_meta(meta: &mut Option<acp::Meta>) {
    let obj = meta.get_or_insert_with(acp::Meta::new);
    obj.insert("echo.agent/session".into(), serde_json::json!({ "kind": "chat" }));
    obj.remove("agentProfile");
}
/// Stamp chat+local intent. Attach also stamps `echo.agent/cloud_existing_workspace`.
/// Own leaves `server_id` unset — shell supervisor mints before handshake.
///
/// Never stamps `envId` or `echo.agent/cloud_server_id`.
#[cfg(feature = "local-workspace")]
pub(super) fn stamp_local_workspace_meta(
    meta: &mut serde_json::Map<String, serde_json::Value>,
    cfg: &crate::app::session_startup::LocalWorkspaceConfig,
) {
    use crate::app::session_startup::LocalWorkspaceMode;
    let mut local = serde_json::Map::new();
    let mode = match cfg.mode {
        LocalWorkspaceMode::Attach => "attach",
        LocalWorkspaceMode::Own => "own",
    };
    local.insert("mode".into(), serde_json::json!(mode));
    if let Some(ref sid) = cfg.server_id {
        local.insert("server_id".into(), serde_json::json!(sid));
    }
    if let Some(ref cwd) = cfg.cwd {
        local
            .insert("cwd".into(), serde_json::json!(cwd.to_string_lossy().into_owned()));
    }
    meta.insert("echo.agent/local_workspace".into(), serde_json::Value::Object(local));
    tracing::info!(
        target: crate::views::welcome::workspace_mode::WORKSPACE_MODE_LOG,
        event = "acp_meta_stamped",
        mode,
        server_id = cfg.server_id.as_deref(),
        cwd = cfg.cwd.as_ref().map(|p| p.display().to_string()),
        "stamped echo.agent/local_workspace onto session meta"
    );
    if cfg.mode == LocalWorkspaceMode::Attach && let Some(ref sid) = cfg.server_id {
        let mut existing = serde_json::Map::new();
        existing.insert("server_id".into(), serde_json::json!(sid));
        if let Some(ref cwd) = cfg.cwd {
            existing
                .insert(
                    "cwd".into(),
                    serde_json::json!(cwd.to_string_lossy().into_owned()),
                );
        }
        meta.insert(
            "echo.agent/cloud_existing_workspace".into(),
            serde_json::Value::Object(existing),
        );
    }
}
/// Apply [`stamp_local_workspace_meta`] onto optional ACP meta.
#[cfg(feature = "local-workspace")]
pub(super) fn apply_local_workspace_meta(
    meta: &mut Option<acp::Meta>,
    cfg: &crate::app::session_startup::LocalWorkspaceConfig,
) {
    let obj = meta.get_or_insert_with(acp::Meta::new);
    stamp_local_workspace_meta(obj, cfg);
}
/// Shared chat create/load/worktree meta finalize: kind + local stamp + scrub.
pub(super) fn finalize_chat_session_meta(
    meta: &mut Option<acp::Meta>,
    is_chat_path: bool,
    #[cfg_attr(not(feature = "local-workspace"), allow(unused_variables))]
    session_flags: &SessionFlags,
) {
    if !is_chat_path {
        return;
    }
    apply_chat_kind_meta(meta);
    #[cfg(feature = "local-workspace")]
    if let Some(ref lw) = session_flags.local_workspace {
        apply_local_workspace_meta(meta, lw);
    }
    scrub_chat_workspace_bind_meta(meta);
}
/// Remove client workspace-bind keys from chat create/load meta (defense in depth).
///
/// Narrow scrub exception: keep `echo.agent/cloud_existing_workspace` when local
/// intent is **attach**. Own stamps intent only (shell mints `server_id`).
/// Never keep `envId` or Direct hub `echo.agent/cloud_server_id`.
pub(super) fn scrub_chat_workspace_bind_meta(meta: &mut Option<acp::Meta>) {
    let Some(obj) = meta.as_mut() else {
        return;
    };
    for key in CHAT_FORBIDDEN_WORKSPACE_BIND_KEYS {
        obj.remove(*key);
    }
    #[cfg(feature = "local-workspace")]
    {
        let allow_existing_attach = obj
            .get("echo.agent/local_workspace")
            .and_then(|v| v.get("mode"))
            .and_then(|m| m.as_str()) == Some("attach");
        if !allow_existing_attach {
            obj.remove("echo.agent/cloud_existing_workspace");
        }
    }
    {
        obj.remove("echo.agent/cloud_existing_workspace");
    }
}
/// Params for shell ACP `echo.agent/session/add_local_workspace`.
///
/// v1 surface is **shell ACP-only** (no pager slash/command wiring). Pager
/// dogfood / headless clients call the extension directly with this payload.
/// No remove path until session end.
#[cfg(feature = "local-workspace")]
#[allow(dead_code)]
pub(crate) fn mid_session_add_local_workspace_params(
    session_id: &str,
    cfg: &crate::app::session_startup::LocalWorkspaceConfig,
) -> serde_json::Value {
    let mut meta = serde_json::Map::new();
    stamp_local_workspace_meta(&mut meta, cfg);
    let mut opt = Some(meta);
    scrub_chat_workspace_bind_meta(&mut opt);
    serde_json::json!({
        "sessionId": session_id,
        "meta": opt.unwrap_or_default(),
    })
}
/// Fail closed on operator attestation outside the FS-only allowlist.
/// `None` / empty attested set → uncheckable → refuse. Live server is not probed.
#[cfg(feature = "local-workspace")]
pub(crate) fn reject_non_fs_only_advertised_tools(
    advertised_tool_ids: Option<&[&str]>,
) -> Result<(), String> {
    let Some(ids) = advertised_tool_ids else {
        return Err(
            "operator attestation ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS is unset \
             (uncheckable); refuse attach. Live workspace_server was not inspected. Set \
             the env to a comma-separated FS-only catalog."
                .into(),
        );
    };
    if ids.is_empty() {
        return Err(
            "operator attestation ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS is empty \
             (uncheckable); refuse attach. Live workspace_server was not inspected."
                .into(),
        );
    }
    let forbidden: Vec<&str> = ids
        .iter()
        .copied()
        .filter(|id| !LOCAL_WORKSPACE_FS_ONLY_TOOL_IDS.contains(id))
        .collect();
    if forbidden.is_empty() {
        Ok(())
    } else {
        Err(
                format!(
            "operator attestation lists tools outside the FS-only allowlist: {}. \
             Live workspace_server was not inspected. Fix \
             ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS or restart workspace_server \
             with --require-explicit-toolset and an FS-only catalog.",
            forbidden.join(", ")
        ),
            )
    }
}
/// Metadata returned from effect execution so the event loop can patch
/// state that requires a spawned task handle (e.g., auth AbortHandle).
#[derive(Default)]
pub(crate) struct EffectMeta {
    /// Auth abort handle + its request sequence. The event loop must
    /// install this into `AppView.auth_state` if the current auth state
    /// still matches the sequence.
    pub auth_abort_handle: Option<(u64, tokio::task::AbortHandle)>,
    /// Auth URL poll abort handle + request sequence (installed on
    /// `AppView.auth_url_poll_handle` when the seq still matches).
    pub auth_url_poll_handle: Option<(u64, tokio::task::AbortHandle)>,
}
/// Extract the first user prompt text from a session's `chat_history.jsonl`.
///
/// Returns the first line of the `<user_query>` content (if present),
/// or the first line of the raw user message text.
pub(super) fn extract_first_user_prompt(
    info: &echo_agent_runtime::session::info::Info,
) -> Option<String> {
    use std::io::BufRead;
    let history_path = echo_agent_runtime::session::persistence::session_dir(info)
        .join("chat_history.jsonl");
    let file = std::fs::File::open(history_path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines() {
        let line = line.ok()?;
        let v: serde_json::Value = serde_json::from_str(&line).ok()?;
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        let content = v.get("content");
        let text = content
            .and_then(|c| c.as_array())
            .and_then(|arr| {
                arr
                    .iter()
                    .find_map(|block| {
                        if block.get("type")?.as_str()? == "text" {
                            block.get("text")?.as_str().map(String::from)
                        } else {
                            None
                        }
                    })
            })
            .or_else(|| content.and_then(|c| c.as_str()).map(String::from))?;
        if let Some(start) = text.find("<user_query>") {
            let after = &text[start + "<user_query>".len()..];
            let end = after.find("</user_query>").unwrap_or(after.len());
            let query = after[..end].trim();
            if !query.is_empty() && !query.starts_with('<') {
                return Some(query.to_string());
            }
        }
    }
    None
}
/// Typed deserialization so schema drift is caught at compile time.
/// Synthetic user messages (auto-continue, doom-loop) are excluded.
pub(super) fn count_chat_history_stats(history_path: &Path) -> (usize, usize) {
    use std::io::BufRead;
    use echo_agent_runtime::sampling::{AssistantItem, ConversationItem, UserItem};
    let mut turn_count = 0usize;
    let mut tool_call_count = 0usize;
    let Ok(file) = std::fs::File::open(history_path) else {
        return (0, 0);
    };
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        match serde_json::from_str::<ConversationItem>(&line) {
            Ok(ConversationItem::User(UserItem { synthetic_reason: None, .. })) => {
                turn_count += 1;
            }
            Ok(ConversationItem::Assistant(AssistantItem { ref tool_calls, .. })) => {
                tool_call_count += tool_calls.len();
            }
            _ => {}
        }
    }
    (turn_count, tool_call_count)
}
/// Degraded conversations lane on `echo.agent/session/list`, parsed from the
/// response's `_meta["echo.agent/partial"]` envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationsPartial {
    NoOauth,
    Timeout,
    Error,
}
impl ConversationsPartial {
    /// Actionable picker notice for a degraded conversations lane.
    pub(crate) fn picker_notice(self) -> &'static str {
        match self {
            Self::NoOauth => "Couldn't load your chats: log in with /login",
            Self::Timeout | Self::Error => "Couldn't load conversations: retry",
        }
    }
}
/// Read `_meta["echo.agent/partial"]` from a session-list payload. `None` when the
/// conversations lane completed (or was skipped); unknown reasons degrade to
/// [`ConversationsPartial::Error`].
pub(super) fn parse_session_list_partial(
    payload: &serde_json::Value,
) -> Option<ConversationsPartial> {
    let partial = payload.get("_meta")?.get("echo.agent/partial")?;
    if partial.get("conversations").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    Some(
        match partial.get("reason").and_then(|v| v.as_str()) {
            Some("no_oauth") => ConversationsPartial::NoOauth,
            Some("timeout") => ConversationsPartial::Timeout,
            _ => ConversationsPartial::Error,
        },
    )
}
/// Reads `_meta["echo.agent/listScope"]` from a session-list payload.
pub(super) fn parse_session_list_scope(payload: &serde_json::Value) -> ListScope {
    match payload
        .get("_meta")
        .and_then(|m| m.get("echo.agent/listScope"))
        .and_then(|v| v.as_str())
    {
        Some("repo") => ListScope::Repo,
        Some("all") => ListScope::All,
        _ => ListScope::Cwd,
    }
}
/// Parse the `echo.agent/session/list` response payload (the unwrapped
/// `{ "sessions": [...] }` object) into [`SessionPickerEntry`] rows.
///
/// Shared by the resume picker ([`Effect::FetchSessionList`]) and the
/// dashboard's non-leader idle-session fallback
/// ([`Effect::FetchDashboardSessions`]) so both produce identical labels.
/// Sessions older than 30 days, and sessions with no usable user prompt
/// (empty `summary` after fallbacks), are dropped.
pub(super) fn parse_session_picker_entries(
    payload: &serde_json::Value,
) -> Vec<crate::app::app_view::SessionPickerEntry> {
    use crate::app::app_view::SessionPickerEntry;
    let entries: Vec<serde_json::Value> = payload
        .get("sessions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::days(30);
    entries
        .into_iter()
        .filter_map(|v| {
            let id = v
                .get("sessionId")
                .or_else(|| v.get("session_id"))
                .and_then(|s| s.as_str())?
                .to_string();
            let summary = v
                .get("summary")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            let first_prompt = v
                .get("firstPrompt")
                .or_else(|| v.get("first_prompt"))
                .and_then(|s| s.as_str())
                .map(String::from);
            let is_conversation = v
                .get("_meta")
                .and_then(|m| m.get("echo.agent/session"))
                .and_then(|s| s.get("kind"))
                .and_then(|k| k.as_str()) == Some("chat");
            let parsed_updated: Option<chrono::DateTime<chrono::Utc>> = v
                .get("updatedAt")
                .or_else(|| v.get("updated_at"))
                .and_then(|s| s.as_str())
                .and_then(|s| s.parse().ok());
            let parsed_created: Option<chrono::DateTime<chrono::Utc>> = v
                .get("createdAt")
                .or_else(|| v.get("created_at"))
                .and_then(|s| s.as_str())
                .and_then(|s| s.parse().ok());
            let updated_at: chrono::DateTime<chrono::Utc> = match parsed_updated {
                Some(ts) => {
                    if !is_conversation && ts < cutoff {
                        return None;
                    }
                    ts
                }
                None => {
                    if !is_conversation {
                        return None;
                    }
                    parsed_created.unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
                }
            };
            use echo_agent_tools::implementations::skills::skill::extract_skill_display_text;
            let display = if let Some(ref fp) = first_prompt {
                if let Some(d) = extract_skill_display_text(fp) {
                    d
                } else if !summary.is_empty() {
                    extract_skill_display_text(&summary).unwrap_or(summary)
                } else {
                    fp.lines().next().unwrap_or_default().trim().to_string()
                }
            } else if !summary.is_empty() {
                extract_skill_display_text(&summary).unwrap_or(summary)
            } else {
                let info_cwd = v
                    .get("cwd")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default()
                    .to_string();
                let info = echo_agent_runtime::session::info::Info {
                    id: acp::SessionId::new(id.clone()),
                    cwd: info_cwd,
                };
                extract_first_user_prompt(&info).unwrap_or_default()
            };
            let created_at: chrono::DateTime<chrono::Utc> = parsed_created
                .unwrap_or(updated_at);
            let cwd_str = v
                .get("cwd")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            let hostname = v.get("hostname").and_then(|s| s.as_str()).map(String::from);
            let source = if is_conversation {
                "conversation".to_string()
            } else {
                v.get("source").and_then(|s| s.as_str()).unwrap_or("local").to_string()
            };
            let model_id = v
                .get("modelId")
                .or_else(|| v.get("model_id"))
                .and_then(|s| s.as_str())
                .map(String::from);
            let num_messages = v
                .get("numMessages")
                .or_else(|| v.get("num_messages"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0) as usize;
            let last_active_at: Option<chrono::DateTime<chrono::Utc>> = v
                .get("lastActiveAt")
                .or_else(|| v.get("last_active_at"))
                .and_then(|s| s.as_str())
                .and_then(|s| s.parse().ok());
            let branch = v.get("branch").and_then(|s| s.as_str()).map(String::from);
            let worktree_label = v
                .get("worktreeLabel")
                .or_else(|| v.get("worktree_label"))
                .and_then(|s| s.as_str())
                .map(String::from);
            let last_turn_summary = v
                .get("lastTurnSummary")
                .or_else(|| v.get("last_turn_summary"))
                .and_then(|s| s.as_str())
                .map(String::from);
            let last_recap = v
                .get("lastRecap")
                .or_else(|| v.get("last_recap"))
                .and_then(|s| s.as_str())
                .map(String::from);
            let repo_name = crate::views::session_picker::repo_name_from_cwd(&cwd_str);
            Some(SessionPickerEntry {
                id,
                summary: display,
                updated_at,
                created_at,
                cwd: cwd_str,
                hostname,
                source,
                model_id,
                num_messages,
                last_active_at,
                branch,
                repo_name,
                worktree_label,
                last_turn_summary,
                last_recap,
                card_detail: None,
            })
        })
        .filter_map(|mut e| {
            if e.summary.is_empty() {
                if e.source == "conversation" {
                    e.summary = "Untitled".to_string();
                } else {
                    return None;
                }
            }
            if e.source == "remote"
                && echo_agent_runtime::session::resolve_local_session_any_cwd(&e.id)
                    .is_some()
            {
                e.source = "local".to_string();
            }
            Some(e)
        })
        .collect()
}
/// Convert a resume-picker session into a dormant dashboard roster row.
///
/// Used by the non-leader dashboard fallback: local on-disk sessions have no
/// live activity signal, so they map to [`RosterActivity::Dormant`] and render
/// in the dashboard's **Inactive** group. The label, cwd, model, and worktree
/// badge all come straight from the picker entry.
pub(super) fn session_picker_entry_to_roster(
    e: &crate::app::app_view::SessionPickerEntry,
) -> crate::app::roster::RosterEntry {
    use crate::app::roster::{RosterActivity, RosterEntry, RosterOrigin};
    let last_change = e.last_active_at.unwrap_or(e.updated_at);
    RosterEntry {
        session_id: e.id.clone(),
        title: Some(e.summary.clone()).filter(|s| !s.trim().is_empty()),
        cwd: e.cwd.clone(),
        is_worktree: e.worktree_label.is_some(),
        model_id: e.model_id.clone(),
        yolo: false,
        activity: RosterActivity::Dormant,
        last_turn_summary: e.last_turn_summary.clone(),
        resident: false,
        last_change_unix_ms: last_change.timestamp_millis(),
        origin: RosterOrigin {
            kind: e.source.clone(),
            host: e.hostname.clone(),
        },
    }
}
pub(super) async fn send_logout(tx: &AcpAgentTx) {
    let req = acp::ExtRequest::new(
        "echo.agent/auth/logout",
        serde_json::value::to_raw_value(&serde_json::json!({}))
            .expect("serialize auth/logout params")
            .into(),
    );
    if let Err(e) = acp_send(req, tx).await {
        tracing::warn!(error = %e, "logout failed");
    }
}
/// Best-effort `echo.agent/auth/cancel`: stops the shell's device/loopback wait so a
/// later login is single-flight. Errors are ignored — UI already left
/// `Authenticating`. `request_seq` scopes the cancel to the abandoned attempt.
pub(super) async fn send_auth_cancel(tx: &AcpAgentTx, request_seq: u64) -> TaskResult {
    let req = acp::ExtRequest::new(
        "echo.agent/auth/cancel",
        serde_json::value::to_raw_value(
                &serde_json::json!({ "request_seq": request_seq }),
            )
            .expect("serialize auth/cancel params")
            .into(),
    );
    if let Err(e) = acp_send(req, tx).await {
        tracing::debug!(error = %e, "auth cancel ext request failed (ignored)");
    }
    TaskResult::AuthCancelComplete
}
pub(super) async fn send_check_subscription(
    tx: &AcpAgentTx,
    verify: Option<u64>,
) -> TaskResult {
    let req = acp::ExtRequest::new(
        "echo.agent/auth/check_subscription",
        serde_json::value::to_raw_value(&serde_json::json!({}))
            .expect("serialize check_subscription params")
            .into(),
    );
    match acp_send(req, tx).await {
        Ok(resp) => {
            let meta = serde_json::from_str::<serde_json::Value>(resp.0.get())
                .ok()
                .and_then(|v| v.get("meta").cloned());
            TaskResult::CheckSubscriptionComplete {
                verify,
                meta,
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "check_subscription failed");
            crate::unified_log::warn(
                "subscription.check.rpc_failed",
                None,
                Some(serde_json::json!({
                    "verify": verify,
                    "error": e.to_string(),
                })),
            );
            TaskResult::CheckSubscriptionComplete {
                verify,
                meta: None,
            }
        }
    }
}
/// One-shot subscription re-check for the credit-limit retry flow.
/// Same ACP call as `send_check_subscription` but returns a
/// `CreditLimitRecheckComplete` so the dispatch layer can decide
/// whether to retry the stashed prompt or show the upsell.
pub(super) async fn send_credit_limit_recheck(
    tx: &AcpAgentTx,
    agent_id: AgentId,
) -> TaskResult {
    let req = acp::ExtRequest::new(
        "echo.agent/auth/check_subscription",
        serde_json::value::to_raw_value(&serde_json::json!({}))
            .expect("serialize check_subscription params")
            .into(),
    );
    match acp_send(req, tx).await {
        Ok(resp) => {
            let meta = serde_json::from_str::<serde_json::Value>(resp.0.get())
                .ok()
                .and_then(|v| v.get("meta").cloned());
            TaskResult::CreditLimitRecheckComplete {
                agent_id,
                meta,
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "credit_limit_recheck failed");
            TaskResult::CreditLimitRecheckComplete {
                agent_id,
                meta: None,
            }
        }
    }
}
pub(super) async fn send_authenticate(
    tx: &AcpAgentTx,
    request_seq: u64,
    method_id: acp::AuthMethodId,
    use_oauth: bool,
    force_interactive: bool,
) -> TaskResult {
    let mut meta = serde_json::json!({
        "use_oauth": use_oauth,
        "request_seq": request_seq,
    });
    if force_interactive {
        meta["force_interactive"] = serde_json::json!(true);
    }
    let req = acp::AuthenticateRequest::new(method_id).meta(meta.as_object().cloned());
    match acp_send(req, tx).await {
        Ok(resp) => {
            ulog::info("auth completed", None, None);
            TaskResult::AuthComplete {
                request_seq,
                meta: resp.meta.map(serde_json::Value::Object),
            }
        }
        Err(e) => {
            let error = sanitize_user_error(&e.to_string());
            ulog::error(
                "auth failed",
                None,
                Some(serde_json::json!({"error": &error})),
            );
            TaskResult::AuthFailed {
                request_seq,
                error,
            }
        }
    }
}
/// Translate a settings-registry key + value into the matching shell
/// helper call. Type mismatches return an error (not panic) so a
/// spawned task doesn't crash the pager. Unknown keys also return
/// a descriptive error.
pub(crate) async fn persist_setting(
    key: crate::settings::SettingKey,
    value: crate::settings::SettingValue,
) -> Result<(), String> {
    use crate::settings::SettingValue;
    fn kind_mismatch(key: &str, expected: &str, got: &SettingValue) -> String {
        format!("persist_setting({key}) expected {expected}, got {got:?}")
    }
    match key {
        "compact_mode" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("compact_mode", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_compact_mode(b)
                .await
                .map_err(|e| e.to_string())
        }
        "trace_upload" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("trace_upload", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_trace_upload(b)
                .await
                .map_err(|e| e.to_string())
        }
        "feedback_trace_card" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("feedback_trace_card", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_feedback_trace_card(b)
                .await
                .map_err(|e| e.to_string())
        }
        "show_timestamps" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("show_timestamps", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_show_timestamps(b)
                .await
                .map_err(|e| e.to_string())
        }
        "page_flip_on_send" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("page_flip_on_send", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_page_flip_on_send(b)
                .await
                .map_err(|e| e.to_string())
        }
        "confirm_before_rewind" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("confirm_before_rewind", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_confirm_before_rewind(b)
                .await
                .map_err(|e| e.to_string())
        }
        "combine_queued_prompts" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("combine_queued_prompts", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_combine_queued_prompts(b)
                .await
                .map_err(|e| e.to_string())
        }
        "follow_up_behavior" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("follow_up_behavior", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_follow_up_behavior(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "show_timeline" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("show_timeline", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_show_timeline(b)
                .await
                .map_err(|e| e.to_string())
        }
        "simple_mode" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("simple_mode", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_simple_mode(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.undo" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("contextual_hints.undo", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_contextual_hint_undo(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.plan_mode" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("contextual_hints.plan_mode", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_contextual_hint_plan_mode(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.image_input" => {
            let SettingValue::Bool(b) = value else {
                return Err(
                    kind_mismatch("contextual_hints.image_input", "Bool", &value),
                );
            };
            echo_agent_runtime::util::config::set_contextual_hint_image_input(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.send_now" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("contextual_hints.send_now", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_contextual_hint_send_now(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.small_screen" => {
            let SettingValue::Bool(b) = value else {
                return Err(
                    kind_mismatch("contextual_hints.small_screen", "Bool", &value),
                );
            };
            echo_agent_runtime::util::config::set_contextual_hint_small_screen(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.word_select" => {
            let SettingValue::Bool(b) = value else {
                return Err(
                    kind_mismatch("contextual_hints.word_select", "Bool", &value),
                );
            };
            echo_agent_runtime::util::config::set_contextual_hint_word_select(b)
                .await
                .map_err(|e| e.to_string())
        }
        "contextual_hints.ssh_wrap" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("contextual_hints.ssh_wrap", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_contextual_hint_ssh_wrap(b)
                .await
                .map_err(|e| e.to_string())
        }
        "theme" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("theme", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_theme(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "auto_dark_theme" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("auto_dark_theme", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_auto_dark_theme(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "auto_light_theme" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("auto_light_theme", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_auto_light_theme(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "default_model" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("default_model", "String", &value));
            };
            echo_agent_runtime::util::config::set_default_model(s)
                .await
                .map_err(|e| e.to_string())
        }
        "scroll_speed" => {
            let SettingValue::Int(i) = value else {
                return Err(kind_mismatch("scroll_speed", "Int", &value));
            };
            echo_agent_runtime::util::config::set_scroll_speed(i)
                .await
                .map_err(|e| e.to_string())
        }
        "scroll_mode" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("scroll_mode", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_scroll_mode(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "invert_scroll" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("invert_scroll", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_invert_scroll(b)
                .await
                .map_err(|e| e.to_string())
        }
        "display_refresh_auto_cadence" => {
            let SettingValue::Bool(b) = value else {
                return Err(
                    kind_mismatch("display_refresh_auto_cadence", "Bool", &value),
                );
            };
            echo_agent_runtime::util::config::set_display_refresh_auto_cadence(b)
                .await
                .map_err(|e| e.to_string())
        }
        "scroll_lines" => {
            let SettingValue::Int(i) = value else {
                return Err(kind_mismatch("scroll_lines", "Int", &value));
            };
            echo_agent_runtime::util::config::set_scroll_lines(i)
                .await
                .map_err(|e| e.to_string())
        }
        "default_selected_permission" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("default_selected_permission", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_default_selected_permission(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "cancel_subagents_on_turn_cancel" => {
            let SettingValue::Enum(s) = value else {
                return Err(
                    kind_mismatch("cancel_subagents_on_turn_cancel", "Enum", &value),
                );
            };
            echo_agent_runtime::util::config::set_cancel_subagents_on_turn_cancel(
                    s.to_string(),
                )
                .await
                .map_err(|e| e.to_string())
        }
        "vim_mode" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("vim_mode", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_vim_mode(b)
                .await
                .map_err(|e| e.to_string())
        }
        "remember_tool_approvals" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("remember_tool_approvals", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_remember_tool_approvals(b)
                .await
                .map_err(|e| e.to_string())
        }
        "toolset.ask_user_question.timeout_enabled" => {
            let SettingValue::Bool(b) = value else {
                return Err(
                    kind_mismatch(
                        "toolset.ask_user_question.timeout_enabled",
                        "Bool",
                        &value,
                    ),
                );
            };
            echo_agent_runtime::util::config::set_ask_user_question_timeout_enabled(b)
                .await
                .map_err(|e| e.to_string())
        }
        "show_thinking_blocks" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("show_thinking_blocks", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_show_thinking_blocks(b)
                .await
                .map_err(|e| e.to_string())
        }
        "group_tool_verbs" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("group_tool_verbs", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_group_tool_verbs(b)
                .await
                .map_err(|e| e.to_string())
        }
        "collapsed_edit_blocks" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("collapsed_edit_blocks", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_collapsed_edit_blocks(b)
                .await
                .map_err(|e| e.to_string())
        }
        "prompt_suggestions" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("prompt_suggestions", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_prompt_suggestions(b)
                .await
                .map_err(|e| e.to_string())
        }
        "keep_text_selection" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("keep_text_selection", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_keep_text_selection(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "respect_manual_folds" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("respect_manual_folds", "Bool", &value));
            };
            tokio::task::spawn_blocking(move || crate::appearance::persist_respect_manual_folds(
                    b,
                ))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())
        }
        "render_mermaid" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("render_mermaid", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_render_mermaid(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "hunk_tracker_mode" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("hunk_tracker_mode", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_hunk_tracker_mode(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "screen_mode" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("screen_mode", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_screen_mode(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "voice_keybind_enabled" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("voice_keybind_enabled", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_voice_keybind_enabled(b)
                .await
                .map_err(|e| e.to_string())
        }
        "voice_capture_mode" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("voice_capture_mode", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_voice_capture_mode(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "voice_stt_language" => {
            let SettingValue::Enum(s) = value else {
                return Err(kind_mismatch("voice_stt_language", "Enum", &value));
            };
            echo_agent_runtime::util::config::set_voice_stt_language(s.to_string())
                .await
                .map_err(|e| e.to_string())
        }
        "max_thoughts_width" => {
            let SettingValue::Int(i) = value else {
                return Err(kind_mismatch("max_thoughts_width", "Int", &value));
            };
            echo_agent_runtime::util::config::set_max_thoughts_width(i)
                .await
                .map_err(|e| e.to_string())
        }
        "show_tips" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("show_tips", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_show_tips(b)
                .await
                .map_err(|e| e.to_string())
        }
        "auto_update" => {
            let SettingValue::Bool(b) = value else {
                return Err(kind_mismatch("auto_update", "Bool", &value));
            };
            echo_agent_runtime::util::config::set_auto_update(b)
                .await
                .map_err(|e| e.to_string())
        }
        "fork_secondary_model" => {
            let SettingValue::String(s) = value else {
                return Err(kind_mismatch("fork_secondary_model", "String", &value));
            };
            echo_agent_runtime::util::config::set_fork_secondary_model(s)
                .await
                .map_err(|e| e.to_string())
        }
        other => Err(format!("unknown setting key for persist: `{other}`")),
    }
}
/// Body for `Effect::PersistPermissionMode`. Factored out for testability.
///
/// 1. Persist `ui.permission_mode` to disk.
/// 2. Fire ACP `echo.agent/yolo_mode_changed` (gated on disk success for
///    `WithRollback`; always for `BestEffort`).
/// 3. Return the matching `TaskResult`.
pub(crate) async fn persist_permission_mode_and_notify(
    canonical: &'static str,
    session_id: Option<acp::SessionId>,
    persist: PermissionModePersist,
    tx: AcpAgentTx,
) -> TaskResult {
    let enabled = canonical == "always-approve";
    let auto_mode = canonical == "auto";
    let config_str: &'static str = canonical;
    let disk_result = echo_agent_runtime::util::config::update_config(|cfg| {
            cfg.ui.permission_mode = Some(config_str.to_string());
        })
        .await;
    let disk_outcome: Result<(), String> = disk_result.map_err(|e| e.to_string());
    if should_send_yolo_acp_notification(&disk_outcome, persist) && session_id.is_some()
    {
        let params = serde_json::json!({
            "yolo_mode": enabled,
            "auto_mode": auto_mode,
            "permission_mode": config_str,
        });
        let notification = acp::ExtNotification::new(
            "echo.agent/yolo_mode_changed",
            serde_json::value::to_raw_value(&params)
                .expect("serialize yolo_mode_changed params")
                .into(),
        );
        if let Err(e) = acp_send(notification, &tx).await {
            tracing::warn!("Failed to send yolo_mode_changed notification: {e}");
        }
    }
    route_permission_mode_result(disk_outcome, persist, config_str)
}
/// Whether to fire the ACP `echo.agent/yolo_mode_changed` notification.
/// `WithRollback` suppresses on disk failure (agent must not see the
/// optimistic value). `BestEffort` always fires.
pub(super) fn should_send_yolo_acp_notification(
    disk_outcome: &Result<(), String>,
    persist: PermissionModePersist,
) -> bool {
    match (disk_outcome, persist) {
        (_, PermissionModePersist::BestEffort) => true,
        (Ok(()), PermissionModePersist::WithRollback(_)) => true,
        (Err(_), PermissionModePersist::WithRollback(_)) => false,
    }
}
pub(super) fn marketplace_outcome_succeeded(
    outcome: &echo_agent_hooks_plugins_types::ActionOutcome,
) -> bool {
    outcome.status == echo_agent_hooks_plugins_types::OutcomeStatus::Success
}
/// Extract the typed kill outcome from an `echo.agent/task/kill` ext response.
///
/// The agent serializes `ExtMethodResult<KillTaskResponse>`, so the outcome
/// lives at `result.outcome` (`{"result":{"taskId":..,"outcome":
/// "not_found"}}`). Deserializes through the same wire DTOs the agent
/// serializes (`echo_agent_runtime::extensions::task::KillTaskResponse` +
/// `echo_agent_runtime::session::result::ExtMethodResult`) so the contract stays
/// typed end-to-end. Returns `None` — which the dispatcher treats as "clear
/// pending state, keep the row" — for error envelopes (`result: null`) or
/// unparseable payloads. Probing the top level with untyped JSON here was
/// why the tasks-pane ✗ never removed stale (`not_found`) rows after a
/// session resume.
pub(super) fn parse_kill_outcome(
    resp: &str,
) -> Option<echo_agent_tools::types::KillOutcome> {
    use echo_agent_runtime::extensions::task::KillTaskResponse;
    use echo_agent_runtime::session::result::ExtMethodResult;
    serde_json::from_str::<ExtMethodResult<KillTaskResponse>>(resp)
        .ok()
        .and_then(|envelope| envelope.result)
        .map(|payload| payload.outcome)
}
/// Map an `echo.agent/subagent/cancel` response (payload under `result`) to a kill
/// outcome. Prefers the typed `outcome`; falls back to the legacy `cancelled`
/// bool for an older shell or an unknown future `kind`. An error/unparseable
/// body is `RpcFailed` (subagent may still be running — leave the row alone).
pub(super) fn parse_subagent_kill_outcome(resp: &str) -> SubagentKillOutcome {
    use echo_agent_runtime::extensions::task::{
        CancelSubagentResponse, SubagentCancelOutcomeDto,
    };
    let Some(payload) = serde_json::from_str::<
        ExtMethodResult<CancelSubagentResponse>,
    >(resp)
        .ok()
        .and_then(|envelope| envelope.result) else {
        return SubagentKillOutcome::RpcFailed;
    };
    match payload.outcome {
        Some(SubagentCancelOutcomeDto::Cancelled) => SubagentKillOutcome::StoppedLive,
        Some(SubagentCancelOutcomeDto::AlreadyFinished { status }) => {
            SubagentKillOutcome::NothingLive {
                status: Some(status),
            }
        }
        Some(SubagentCancelOutcomeDto::NotFound) => {
            SubagentKillOutcome::NothingLive {
                status: None,
            }
        }
        Some(SubagentCancelOutcomeDto::Unknown) | None => {
            if payload.cancelled {
                SubagentKillOutcome::StoppedLive
            } else {
                SubagentKillOutcome::NothingLive {
                        status: None,
                    }
            }
        }
    }
}
/// Map disk-write outcome + persist variant to the correct `TaskResult`.
pub(super) fn route_permission_mode_result(
    disk_outcome: Result<(), String>,
    persist: PermissionModePersist,
    config_str: &'static str,
) -> TaskResult {
    match (disk_outcome, persist) {
        (Ok(()), _) => {
            TaskResult::SettingPersisted {
                key: "permission_mode",
                value: crate::settings::SettingValue::Enum(config_str),
            }
        }
        (Err(e), PermissionModePersist::WithRollback(prev_canonical)) => {
            tracing::warn!("failed to save permission mode preference: {e} — rolling back");
            TaskResult::SettingPersistFailed {
                key: "permission_mode",
                rollback_value: crate::settings::SettingValue::Enum(prev_canonical),
                error: e,
            }
        }
        (Err(e), PermissionModePersist::BestEffort) => {
            tracing::warn!("failed to save permission mode preference (best-effort): {e}");
            TaskResult::SettingPersistFailedBestEffort {
                key: "permission_mode",
                error: e,
            }
        }
    }
}
/// Fire-and-forget blocking write of one `[hints]` value to config.toml.
/// `what` names the preference for log messages.
pub(super) fn persist_hint(
    tasks: &mut JoinSet<TaskResult>,
    key: &'static str,
    value: impl Into<toml_edit::Value> + Send + 'static,
    what: &'static str,
) {
    tasks
        .spawn(async move {
            match tokio::task::spawn_blocking(move || crate::config_toml_edit::set_hint(
                    key,
                    value,
                ))
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("failed to persist {what}: {e}"),
                Err(e) => tracing::warn!("failed to persist {what} (join error): {e}"),
            }
            TaskResult::CancelComplete
        });
}
/// Map a billing config into a [`CreditBalance`].
///
/// Prefers the newer credits-config fields (`credit_usage_percent`,
/// `current_period`) and falls back to the deprecated
/// `monthly_limit`/`used`/`billing_period_end`. Shared by `Effect::FetchBilling`
/// and `Effect::FetchAppBilling` so every pager UI path derives identical usage
/// values from the same config.
pub(super) fn credit_balance_from_config(
    c: echo_agent_runtime::extensions::billing::BillingConfig,
) -> crate::views::credit_bar::CreditBalance {
    let limit = c.monthly_limit.map(|v| v.val).unwrap_or(0);
    let used = c.used.map(|v| v.val).unwrap_or(0);
    let has_credit_pct = c.credit_usage_percent.is_some();
    let usage_pct = match c.credit_usage_percent {
        Some(pct) => pct.clamp(0.0, 100.0),
        None if limit > 0 => (used as f64 / limit as f64 * 100.0).min(100.0),
        None => 0.0,
    };
    let period_end_display = c
        .current_period
        .as_ref()
        .and_then(|p| p.end.clone())
        .or(c.billing_period_end)
        .and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| {
                    dt.with_timezone(&chrono::Local).format("%B %-d, %H:%M").to_string()
                })
        });
    let on_demand_val = c.on_demand_cap.map(|v| v.val).unwrap_or(0);
    let pay_as_you_go = on_demand_val > 0;
    let on_demand_cap_cents = if on_demand_val > 0 { Some(on_demand_val) } else { None };
    let on_demand_used_cents = c
        .on_demand_used
        .map(|v| v.val)
        .unwrap_or_else(|| (used - limit).max(0));
    let effective_usage_pct = if on_demand_val > 0 {
        if usage_pct >= 100.0 {
            (on_demand_used_cents as f64 / on_demand_val as f64 * 100.0).min(100.0)
        } else if has_credit_pct {
            usage_pct
        } else {
            let total_budget = limit + on_demand_val;
            if total_budget > 0 {
                (used as f64 / total_budget as f64 * 100.0).min(100.0)
            } else {
                0.0
            }
        }
    } else {
        usage_pct
    };
    let period_type = c.current_period.as_ref().and_then(|p| p.period_type.clone());
    crate::views::credit_bar::CreditBalance {
        usage_pct,
        effective_usage_pct,
        period_end_display,
        pay_as_you_go,
        on_demand_cap_cents,
        on_demand_used_cents: Some(on_demand_used_cents),
        prepaid_balance_cents: c.prepaid_balance.map(|v| v.val),
        period_type,
        is_unified_billing_user: c.is_unified_billing_user,
    }
}
/// Whether the balance carries a non-zero prepaid credit balance (signed cents).
pub(super) fn has_prepaid_credits(
    balance: Option<&crate::views::credit_bar::CreditBalance>,
) -> bool {
    balance.and_then(|b| b.prepaid_balance_cents).map(i64::abs).is_some_and(|c| c > 0)
}
/// Fetch the user's auto top-up rule via the `echo.agent/auto-topup-rule` extension.
/// A transport failure yields [`AutoTopupFetch::Unchanged`] so the caller keeps
/// any cached rule rather than treating the blip as "no auto top-up".
pub(super) async fn fetch_auto_topup_info(
    tx: &echo_agent_acp::AcpAgentTx,
) -> crate::views::credit_bar::AutoTopupFetch {
    use crate::views::credit_bar::AutoTopupFetch;
    let req = acp::ExtRequest::new(
        "echo.agent/auto-topup-rule",
        serde_json::value::to_raw_value(&serde_json::json!({}))
            .expect("serialize auto-topup params")
            .into(),
    );
    let Ok(resp) = acp_send(req, tx).await else {
        return AutoTopupFetch::Unchanged;
    };
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get())
        .unwrap_or_default();
    let result = wrapper.get("result").unwrap_or(&wrapper);
    parse_auto_topup_response(result)
}
/// Map an `echo.agent/auto-topup-rule` payload to an [`AutoTopupFetch`]. A body that
/// fails to deserialize is a fetch error (→ `Unchanged`, keep the cached rule),
/// not a definitive "no rule", so a malformed response can't silently flip the
/// credits warning.
pub(super) fn parse_auto_topup_response(
    result: &serde_json::Value,
) -> crate::views::credit_bar::AutoTopupFetch {
    use crate::views::credit_bar::{AutoTopupFetch, AutoTopupInfo};
    use echo_agent_runtime::extensions::billing::GetAutoTopupRuleResponse;
    match serde_json::from_value::<GetAutoTopupRuleResponse>(result.clone()) {
        Ok(parsed) => {
            AutoTopupFetch::Resolved(
                parsed
                    .rule
                    .map_or_else(
                        AutoTopupInfo::disabled,
                        |rule| AutoTopupInfo {
                            enabled: rule.enabled,
                            topup_amount_cents: rule.topup_amount.map(|c| c.val),
                            max_amount_cents: rule.max_amount_per_month.map(|c| c.val),
                        },
                    ),
            )
        }
        Err(_) => AutoTopupFetch::Unchanged,
    }
}
/// A blocking flock on the shared, possibly-network `~/.echo-agent` lock must never
/// stall the event-loop thread (and would hang exit on `/quit`); the registry
/// is best-effort, so skip on contention.
pub(super) fn unregister_active_session_best_effort(session_id: &acp::SessionId) {
    unregister_active_session_best_effort_in(
        &echo_agent_runtime::util::echo_agent_home::echo_agent_home(),
        session_id,
    );
}
pub(super) fn unregister_active_session_best_effort_in(
    root: &Path,
    session_id: &acp::SessionId,
) {
    match echo_agent_active_sessions::try_unregister_in(root, session_id) {
        Ok(true) => {}
        Ok(false) => {
            tracing::debug!(
            session_id = %session_id.0,
            "Skipped active-session unregister under lock contention; \
             reaped by collect_crashed on next launch"
        )
        }
        Err(e) => tracing::warn!(?e, "Failed to unregister active session"),
    }
}
