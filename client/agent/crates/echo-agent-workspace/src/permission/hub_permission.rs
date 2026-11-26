//! Tool-permission emit: when the rules engine returns "ask" for a guarded
//! tool, request the decision from chat over the server instead of prompting a
//! local ACP client, then map chat's reply back onto a [`PromptOutcome`] so the
//! manager's existing decision + `ALWAYS_*` persistence applies unchanged.
use crate::permission::prompter::{PromptOutcome, tool_name_for_access};
use crate::permission::types::AccessKind;
use async_trait::async_trait;
use echo_agent_computer_hub_sdk::harness::PERMISSION_REQUEST_KIND;
use echo_agent_computer_hub_sdk::{ToolServer, WeakToolServer};
use echo_agent_tool_protocol::SessionId;
use prometheus::{HistogramVec, IntCounter, register_histogram_vec, register_int_counter};
use serde_json::Value;
use std::sync::LazyLock;
/// Wall-clock time the workspace awaits chat's decision on a `permission_request`
/// hook. `outcome` is `ok` (chat replied) or `error` (transport failure /
/// backstop deadline).
static PERMISSION_REPLY_DURATION: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "echo_agent_workspace_permission_reply_seconds",
        "Wall-clock time awaiting chat's reply to a permission_request hook",
        &["outcome"],
        vec![0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0]
    )
    .expect("echo_agent_workspace_permission_reply_seconds must register once")
});
/// Permission requests whose reply timed out (the server backstop deadline fired).
/// A subset of the histogram's `error` outcome, promoted to its own counter so a
/// stuck/lost reply is distinguishable from other transport failures.
static PERMISSION_TIMEOUT_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        "echo_agent_workspace_permission_timeout_total",
        "permission_request hooks whose reply timed out (backstop deadline fired)"
    )
    .expect("echo_agent_workspace_permission_timeout_total must register once")
});
/// Zero-init this module's metric families. See [`crate::init_metrics`].
pub(crate) fn init_metrics() {
    for outcome in ["ok", "error"] {
        let _ = PERMISSION_REPLY_DURATION.with_label_values(&[outcome]);
    }
    PERMISSION_TIMEOUT_TOTAL.inc_by(0);
}
/// Identifies the reply backstop-deadline timeout by its rendered message; the
/// server SDK exposes no typed timeout variant to match on. If that message text
/// changes, such a reply is recorded under the histogram's `error` outcome but
/// not counted in `permission_timeout_total`.
fn is_timeout_err(msg: &str) -> bool {
    msg.contains("timed out")
}
/// Env var that enables the HITL-live **tool-permission** emit (workspace →
/// chat over the server) for local e2e and gradual rollout. Prefer server capability
/// negotiation long-term; this is the interim gate so tool-permission can be
/// exercised without waiting on that wire format.
pub const HITL_PERMISSION_LIVE_ENV: &str = "ECHO_AGENT_HITL_PERMISSION_LIVE";
/// Whether the HITL-live permission path is enabled.
///
/// Intended long-term gate: the chat flag `echo_agent_chat_enable_hitl_live_path`,
/// propagated by the server at session-bind (capability negotiation). Until that
/// lands, honor [`HITL_PERMISSION_LIVE_ENV`] (`1` / `true` / `yes`) so local
/// stacks and e2e can turn the emit on explicitly. Default remains **off**
/// (fail closed to the local ACP prompt).
pub fn hitl_permission_live_enabled() -> bool {
    match std::env::var(HITL_PERMISSION_LIVE_ENV) {
        Ok(v) => {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        }
        Err(_) => false,
    }
}
/// Sends a `permission_request` hook to chat and awaits the decision reply.
#[async_trait]
pub trait PermissionHookTransport: Send + Sync {
    /// Emit the permission-request `payload` and return chat's decision reply.
    async fn request_permission(&self, payload: Value) -> Result<Value, String>;
}
/// Hub-backed permission transport (weak server handle; upgrades per request).
pub struct ToolServerPermissionTransport {
    server: WeakToolServer,
    session_id: SessionId,
}
impl ToolServerPermissionTransport {
    pub fn new(server: ToolServer, session_id: SessionId) -> Self {
        Self {
            server: server.downgrade(),
            session_id,
        }
    }
    /// Build from a session id held as a string; `None` if it is not a valid
    /// [`SessionId`].
    pub fn from_session_id(server: ToolServer, session_id: &str) -> Option<Self> {
        SessionId::new(session_id)
            .ok()
            .map(|sid| Self::new(server, sid))
    }
}
#[async_trait]
impl PermissionHookTransport for ToolServerPermissionTransport {
    async fn request_permission(&self, payload: Value) -> Result<Value, String> {
        let start = std::time::Instant::now();
        let Some(server) = self.server.upgrade() else {
            PERMISSION_REPLY_DURATION
                .with_label_values(&["error"])
                .observe(start.elapsed().as_secs_f64());
            return Err("tool server gone (weak upgrade failed)".to_owned());
        };
        let raw = server
            .request_hook(
                self.session_id.clone(),
                PERMISSION_REQUEST_KIND.to_owned(),
                payload,
            )
            .await;
        let outcome = match &raw {
            Ok(_) => "ok",
            Err(e) => {
                if is_timeout_err(&e.to_string()) {
                    PERMISSION_TIMEOUT_TOTAL.inc();
                }
                "error"
            }
        };
        PERMISSION_REPLY_DURATION
            .with_label_values(&[outcome])
            .observe(start.elapsed().as_secs_f64());
        raw.map_err(|e| e.to_string())
    }
}
fn scope_for_access(access: &AccessKind) -> &'static str {
    match access {
        AccessKind::Bash(_) | AccessKind::Edit(_) | AccessKind::MCPTool { .. } => "write",
        AccessKind::Read(_)
        | AccessKind::Grep { .. }
        | AccessKind::WebFetch(_)
        | AccessKind::WebSearch(_) => "read",
    }
}
fn describe_access(access: &AccessKind) -> String {
    match access {
        AccessKind::Bash(_) => "Run a terminal command".to_owned(),
        AccessKind::Edit(path) => format!("Edit {path}"),
        AccessKind::MCPTool { name, .. } => format!("Run MCP tool {name}"),
        AccessKind::WebFetch(url) => format!("Fetch {url}"),
        AccessKind::WebSearch(query) => format!("Search the web for {query}"),
        AccessKind::Read(_) => "Read a file".to_owned(),
        AccessKind::Grep { .. } => "Search file contents".to_owned(),
    }
}
/// Build the server → chat `permission_request` payload. The field set matches
/// chat's `PermissionRequestPayload` parser: `tool_call_id`, `tool_name`,
/// `description`, `scope`, and the bash/edit context.
pub(crate) fn build_permission_payload(access: &AccessKind, tool_call_id: &str) -> Value {
    let mut payload = serde_json::json!({
        "tool_call_id": tool_call_id,
        "tool_name": tool_name_for_access(access),
        "description": describe_access(access),
        "scope": scope_for_access(access),
    });
    if let Some(map) = payload.as_object_mut() {
        match access {
            AccessKind::Bash(command) => {
                map.insert("bash_command".to_owned(), Value::from(command.clone()));
            }
            AccessKind::Edit(path) => {
                map.insert(
                    "edit_file_paths".to_owned(),
                    Value::from(vec![path.clone()]),
                );
            }
            _ => {}
        }
    }
    payload
}
/// Decode chat's decision reply onto a [`PromptOutcome`]. The reply is chat's
/// `permission_answer_to_json` output: `{ "outcome", "scope"?, "followup_message"? }`.
/// An unknown / `unspecified` outcome fails closed (reject).
pub(crate) fn reply_to_outcome(reply: &Value) -> PromptOutcome {
    let outcome = match reply.get("outcome") {
        Some(Value::String(s)) => s.as_str(),
        Some(Value::Number(n)) => match n.as_i64() {
            Some(1) => "approve",
            Some(2) => "reject",
            Some(3) => "always_approve",
            Some(4) => "always_reject",
            _ => "",
        },
        _ => "",
    };
    let followup = reply
        .get("followup_message")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match outcome {
        "approve" => PromptOutcome::AllowOnce,
        "always_approve" => match scope_kind_value(reply) {
            Some(("bash_command", Some(value))) => PromptOutcome::AllowAlwaysBashCommand(value),
            Some(("bash_glob", Some(value))) => PromptOutcome::AllowAlwaysBashGlob(value),
            Some(("server_prefix", Some(value))) => PromptOutcome::AllowAlwaysMcpServer(value),
            Some(("domain", Some(value))) => PromptOutcome::AllowAlwaysDomain(value),
            _ => PromptOutcome::AllowAlways,
        },
        "reject" => match followup {
            Some(message) => PromptOutcome::FollowupMessage(message.to_owned()),
            None => PromptOutcome::RejectOnce,
        },
        "always_reject" => match scope_kind_value(reply) {
            Some(("bash_command", Some(value))) => PromptOutcome::RejectAlwaysBashCommand(value),
            _ => PromptOutcome::RejectOnce,
        },
        "cancelled" => PromptOutcome::Cancelled,
        _ => PromptOutcome::RejectOnce,
    }
}
fn scope_kind_value(reply: &Value) -> Option<(&str, Option<String>)> {
    let scope = reply.get("scope")?;
    let kind = scope.get("kind").and_then(Value::as_str)?;
    let value = scope
        .get("value")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some((kind, value))
}
/// Map a hub-served tool name + JSON args onto an [`AccessKind`] for the
/// permission gate in [`crate::hub::SessionRoutedToolHandler`]. Returns `None`
/// for tools that never need a user prompt (reads / todos / dynamic).
pub fn access_kind_for_hub_tool(tool_name: &str, args: &Value) -> Option<AccessKind> {
    let name = tool_name.rsplit(':').next().unwrap_or(tool_name);
    let name = name.strip_prefix("EchoAgentBuild:").unwrap_or(name);
    match name {
        "run_terminal_command" | "run_terminal_cmd" | "bash" | "shell" => {
            let cmd = args
                .get("command")
                .or_else(|| args.get("full_command"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            Some(AccessKind::Bash(cmd))
        }
        "search_replace" | "hashline_edit" | "edit" => {
            let path = args
                .get("file_path")
                .or_else(|| args.get("filePath"))
                .or_else(|| args.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            Some(AccessKind::Edit(path))
        }
        "write" | "write_file" => {
            let path = args
                .get("file_path")
                .or_else(|| args.get("filePath"))
                .or_else(|| args.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            Some(AccessKind::Edit(path))
        }
        "apply_patch" => Some(AccessKind::Edit("apply_patch".to_owned())),
        "task" | "Task" | "spawn_subagent" => {
            let kind = args
                .get("subagent_type")
                .and_then(Value::as_str)
                .unwrap_or("task");
            Some(AccessKind::Edit(format!("task:{kind}")))
        }
        "web_fetch" => {
            let url = args
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            Some(AccessKind::WebFetch(url))
        }
        n if n.contains("__") || n.starts_with("mcp") => Some(AccessKind::MCPTool {
            name: tool_name.to_owned(),
            input: args.clone(),
        }),
        _ => None,
    }
}
/// Whether a [`PromptOutcome`] allows the tool call to proceed.
pub fn prompt_outcome_allows(outcome: &PromptOutcome) -> bool {
    matches!(
        outcome,
        PromptOutcome::AllowOnce
            | PromptOutcome::AllowAlways
            | PromptOutcome::AllowEditsForSession
            | PromptOutcome::AllowAlwaysBashCommand(_)
            | PromptOutcome::AllowAlwaysBashGlob(_)
            | PromptOutcome::AllowAlwaysDomain(_)
            | PromptOutcome::AllowAlwaysMcpTool(_)
            | PromptOutcome::AllowAlwaysMcpServer(_)
    )
}
/// Request a permission decision from chat over `transport` and map the reply
/// to a [`PromptOutcome`]. A transport error fails closed (the manager turns an
/// `Error` outcome into a reject) so a lost server connection never silently runs
/// a guarded tool.
pub async fn request_permission_via_hub(
    transport: &dyn PermissionHookTransport,
    access: &AccessKind,
    tool_call_id: &str,
) -> PromptOutcome {
    let payload = build_permission_payload(access, tool_call_id);
    match transport.request_permission(payload).await {
        Ok(reply) => match reply_to_outcome(&reply) {
            PromptOutcome::AllowAlways if matches!(access, AccessKind::Edit(_)) => {
                PromptOutcome::AllowEditsForSession
            }
            other => other,
        },
        Err(e) => {
            tracing::error!(error = %e, "hub permission request failed; rejecting");
            PromptOutcome::Error(format!("hub permission request failed: {e}"))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    /// Pins the current SDK timeout wording the classifier matches on.
    #[test]
    fn is_timeout_err_matches_backstop_wording_only() {
        assert!(is_timeout_err("request timed out after 600s"));
        assert!(is_timeout_err("request timed out after 600.0s"));
        assert!(!is_timeout_err("connection lost"));
        assert!(!is_timeout_err("tool server gone (weak upgrade failed)"));
    }
    #[test]
    fn payload_for_bash_carries_command_and_write_scope() {
        let payload = build_permission_payload(&AccessKind::Bash("rm -rf /tmp/x".into()), "tc-1");
        assert_eq!(payload["tool_call_id"], "tc-1");
        assert_eq!(payload["tool_name"], "run_terminal_command");
        assert_eq!(payload["description"], "Run a terminal command");
        assert_eq!(payload["scope"], "write");
        assert_eq!(payload["bash_command"], "rm -rf /tmp/x");
        assert!(payload.get("edit_file_paths").is_none());
    }
    #[test]
    fn payload_for_edit_carries_file_paths() {
        let payload = build_permission_payload(&AccessKind::Edit("src/main.rs".into()), "tc-2");
        assert_eq!(payload["tool_name"], "search_replace");
        assert_eq!(payload["description"], "Edit src/main.rs");
        assert_eq!(payload["scope"], "write");
        assert_eq!(
            payload["edit_file_paths"],
            serde_json::json!(["src/main.rs"])
        );
        assert!(payload.get("bash_command").is_none());
        assert!(payload.get("edit_kind").is_none());
    }
    #[test]
    fn hub_gates_edit_and_task() {
        assert!(matches!(
            access_kind_for_hub_tool(
                "opencode:edit",
                &serde_json::json!({
                    "filePath": "/tmp/denied.txt",
                    "oldString": "ORIGINAL",
                    "newString": "BYPASS",
                }),
            ),
            Some(AccessKind::Edit(p)) if p == "/tmp/denied.txt"
        ));
        for name in ["spawn_subagent", "Task", "task"] {
            assert!(
                matches!(
                    access_kind_for_hub_tool(
                        name,
                        &serde_json::json!({
                            "subagent_type": "general-purpose",
                            "prompt": "edit config.toml",
                        }),
                    ),
                    Some(AccessKind::Edit(p)) if p == "task:general-purpose"
                ),
                "hub must gate {name:?}"
            );
        }
    }
    #[test]
    fn payload_for_mcp_has_no_tool_context() {
        let payload = build_permission_payload(
            &AccessKind::MCPTool {
                name: "linear__list".into(),
                input: serde_json::Value::Null,
            },
            "tc-3",
        );
        assert_eq!(payload["tool_name"], "mcp:linear__list");
        assert_eq!(payload["description"], "Run MCP tool linear__list");
        assert_eq!(payload["scope"], "write");
        assert!(payload.get("bash_command").is_none());
        assert!(payload.get("edit_file_paths").is_none());
    }
    #[test]
    fn reply_outcomes_map_to_prompt_outcomes() {
        assert!(matches!(
            reply_to_outcome(&serde_json::json!({ "outcome": "approve" })),
            PromptOutcome::AllowOnce
        ));
        assert!(matches!(
            reply_to_outcome(&serde_json::json!({ "outcome": "reject" })),
            PromptOutcome::RejectOnce
        ));
        assert!(matches!(
            reply_to_outcome(&serde_json::json!({ "outcome": "cancelled" })),
            PromptOutcome::Cancelled
        ));
        assert!(matches!(
            reply_to_outcome(&serde_json::json!({ "outcome": "unspecified" })),
            PromptOutcome::RejectOnce
        ));
        assert!(matches!(
            reply_to_outcome(&serde_json::json!({})),
            PromptOutcome::RejectOnce
        ));
    }
    #[test]
    fn reject_with_followup_routes_message_to_model() {
        let reply =
            serde_json::json!({ "outcome": "reject", "followup_message": "use cargo instead" });
        match reply_to_outcome(&reply) {
            PromptOutcome::FollowupMessage(m) => assert_eq!(m, "use cargo instead"),
            other => panic!("expected FollowupMessage, got {other:?}"),
        }
    }
    #[test]
    fn always_approve_maps_scope_to_persistent_outcome() {
        let bash = serde_json::json!({
            "outcome": "always_approve",
            "scope": { "kind": "bash_command", "value": "cargo build" },
        });
        match reply_to_outcome(&bash) {
            PromptOutcome::AllowAlwaysBashCommand(v) => assert_eq!(v, "cargo build"),
            other => panic!("expected AllowAlwaysBashCommand, got {other:?}"),
        }
        let server = serde_json::json!({
            "outcome": "always_approve",
            "scope": { "kind": "server_prefix", "value": "linear" },
        });
        match reply_to_outcome(&server) {
            PromptOutcome::AllowAlwaysMcpServer(v) => assert_eq!(v, "linear"),
            other => panic!("expected AllowAlwaysMcpServer, got {other:?}"),
        }
        assert!(matches!(
            reply_to_outcome(&serde_json::json!({ "outcome": "always_approve" })),
            PromptOutcome::AllowAlways
        ));
    }
    #[test]
    fn always_reject_with_bash_scope_persists_the_denied_prefix() {
        let reply = serde_json::json!({
            "outcome": "always_reject",
            "scope": { "kind": "bash_command", "value": "curl" },
        });
        match reply_to_outcome(&reply) {
            PromptOutcome::RejectAlwaysBashCommand(v) => assert_eq!(v, "curl"),
            other => panic!("expected RejectAlwaysBashCommand, got {other:?}"),
        }
    }
    struct StubTransport {
        reply: Result<Value, String>,
        seen: Mutex<Option<Value>>,
    }
    #[async_trait]
    impl PermissionHookTransport for StubTransport {
        async fn request_permission(&self, payload: Value) -> Result<Value, String> {
            *self.seen.lock().unwrap() = Some(payload);
            self.reply.clone()
        }
    }
    #[tokio::test]
    async fn request_sends_payload_and_decodes_reply() {
        let transport = StubTransport {
            reply: Ok(serde_json::json!({ "outcome": "approve" })),
            seen: Mutex::new(None),
        };
        let outcome =
            request_permission_via_hub(&transport, &AccessKind::Bash("ls -la".into()), "tc-7")
                .await;
        assert!(matches!(outcome, PromptOutcome::AllowOnce));
        let seen = transport
            .seen
            .lock()
            .unwrap()
            .clone()
            .expect("payload sent");
        assert_eq!(seen["tool_call_id"], "tc-7");
        assert_eq!(seen["bash_command"], "ls -la");
    }
    #[tokio::test]
    async fn transport_error_fails_closed() {
        let transport = StubTransport {
            reply: Err("connection lost".to_owned()),
            seen: Mutex::new(None),
        };
        let outcome =
            request_permission_via_hub(&transport, &AccessKind::Edit("a.rs".into()), "tc-8").await;
        assert!(matches!(outcome, PromptOutcome::Error(_)));
    }
    #[tokio::test]
    async fn edit_always_approve_maps_to_session_scope() {
        let transport = StubTransport {
            reply: Ok(serde_json::json!({ "outcome": "always_approve" })),
            seen: Mutex::new(None),
        };
        let outcome =
            request_permission_via_hub(&transport, &AccessKind::Edit("a.rs".into()), "tc-9").await;
        assert!(matches!(outcome, PromptOutcome::AllowEditsForSession));
        let transport = StubTransport {
            reply: Ok(serde_json::json!({ "outcome": "always_approve" })),
            seen: Mutex::new(None),
        };
        let outcome = request_permission_via_hub(
            &transport,
            &AccessKind::MCPTool {
                name: "x".into(),
                input: serde_json::Value::Null,
            },
            "tc-10",
        )
        .await;
        assert!(matches!(outcome, PromptOutcome::AllowAlways));
    }
    #[test]
    fn hitl_permission_live_defaults_off_without_env() {
        if std::env::var(HITL_PERMISSION_LIVE_ENV).is_err() {
            assert!(!hitl_permission_live_enabled());
        }
    }
}
