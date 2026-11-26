//! `echo.agent/hooks/*` extension handlers.
//!
//! The file-hook list/action endpoints for the pager's hooks modal, plus the
//! client-registered hook wire types and `parse_client_hooks`.

use std::collections::HashMap;

use agent_client_protocol as acp;
use echo_agent_hooks::event::{HookEventEnvelope, HookEventName};
use echo_agent_hooks::matcher::HookMatcher;
use echo_agent_hooks_plugins_types::{HookEvent, HookHandlerType, HookInfo};
use serde::Deserialize;

use crate::agent::MvpAgent;

type ExtResult = Result<acp::ExtResponse, acp::Error>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRequest {
    session_id: String,
}

pub(crate) fn hook_spec_to_info(spec: &echo_agent_hooks::config::HookSpec) -> HookInfo {
    use echo_agent_hooks::event::HookEventName;

    let event = match spec.event {
        // Session lifecycle
        HookEventName::SessionStart => HookEvent::SessionStart,
        HookEventName::SessionEnd => HookEvent::SessionEnd,
        HookEventName::Stop => HookEvent::Stop,
        HookEventName::StopFailure => HookEvent::StopFailure,
        HookEventName::StopCancelled => HookEvent::StopCancelled,
        // Tool events
        HookEventName::PreToolUse => HookEvent::PreToolUse,
        HookEventName::PostToolUse => HookEvent::PostToolUse,
        HookEventName::PostToolUseFailure => HookEvent::PostToolUseFailure,
        HookEventName::PermissionDenied => HookEvent::PermissionDenied,
        // User / notification
        HookEventName::UserPromptSubmit => HookEvent::UserPromptSubmit,
        HookEventName::Notification => HookEvent::Notification,
        // Subagent
        HookEventName::SubagentStart => HookEvent::SubagentStart,
        HookEventName::SubagentStop | HookEventName::SubagentEnd => HookEvent::SubagentStop,
        // Compaction
        HookEventName::PreCompact => HookEvent::PreCompact,
        HookEventName::PostCompact => HookEvent::PostCompact,
    };

    let handler_type = if spec.url.is_some() {
        HookHandlerType::Http
    } else {
        HookHandlerType::Command
    };

    // Display the pre-expansion source string when available so the
    // pager UI / ACP DTO never leaks values resolved from the user
    // `env` map (which may contain secrets like API tokens). Fall back
    // to the post-expansion form for any future code path that builds
    // a `HookSpec` without populating the raw source.
    let command_display = spec
        .command_raw
        .clone()
        .or_else(|| spec.command.as_ref().map(|p| p.display().to_string()));
    let url_display = spec.url_raw.clone().or_else(|| spec.url.clone());

    HookInfo {
        name: spec.name.clone(),
        event,
        handler_type,
        matcher: spec.configured_matcher.clone(),
        command: command_display,
        url: url_display,
        timeout_ms: spec.timeout_ms,
        source_dir: spec.source_dir.display().to_string(),
        disabled: echo_agent_hooks::trust::is_hook_disabled(&spec.name),
    }
}

// Wire types for client-registered hooks (`echo.agent/hooks/run`); the gate that uses
// them lives in `session::acp_session::hooks`.

/// A matcher group from the client's registration: `{ matcher, hookCallbackIds, timeout }`.
///
/// `pub` (not `pub(crate)`) because [`ClientHooks`] flows through the public
/// `SessionCommand::SnapshotClientHooks` so subagents can inherit the parent's hooks.
#[derive(Debug, Clone)]
pub struct ClientHookGroup {
    /// `None` (wire `null`, `""`, or `"*"`) matches every tool.
    pub matcher: Option<HookMatcher>,
    pub callback_ids: Vec<String>,
    /// Per-group gate reply deadline (wire seconds); `None` uses the default.
    pub timeout: Option<std::time::Duration>,
}

pub(crate) type ClientHooks = HashMap<HookEventName, Vec<ClientHookGroup>>;

/// One hook dispatched to a client callback: the shared [`HookEventEnvelope`]
/// (flattened, camelCase) plus the `hookCallbackId` it targets. The same shape is sent
/// for both the `echo.agent/hooks/run` request (gate) and the `echo.agent/hooks/event` notification
/// (observe-only), so the client decodes one payload for every hook.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientHookDispatch<'a> {
    pub hook_callback_id: &'a str,
    #[serde(flatten)]
    pub envelope: &'a HookEventEnvelope,
}

pub(crate) const ADVERTISED_BLOCKING_EVENTS: &[echo_agent_hooks::event::HookEventName] = &[
    echo_agent_hooks::event::HookEventName::PreToolUse,
    echo_agent_hooks::event::HookEventName::Stop,
    echo_agent_hooks::event::HookEventName::SubagentStop,
];

pub(crate) const ADVERTISED_DECISIONS: &[&str] = &["deny", "block"];

pub(crate) const ADVERTISED_STOP_SIGNALS: &[&str] =
    &["continue", "stopReason", "additionalContext"];

/// Only `Deny` blocks; every other value proceeds (fail-open).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClientHookDecision {
    #[default]
    Continue,
    #[serde(alias = "block")]
    Deny,
    #[serde(other)]
    Other,
}

/// Response payload for `echo.agent/hooks/run` (client to agent). `Default` (used on
/// timeout, transport error, or a malformed reply) proceeds.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientHookResponse {
    #[serde(default)]
    pub decision: ClientHookDecision,
    #[serde(default, alias = "reason")]
    pub system_message: Option<String>,
    #[serde(default, rename = "continue")]
    pub continue_: Option<bool>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub additional_context: Option<String>,
}

/// Parse client hooks from `session/new` `_meta["echo.agent/hooks"]`, shaped
/// `{ "<Event>": [{ matcher, hookCallbackIds }] }` (PascalCase or snake_case
/// events). Each `matcher` is compiled with the agent's [`HookMatcher`] so client
/// and file hooks match identically. Unknown events, malformed groups, invalid
/// matchers, and callback-less groups are skipped; absent meta yields no hooks.
pub(crate) fn parse_client_hooks(meta: Option<&acp::Meta>) -> ClientHooks {
    let mut hooks = ClientHooks::new();
    let Some(map) = meta
        .and_then(|m| m.get("echo.agent/hooks"))
        .and_then(|h| h.as_object())
    else {
        return hooks;
    };
    for (event_name, value) in map {
        let de = serde::de::value::StrDeserializer::<serde::de::value::Error>::new(event_name);
        let Ok(event) = HookEventName::deserialize(de) else {
            tracing::warn!(event = %event_name, "ignoring unknown echo.agent/hooks event");
            continue;
        };
        let Some(array) = value.as_array() else {
            tracing::warn!(event = %event_name, "echo.agent/hooks event value is not an array; skipping");
            continue;
        };
        let groups: Vec<ClientHookGroup> = array
            .iter()
            .filter_map(|group| parse_hook_group(event, group))
            .collect();
        if !groups.is_empty() {
            // Key by the canonical event so a registration under an alias (e.g.
            // `SubagentEnd`) still matches the event the agent fires (`SubagentStop`).
            hooks.entry(event.canonical()).or_default().extend(groups);
        }
    }
    hooks
}

/// Hooks to apply on a `load_session` reconnect: `Some` (possibly empty, an explicit
/// clear) when the request meta carries `echo.agent/hooks`, else `None` so a reconnect that
/// omits the key leaves the live registrations from `session/new` untouched.
pub(crate) fn reconnect_client_hooks(meta: Option<&acp::Meta>) -> Option<ClientHooks> {
    meta.and_then(|m| m.get("echo.agent/hooks"))
        .map(|_| parse_client_hooks(meta))
}

/// Parse one `{ matcher, hookCallbackIds }` registration entry. Returns `None`
/// (with a warning) when the entry is malformed, carries no callback ids, or its
/// matcher fails to compile.
fn parse_hook_group(event: HookEventName, value: &serde_json::Value) -> Option<ClientHookGroup> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct WireGroup {
        #[serde(default)]
        matcher: Option<String>,
        #[serde(default)]
        hook_callback_ids: Vec<String>,
        /// Per-group gate timeout in seconds.
        #[serde(default)]
        timeout: Option<f64>,
    }

    let group = WireGroup::deserialize(value)
        .inspect_err(
            |err| tracing::warn!(%event, %err, "ignoring malformed echo.agent/hooks group"),
        )
        .ok()?;
    if group.hook_callback_ids.is_empty() {
        tracing::warn!(%event, "ignoring echo.agent/hooks group with no hookCallbackIds");
        return None;
    }
    // Drop a non-finite/non-positive timeout (fall back to the default gate timeout) and
    // cap it so a client can't make a tool hang on the gate for an unbounded time.
    const MAX_HOOK_TIMEOUT_SECS: f64 = 600.0;
    let timeout = group
        .timeout
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| std::time::Duration::from_secs_f64(s.min(MAX_HOOK_TIMEOUT_SECS)));
    let matcher = match group.matcher.as_deref() {
        // Match-all tokens map to no matcher (group always fires). `HookMatcher::new`
        // also treats these as match-all; short-circuiting here keeps the intent explicit.
        None | Some("") | Some("*") => None,
        // Same policy as file hooks (`MatcherPolicy::Ignored`): warn and drop
        // the matcher rather than let the registration appear scoped.
        Some(pattern)
            if event.traits().matcher == echo_agent_hooks::event::MatcherPolicy::Ignored =>
        {
            tracing::warn!(%event, pattern, "matcher on a {event} hook group is ignored (this event always fires)");
            None
        }
        Some(pattern) => match HookMatcher::new(pattern) {
            Ok(matcher) => Some(matcher),
            Err(err) => {
                tracing::warn!(%event, pattern, %err, "ignoring echo.agent/hooks group with invalid matcher");
                return None;
            }
        },
    };
    Some(ClientHookGroup {
        matcher,
        callback_ids: group.hook_callback_ids,
        timeout,
    })
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/hooks/list" => {
            let req: ListRequest = super::parse_params(args)?;
            let sid = acp::SessionId::new(req.session_id);

            let result = agent
                .list_hooks(&sid)
                .await
                .ok_or_else(|| anyhow::anyhow!("session not found"));
            super::to_ext_response(result)
        }
        "echo.agent/hooks/action" => {
            let req: echo_agent_hooks_plugins_types::HooksActionRequest =
                super::parse_params(args)?;
            let sid = acp::SessionId::new(req.session_id);

            let result = agent
                .execute_hooks_action(&sid, req.action)
                .await
                .ok_or_else(|| anyhow::anyhow!("session not found"));
            super::to_ext_response(result)
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_hooks::config::HookSpec;
    use echo_agent_hooks::event::HookEventName;
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Minimal `HookSpec` for `hook_spec_to_info` tests (`handler_type` is unused;
    /// the DTO derives it from `url`).
    fn make_spec(
        command_raw: Option<&str>,
        command: Option<&str>,
        url_raw: Option<&str>,
        url: Option<&str>,
    ) -> HookSpec {
        HookSpec {
            name: "test:pre_tool_use[0].hooks[0]".to_string(),
            event: HookEventName::PreToolUse,
            handler_type: echo_agent_hooks::config::HandlerType::Command,
            configured_matcher: None,
            matcher: None,
            enabled: true,
            command: command.map(PathBuf::from),
            command_raw: command_raw.map(str::to_string),
            url: url.map(str::to_string),
            url_raw: url_raw.map(str::to_string),
            timeout_ms: 5000,
            source_dir: PathBuf::from("/tmp"),
            extra_env: HashMap::new(),
            layer: echo_agent_hooks::config::HookProvenance::File,
        }
    }

    /// `*_raw` (pre-expansion) wins over the resolved value so secrets never reach
    /// the DTO; then the resolved value, else `None`. Same for `command` and `url`.
    #[test]
    fn hook_spec_to_info_display_precedence() {
        let command =
            |raw, resolved| hook_spec_to_info(&make_spec(raw, resolved, None, None)).command;
        assert_eq!(
            command(Some("${VAR}/x"), Some("/resolved/x")).as_deref(),
            Some("${VAR}/x")
        );
        assert_eq!(
            command(None, Some("/legacy/x")).as_deref(),
            Some("/legacy/x")
        );
        assert!(command(None, None).is_none());

        let url = |raw, resolved| hook_spec_to_info(&make_spec(None, None, raw, resolved)).url;
        assert_eq!(
            url(
                Some("https://${HOST}/p?token=${TOKEN}"),
                Some("https://api/p?token=ghp_X")
            )
            .as_deref(),
            Some("https://${HOST}/p?token=${TOKEN}"),
        );
        assert_eq!(
            url(None, Some("https://h/c")).as_deref(),
            Some("https://h/c")
        );
        assert!(url(None, None).is_none());
    }

    #[test]
    fn parse_client_hooks_parses_valid_groups() {
        let meta = serde_json::json!({
            "echo.agent/hooks": {
                "PreToolUse": [
                    { "matcher": "run_terminal_command", "hookCallbackIds": ["cb_0"] },
                    { "matcher": null, "hookCallbackIds": ["cb_1"] },
                    { "matcher": "*", "hookCallbackIds": ["cb_2"] }
                ],
                "post_tool_use": [{ "hookCallbackIds": ["cb_3"] }]
            }
        });
        let hooks = parse_client_hooks(meta.as_object());

        let pre = &hooks[&HookEventName::PreToolUse];
        assert_eq!(pre.len(), 3);
        assert_eq!(pre[0].callback_ids, ["cb_0"]);
        let matcher = pre[0].matcher.as_ref().unwrap();
        assert!(matcher.is_match("run_terminal_command"));
        assert!(!matcher.is_match("read_file"));
        assert!(pre[1].matcher.is_none()); // null / "*" = match-all
        assert!(pre[2].matcher.is_none());
        assert!(hooks.contains_key(&HookEventName::PostToolUse)); // snake_case resolves
    }

    #[test]
    fn parse_client_hooks_drops_invalid_and_absent() {
        assert!(parse_client_hooks(None).is_empty());
        assert!(
            parse_client_hooks(serde_json::json!({ "askUserQuestion": true }).as_object())
                .is_empty()
        );

        let meta = serde_json::json!({
            "NotARealEvent": [{ "hookCallbackIds": ["x"] }],
            "echo.agent/hooks": {
                "PreToolUse": [
                    { "matcher": "[invalid", "hookCallbackIds": ["bad_regex"] },
                    { "matcher": "run_terminal_command", "hookCallbackIds": [] },
                    { "matcher": "read_file", "hookCallbackIds": ["good"] }
                ]
            }
        });
        let groups = &parse_client_hooks(meta.as_object())[&HookEventName::PreToolUse];
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].callback_ids, ["good"]);
    }

    /// A group's `timeout` (seconds) parses to a `Duration`; absent or non-positive falls
    /// back to the default gate timeout (`None`).
    #[test]
    fn parse_client_hooks_reads_group_timeout() {
        let meta = serde_json::json!({
            "echo.agent/hooks": {
                "PreToolUse": [
                    { "hookCallbackIds": ["a"], "timeout": 5.0 },
                    { "hookCallbackIds": ["b"], "timeout": 0 },
                    { "hookCallbackIds": ["c"] },
                    { "hookCallbackIds": ["d"], "timeout": 100000 }
                ]
            }
        });
        let groups = &parse_client_hooks(meta.as_object())[&HookEventName::PreToolUse];
        assert_eq!(groups[0].timeout, Some(std::time::Duration::from_secs(5)));
        assert_eq!(groups[1].timeout, None); // non-positive -> default
        assert_eq!(groups[2].timeout, None); // absent -> default
        assert_eq!(groups[3].timeout, Some(std::time::Duration::from_secs(600))); // capped
    }

    /// A registration under the `SubagentEnd` alias must land on the canonical
    /// `SubagentStop` key the agent fires.
    #[test]
    fn parse_client_hooks_canonicalizes_subagent_alias() {
        let meta = serde_json::json!({
            "echo.agent/hooks": { "SubagentEnd": [{ "hookCallbackIds": ["cb"] }] }
        });
        let hooks = parse_client_hooks(meta.as_object());
        assert!(hooks.contains_key(&HookEventName::SubagentStop));
        assert!(!hooks.contains_key(&HookEventName::SubagentEnd));
    }

    /// Reconnect refresh applies hooks only when the load meta carries `echo.agent/hooks`:
    /// an absent key returns `None` (don't wipe `session/new` registrations); a present
    /// key returns `Some` (an empty object is an explicit clear).
    #[test]
    fn reconnect_client_hooks_only_when_key_present() {
        assert!(reconnect_client_hooks(None).is_none());
        assert!(reconnect_client_hooks(serde_json::json!({ "other": true }).as_object()).is_none());

        let cleared =
            reconnect_client_hooks(serde_json::json!({ "echo.agent/hooks": {} }).as_object());
        assert!(cleared.is_some_and(|h| h.is_empty()));

        let set = reconnect_client_hooks(
            serde_json::json!({
                "echo.agent/hooks": { "PreToolUse": [{ "hookCallbackIds": ["cb"] }] }
            })
            .as_object(),
        );
        assert!(set.is_some_and(|h| h.contains_key(&HookEventName::PreToolUse)));
    }

    /// `deny` parses to `Deny` (+ optional message); everything else fails open:
    /// unknown values to `Other`, missing/empty/default to `Continue`.
    #[test]
    fn client_hook_response_deserialization() {
        let deny: ClientHookResponse =
            serde_json::from_str(r#"{"decision":"deny","systemMessage":"blocked"}"#).unwrap();
        assert_eq!(deny.decision, ClientHookDecision::Deny);
        assert_eq!(deny.system_message.as_deref(), Some("blocked"));

        let unknown: ClientHookResponse =
            serde_json::from_str(r#"{"decision":"maybe_later"}"#).unwrap();
        assert_eq!(unknown.decision, ClientHookDecision::Other);

        let empty: ClientHookResponse = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.decision, ClientHookDecision::Continue);
        assert!(empty.system_message.is_none());
        assert_eq!(
            ClientHookResponse::default().decision,
            ClientHookDecision::Continue
        );

        let stop: ClientHookResponse = serde_json::from_str(
            r#"{"continue":false,"stopReason":"budget","additionalContext":"ctx"}"#,
        )
        .unwrap();
        assert_eq!(stop.decision, ClientHookDecision::Continue);
        assert_eq!(stop.continue_, Some(false));
        assert_eq!(stop.stop_reason.as_deref(), Some("budget"));
        assert_eq!(stop.additional_context.as_deref(), Some("ctx"));

        // Literal stop-hook output parses on the raw wire: `block` aliases
        // `deny` and `reason` aliases `systemMessage`.
        let blocked: ClientHookResponse =
            serde_json::from_str(r#"{"decision":"block","reason":"run the tests"}"#).unwrap();
        assert_eq!(blocked.decision, ClientHookDecision::Deny);
        assert_eq!(blocked.system_message.as_deref(), Some("run the tests"));
    }

    #[test]
    fn advertised_blocking_events_are_gates() {
        use echo_agent_hooks::event::GateKind;
        for event in ADVERTISED_BLOCKING_EVENTS {
            assert_ne!(
                event.traits().gate,
                GateKind::Observe,
                "advertised blocking event {event:?} has no decision gate"
            );
        }
    }

    #[test]
    fn advertised_capabilities_match_response_parser() {
        for decision in ADVERTISED_DECISIONS {
            let parsed: ClientHookDecision =
                serde_json::from_value(serde_json::json!(decision)).unwrap();
            assert_eq!(
                parsed,
                ClientHookDecision::Deny,
                "advertised decision {decision:?} must parse as a blocking decision"
            );
        }

        let signal_values = serde_json::json!({
            "continue": false,
            "stopReason": "r",
            "additionalContext": "c",
        });
        for signal in ADVERTISED_STOP_SIGNALS {
            let response: ClientHookResponse = serde_json::from_value(
                serde_json::json!({ *signal: signal_values[*signal].clone() }),
            )
            .unwrap();
            let captured = match *signal {
                "continue" => response.continue_ == Some(false),
                "stopReason" => response.stop_reason.as_deref() == Some("r"),
                "additionalContext" => response.additional_context.as_deref() == Some("c"),
                other => panic!("unknown advertised stop signal {other:?}"),
            };
            assert!(captured, "advertised stop signal {signal:?} was not parsed");
        }
    }

    /// The callback id sits beside the flattened envelope (camelCase keys,
    /// `hookEventName` snake_case); the one shape sent for both run and event.
    #[test]
    fn client_hook_dispatch_serializes_envelope() {
        use echo_agent_hooks::event::{HookEventEnvelope, HookPayload};

        let envelope = HookEventEnvelope {
            hook_event_name: HookEventName::PreToolUse,
            session_id: "s1".into(),
            cwd: "/work".into(),
            workspace_root: "/work".into(),
            timestamp: "t".into(),
            transcript_path: None,
            client_identifier: None,
            prompt_id: None,
            permission_mode: Some("default".into()),
            payload: HookPayload::PreToolUse {
                tool_name: "run_terminal_command".into(),
                tool_use_id: "call_1".into(),
                tool_input: serde_json::json!({ "command": "ls" }),
                tool_input_truncated: true,
                subagent_type: None,
            },
        };
        let dispatch = ClientHookDispatch {
            hook_callback_id: "cb_0",
            envelope: &envelope,
        };
        let value = serde_json::to_value(&dispatch).unwrap();
        assert_eq!(value["hookCallbackId"], "cb_0");
        assert_eq!(value["hookEventName"], "pre_tool_use");
        assert_eq!(value["sessionId"], "s1");
        assert_eq!(value["cwd"], "/work");
        assert_eq!(value["toolUseId"], "call_1");
        assert_eq!(value["toolName"], "run_terminal_command");
        assert_eq!(value["toolInput"]["command"], "ls");
        assert_eq!(value["toolInputTruncated"], true);
        assert_eq!(value["permissionMode"], "default");
    }
}
