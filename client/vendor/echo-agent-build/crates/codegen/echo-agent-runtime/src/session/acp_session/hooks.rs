//! Client-registered hooks for [`SessionActor`].
//!
//! Hooks registered at `session/new` (`_meta["echo.agent/hooks"]`) come in two flavors,
//! both matched by the agent ([`echo_agent_hooks::matcher::HookMatcher`], shared with
//! file hooks):
//! - **Gates** (awaited reverse *requests* `echo.agent/hooks/run`):
//!   - `PreToolUse`: a `deny` blocks the tool.
//!   - `Stop` / `SubagentStop` (turn-end gate): a `deny` blocks the agent from
//!     stopping (its `systemMessage` becomes the feedback), `continue: false`
//!     (+ `stopReason`) force-stops overriding blocks, and `additionalContext`
//!     keeps the agent working with non-error feedback: the same vocabulary
//!     file hooks produce, aggregated in [`Self::run_stop_client_hooks`].
//! - **All other events**: fire-and-forget *notifications* `echo.agent/hooks/event`,
//!   observe-only (the callback's return is ignored). Sent per matching callback.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol as acp;
use agent_client_protocol::Client as _;
use echo_agent_hooks::event::{HookEventEnvelope, HookEventName, HookPayload};
use echo_agent_telemetry::events::ClientHookGateOutcome;
use futures::stream::{FuturesUnordered, StreamExt as _};
use serde_json::value::RawValue;

use super::{SessionActor, ToolLoop};
use crate::extensions::hooks::{
    ClientHookDecision, ClientHookDispatch, ClientHookGroup, ClientHookResponse,
};
use crate::sampling::types::ToolCallResponse;

const HOOK_EVENT_METHOD: &str = "echo.agent/hooks/event";
const HOOK_RUN_METHOD: &str = "echo.agent/hooks/run";

/// Default reply deadline for the `PreToolUse` client gate: short because it
/// sits in the interactive tool hot path. On timeout the gate fails open (the
/// tool proceeds). Stop gates use `CLIENT_STOP_GATE_TIMEOUT` instead.
const CLIENT_HOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// Default reply deadline for the `Stop`/`SubagentStop` client gate. A
/// timed-out gate fails open (the agent stops), so too short a default would
/// silently drop a ported goal policy that runs a build or test suite.
const CLIENT_STOP_GATE_TIMEOUT: Duration = Duration::from_secs(600);

/// Outcome of the `echo.agent/hooks/run` reverse request, before interpreting it as a
/// decision. Separate so [`classify`] stays pure and unit-testable.
enum ReverseOutcome {
    Responded(Arc<RawValue>),
    Transport(acp::Error),
    Timeout,
}

/// Map a reverse-request outcome to a decision. Malformed / transport / timeout
/// all fail open (the `Default` response = proceed).
fn classify(outcome: ReverseOutcome) -> (ClientHookResponse, ClientHookGateOutcome) {
    match outcome {
        ReverseOutcome::Responded(raw) => {
            match serde_json::from_str::<ClientHookResponse>(raw.get()) {
                Ok(resp) => {
                    // An unknown `decision` fails open like Continue, but is almost always a
                    // client bug (typo / version skew), so surface it rather than allow silently.
                    let label = match resp.decision {
                        ClientHookDecision::Deny => ClientHookGateOutcome::Denied,
                        ClientHookDecision::Other => {
                            tracing::warn!(
                                "echo.agent/hooks/run returned an unknown decision value; failing open"
                            );
                            ClientHookGateOutcome::UnknownDecision
                        }
                        ClientHookDecision::Continue => ClientHookGateOutcome::Proceeded,
                    };
                    (resp, label)
                }
                Err(err) => {
                    tracing::warn!(%err, "malformed echo.agent/hooks/run response; failing open");
                    (
                        ClientHookResponse::default(),
                        ClientHookGateOutcome::Malformed,
                    )
                }
            }
        }
        ReverseOutcome::Transport(err) => {
            tracing::warn!(%err, "echo.agent/hooks/run transport error (no client wired?); failing open");
            (
                ClientHookResponse::default(),
                ClientHookGateOutcome::TransportError,
            )
        }
        ReverseOutcome::Timeout => {
            tracing::warn!("echo.agent/hooks/run timed out; failing open");
            (
                ClientHookResponse::default(),
                ClientHookGateOutcome::TimedOut,
            )
        }
    }
}

/// Callback ids that fire for an event, in registration order.
fn matching_callback_ids<'a>(
    groups: &'a [ClientHookGroup],
    match_value: Option<&str>,
) -> Vec<&'a str> {
    groups
        .iter()
        .filter(|group| {
            echo_agent_hooks::matcher::matcher_allows(group.matcher.as_ref(), match_value)
        })
        .flat_map(|group| group.callback_ids.iter().map(String::as_str))
        .collect()
}

/// Serialize a [`ClientHookDispatch`] to reverse-RPC params, or `None` (logged) on the
/// should-never-happen serialization failure; callers then skip that callback (fail open)
/// rather than panic the session actor.
fn dispatch_params(dispatch: &ClientHookDispatch<'_>) -> Option<Arc<RawValue>> {
    serde_json::value::to_raw_value(dispatch)
        .inspect_err(|err| tracing::warn!(%err, "failed to serialize client hook dispatch"))
        .ok()
        .map(Into::into)
}

impl SessionActor {
    /// Build a [`HookEventEnvelope`] with this session's common fields filled (session id,
    /// cwd, workspace root, timestamp). Single source of truth for envelope shape; every
    /// fire site goes through here. The event name is canonicalized so alias
    /// fire sites (`SubagentEnd`) serialize the canonical `hookEventName`.
    pub(super) fn make_hook_envelope(
        &self,
        hook_event_name: HookEventName,
        prompt_id: Option<String>,
        payload: HookPayload,
    ) -> HookEventEnvelope {
        HookEventEnvelope {
            hook_event_name: hook_event_name.canonical(),
            session_id: self.session_id_string(),
            cwd: self.session_info.cwd.clone(),
            workspace_root: self.hook_workspace_root(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            transcript_path: self.get_transcript_path(),
            client_identifier: None,
            prompt_id,
            permission_mode: Some(self.permission_mode_label().to_string()),
            payload,
        }
    }

    pub(super) fn is_hook_execution_allowed(&self) -> bool {
        !self.is_readonly_turn() && !self.desktop_document_turn.get()
            && !self.startup_hints.inherited_turn_authority.as_ref().is_some_and(|authority|authority.document_tools)
            // A desktop session is created before its first prompt declares
            // capabilities. Startup/idle hooks cannot execute on that gap.
            && !(self.client_identifier.as_deref()==Some("echo-agent-desktop")
                && self.current_prompt_id.lock().unwrap().is_none())
    }
    pub(super) fn allowed_hook_registry(
        &self,
    ) -> Option<std::sync::Arc<echo_agent_hooks::discovery::HookRegistry>> {
        self.is_hook_execution_allowed()
            .then(|| self.hook_registry.borrow().clone())
            .flatten()
    }
    /// Whether any hook source could consume `event`, letting the hot path skip
    /// building a payload when nothing is listening. Deliberately coarse: any
    /// on-disk registry activates every event (see [`Self::has_enabled_hooks_for`] for
    /// the precise check), while client hooks are checked per event.
    pub(super) fn may_have_hooks_for(&self, event: HookEventName) -> bool {
        if !self.is_hook_execution_allowed() {
            return false;
        }
        self.hook_registry.borrow().is_some()
            || self.client_hooks.borrow().contains_key(&event.canonical())
    }

    /// Whether a hook is registered and enabled for `event` specifically, checking the on-disk
    /// registry per event rather than treating any registry as active.
    pub(super) fn has_enabled_hooks_for(&self, event: HookEventName) -> bool {
        if !self.is_hook_execution_allowed() {
            return false;
        }
        self.hook_registry
            .borrow()
            .as_ref()
            .is_some_and(|registry| registry.has_enabled_hooks_for_canonical(event))
            || self.client_hooks.borrow().contains_key(&event.canonical())
    }

    /// Build the envelope for an observe-only event, fire observe client hooks for it, and
    /// return the envelope for any subsequent file-hook dispatch. One call so a fire site
    /// can't build the envelope but forget to notify.
    pub(super) fn fire_hook(
        &self,
        hook_event_name: HookEventName,
        prompt_id: Option<String>,
        payload: HookPayload,
    ) -> HookEventEnvelope {
        let envelope = self.make_hook_envelope(hook_event_name, prompt_id, payload);
        self.notify_client_hooks(&envelope);
        envelope
    }

    /// Block a tool call denied by a `PreToolUse` hook (file- or client-side),
    /// emitting the shared telemetry + UI side-effects and returning the
    /// [`ToolLoop::HookDenied`] the caller should propagate.
    pub(super) async fn deny_tool(
        &self,
        model_call_id: &str,
        tool_call_id: &acp::ToolCallId,
        tool_name: String,
        hook_name: String,
        reason: String,
    ) -> Result<ToolLoop, acp::Error> {
        tracing::info!(%tool_name, %hook_name, %reason, "tool call denied by pre_tool_use hook");
        echo_agent_telemetry::session_ctx::log_event(echo_agent_telemetry::events::HookBlocked {
            hook_name: hook_name.clone(),
        });
        self.handle_tool_not_executed(
            model_call_id,
            tool_call_id,
            format!("Hook denied: {reason}"),
        )
        .await?;
        self.send_hook_annotation(&format!(
            "\u{26a0} `{tool_name}` blocked by hook `{hook_name}`: {reason}"
        ))
        .await;
        Ok(ToolLoop::HookDenied { hook_name })
    }

    /// Fan one `echo.agent/hooks/run` gate dispatch out to every matching callback,
    /// yielding `(callback_id, response)` in completion order. Independent
    /// per-callback timeouts stop one slow callback starving another; timeout,
    /// transport error, and malformed replies fail open per callback.
    fn client_gate_responses<'a>(
        &'a self,
        groups: &'a [ClientHookGroup],
        tool_name: Option<&'a str>,
        envelope: &'a HookEventEnvelope,
    ) -> FuturesUnordered<
        impl Future<Output = (&'a str, ClientHookResponse, Duration, ClientHookGateOutcome)> + 'a,
    > {
        let default_timeout =
            if envelope.hook_event_name.traits().gate == echo_agent_hooks::event::GateKind::Stop {
                CLIENT_STOP_GATE_TIMEOUT
            } else {
                CLIENT_HOOK_TIMEOUT
            };
        // Dedupe callback ids registered in multiple groups: one dispatch each.
        let mut seen = std::collections::HashSet::new();
        groups
            .iter()
            .filter(move |group| {
                echo_agent_hooks::matcher::matcher_allows(group.matcher.as_ref(), tool_name)
            })
            .flat_map(move |group| {
                let timeout = group.timeout.unwrap_or(default_timeout);
                group
                    .callback_ids
                    .iter()
                    .map(move |callback_id| (callback_id.as_str(), timeout))
            })
            .filter(move |(callback_id, _)| seen.insert(*callback_id))
            .map(move |(callback_id, timeout)| {
                let dispatch = ClientHookDispatch {
                    hook_callback_id: callback_id,
                    envelope,
                };
                async move {
                    let started = tokio::time::Instant::now();
                    let (response, gate_outcome) =
                        classify(self.send_hook_run(&dispatch, timeout).await);
                    let elapsed = started.elapsed();
                    echo_agent_telemetry::session_ctx::log_event(
                        echo_agent_telemetry::events::ClientHookGate {
                            callback_id: callback_id.to_string(),
                            tool_name: tool_name.map(str::to_string),
                            outcome: gate_outcome,
                            duration_ms: elapsed.as_millis() as u64,
                        },
                    );
                    (callback_id, response, elapsed, gate_outcome)
                }
            })
            .collect()
    }

    /// Run the client-registered `PreToolUse` hooks for `call`, firing
    /// `echo.agent/hooks/run` once per matching callback with the shared `envelope` (the
    /// same payload file hooks and observe events receive).
    ///
    /// Returns `Some(ToolLoop::HookDenied)` on the first deny, else `None`.
    pub(super) async fn run_pre_tool_use_client_hook(
        &self,
        call: &ToolCallResponse,
        tool_call_id: &acp::ToolCallId,
        envelope: &HookEventEnvelope,
    ) -> Result<Option<ToolLoop>, acp::Error> {
        // Clone the matched groups so we don't hold the `client_hooks` borrow across the
        // dispatch awaits below.
        let Some(groups) = self
            .client_hooks
            .borrow()
            .get(&HookEventName::PreToolUse)
            .cloned()
        else {
            return Ok(None);
        };
        // Match on the resolved target (in the envelope) so a client deny matcher
        // keyed on the real MCP tool gates a meta-dispatch call, matching the
        // observe path (`notify_client_hooks`). Equals `function.name` otherwise.
        let tool_name = envelope
            .payload
            .match_value()
            .unwrap_or(call.function.name.as_str());

        let mut pending = self.client_gate_responses(&groups, Some(tool_name), envelope);
        while let Some((callback_id, response, _elapsed, _outcome)) = pending.next().await {
            if response.decision == ClientHookDecision::Deny {
                let reason = response
                    .system_message
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "blocked by client hook".to_string());
                return Ok(Some(
                    self.deny_tool(
                        &call.id,
                        tool_call_id,
                        tool_name.to_owned(),
                        // Name the specific callback so telemetry / the UI annotation can
                        // attribute the block, not collapse every client hook to "client".
                        format!("client:{callback_id}"),
                        reason,
                    )
                    .await?,
                ));
            }
        }
        Ok(None)
    }

    /// Run the client `Stop`/`SubagentStop` gate for a turn-end envelope.
    /// Unlike the `PreToolUse` gate (first deny wins), every callback's response
    /// is aggregated into a [`StopDispatchResult`] (a `deny` maps to a block).
    pub(super) async fn run_stop_client_hooks(
        &self,
        envelope: &HookEventEnvelope,
    ) -> echo_agent_hooks::dispatcher::StopDispatchResult {
        use echo_agent_hooks::result::HookRunResult;

        let mut out = echo_agent_hooks::dispatcher::StopDispatchResult::default();
        // Clone: don't hold the borrow across awaits (see run_pre_tool_use_client_hook).
        let Some(groups) = self
            .client_hooks
            .borrow()
            .get(&envelope.hook_event_name.canonical())
            .cloned()
        else {
            return out;
        };

        let match_value = envelope.payload.match_value();
        // Aggregate in registration order so the attributed force-stop winner is
        // deterministic (completion order is not).
        let mut pending = self.client_gate_responses(&groups, match_value, envelope);
        let mut responses = std::collections::HashMap::new();
        while let Some((callback_id, response, elapsed, gate_outcome)) = pending.next().await {
            responses.insert(callback_id, (response, elapsed, gate_outcome));
        }
        let ordered = groups
            .iter()
            .flat_map(|group| group.callback_ids.iter())
            .filter_map(|id| responses.remove(id.as_str()).map(|r| (id.as_str(), r)));
        for (callback_id, (response, elapsed, gate_outcome)) in ordered {
            let hook_name = format!("client:{callback_id}");
            let block_reason = (response.decision == ClientHookDecision::Deny).then(|| {
                response
                    .system_message
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "blocked by client hook".to_string())
            });
            let stop_reason = (response.continue_ == Some(false)).then(|| {
                response
                    .stop_reason
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "stopped by client hook".to_string())
            });

            let detail = echo_agent_hooks::dispatcher::stop_detail(
                stop_reason.is_some(),
                stop_reason.as_deref(),
                block_reason.as_deref(),
            );
            // A hook that never answered did not observe the turn end, so the gate must not
            // count it as one that ran.
            let unanswered = match gate_outcome {
                ClientHookGateOutcome::TimedOut => Some("timed out"),
                ClientHookGateOutcome::TransportError => Some("transport error"),
                ClientHookGateOutcome::Denied
                | ClientHookGateOutcome::Proceeded
                | ClientHookGateOutcome::Malformed
                | ClientHookGateOutcome::UnknownDecision => None,
            };
            out.results.push(match (detail, unanswered) {
                (Some(detail), _) => HookRunResult::Blocked {
                    hook_name: hook_name.clone(),
                    detail,
                    elapsed,
                    http_info: None,
                },
                (None, Some(why)) => HookRunResult::Failed {
                    hook_name: hook_name.clone(),
                    error: format!("client hook {why}"),
                    elapsed,
                    http_info: None,
                },
                (None, None) => HookRunResult::Success {
                    hook_name: hook_name.clone(),
                    elapsed,
                    http_info: None,
                },
            });

            out.absorb(
                &hook_name,
                echo_agent_hooks::dispatcher::StopSignals {
                    block_reason,
                    stop_reason,
                    additional_context: response
                        .additional_context
                        .filter(|c| !c.trim().is_empty()),
                },
            );
        }
        out
    }

    /// Issue one `echo.agent/hooks/run` reverse request, bounded by a per-callback `timeout`.
    async fn send_hook_run(
        &self,
        dispatch: &ClientHookDispatch<'_>,
        timeout: Duration,
    ) -> ReverseOutcome {
        let Some(params) = dispatch_params(dispatch) else {
            return ReverseOutcome::Transport(acp::Error::internal_error());
        };
        let ext_request = acp::ExtRequest::new(HOOK_RUN_METHOD, params);
        match tokio::time::timeout(timeout, self.notifications.gateway.ext_method(ext_request))
            .await
        {
            Ok(Ok(raw)) => ReverseOutcome::Responded(raw.0),
            Ok(Err(err)) => ReverseOutcome::Transport(err),
            Err(_) => ReverseOutcome::Timeout,
        }
    }

    /// Fire observe-only client hooks for `envelope`'s event: send an
    /// `echo.agent/hooks/event` notification to each matching registered callback.
    /// Fire-and-forget (no decision is consumed); independent of file hooks, so it
    /// runs even when no on-disk hook registry exists. No-op when nothing is registered.
    pub(super) fn notify_client_hooks(&self, envelope: &HookEventEnvelope) {
        if !self.is_hook_execution_allowed() {
            return;
        }
        let hooks = self.client_hooks.borrow();
        let Some(groups) = hooks.get(&envelope.hook_event_name.canonical()) else {
            return;
        };
        let match_value = envelope.payload.match_value();
        for callback_id in matching_callback_ids(groups, match_value) {
            let dispatch = ClientHookDispatch {
                hook_callback_id: callback_id,
                envelope,
            };
            if let Some(params) = dispatch_params(&dispatch) {
                self.notifications
                    .gateway
                    .forward_fire_and_forget(acp::ExtNotification::new(HOOK_EVENT_METHOD, params));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(value: serde_json::Value) -> Arc<RawValue> {
        serde_json::value::to_raw_value(&value).unwrap().into()
    }

    /// Only an explicit `deny` blocks; malformed/transport/timeout all proceed. The second
    /// tuple element is the telemetry outcome, distinct per fail-open mode.
    #[test]
    fn classify_only_deny_blocks() {
        let (denied, outcome) = classify(ReverseOutcome::Responded(raw(
            serde_json::json!({ "decision": "deny" }),
        )));
        assert_eq!(denied.decision, ClientHookDecision::Deny);
        assert!(matches!(outcome, ClientHookGateOutcome::Denied));

        let (cont, outcome) = classify(ReverseOutcome::Responded(raw(
            serde_json::json!({ "decision": "continue" }),
        )));
        assert_eq!(cont.decision, ClientHookDecision::Continue);
        assert!(matches!(outcome, ClientHookGateOutcome::Proceeded));

        // Unknown decision fails open (proceeds) but reports a distinct outcome.
        let (unknown, outcome) = classify(ReverseOutcome::Responded(raw(
            serde_json::json!({ "decision": "maybe_later" }),
        )));
        assert_ne!(unknown.decision, ClientHookDecision::Deny);
        assert!(matches!(outcome, ClientHookGateOutcome::UnknownDecision));

        // Every failure mode falls open to Continue, but reports a distinct outcome.
        let (malformed, outcome) = classify(ReverseOutcome::Responded(raw(
            serde_json::json!({ "decision": 123 }),
        )));
        assert_eq!(malformed.decision, ClientHookDecision::Continue);
        assert!(matches!(outcome, ClientHookGateOutcome::Malformed));

        let (transport, outcome) =
            classify(ReverseOutcome::Transport(acp::Error::internal_error()));
        assert_eq!(transport.decision, ClientHookDecision::Continue);
        assert!(matches!(outcome, ClientHookGateOutcome::TransportError));

        let (timeout, outcome) = classify(ReverseOutcome::Timeout);
        assert_eq!(timeout.decision, ClientHookDecision::Continue);
        assert!(matches!(outcome, ClientHookGateOutcome::TimedOut));
    }

    /// `may_have_hooks_for` is the inert-when-unused guard: `false` with no file registry and
    /// no client hook for the event; `true` once a client hook for that event is registered;
    /// and `true` for any event whenever a file registry is present.
    #[tokio::test(flavor = "current_thread")]
    async fn may_have_hooks_for_inert_vs_active() {
        use echo_agent_hooks::event::HookEventName;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
                let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
                let actor = crate::session::acp_session::support::create_test_actor(
                    0,
                    256_000,
                    85,
                    gateway_tx,
                    persistence_tx,
                )
                .await;

                // Inert: no file registry, no client hooks.
                assert!(actor.hook_registry.borrow().is_none());
                assert!(actor.client_hooks.borrow().is_empty());
                assert!(!actor.may_have_hooks_for(HookEventName::PreToolUse));

                // A registered client hook activates exactly its event.
                actor.client_hooks.borrow_mut().insert(
                    HookEventName::PreToolUse,
                    vec![ClientHookGroup {
                        matcher: None,
                        callback_ids: vec!["cb_0".to_string()],
                        timeout: None,
                    }],
                );
                assert!(actor.may_have_hooks_for(HookEventName::PreToolUse));
                assert!(!actor.may_have_hooks_for(HookEventName::Stop));

                // A present file registry activates every event, even ones with no client hook.
                *actor.hook_registry.borrow_mut() = Some(std::sync::Arc::new(
                    echo_agent_hooks::discovery::HookRegistry::default(),
                ));
                assert!(actor.may_have_hooks_for(HookEventName::Stop));
                assert!(actor.may_have_hooks_for(HookEventName::PostCompact));
            })
            .await;
    }

    /// Tool events filter by matcher (matcher-less groups always fire); a non-tool
    /// event (`None`) fires every group regardless of its matcher.
    #[test]
    fn matching_callback_ids_filters_by_matcher() {
        use echo_agent_hooks::matcher::HookMatcher;

        let groups = vec![
            ClientHookGroup {
                matcher: Some(HookMatcher::new("run_terminal_command").unwrap()),
                callback_ids: vec!["bash_only".to_string()],
                timeout: None,
            },
            ClientHookGroup {
                matcher: None,
                callback_ids: vec!["all_a".to_string(), "all_b".to_string()],
                timeout: None,
            },
            ClientHookGroup {
                matcher: Some(HookMatcher::new("read_file").unwrap()),
                callback_ids: vec!["read_only".to_string()],
                timeout: None,
            },
        ];

        assert_eq!(
            matching_callback_ids(&groups, Some("run_terminal_command")),
            ["bash_only", "all_a", "all_b"]
        );
        assert_eq!(
            matching_callback_ids(&groups, Some("list_dir")),
            ["all_a", "all_b"]
        );
        assert_eq!(
            matching_callback_ids(&groups, None),
            ["bash_only", "all_a", "all_b", "read_only"]
        );
    }
}
