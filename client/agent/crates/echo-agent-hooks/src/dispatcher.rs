use crate::config::HookSpec;
use crate::discovery::HookRegistry;
use crate::event::{HookEventEnvelope, HookEventName};
use crate::result::{HookDecision, HookRunResult};
use crate::runner::{self, GateKind, HookRunnerResult, RunContext};

fn dispatch_span(event: HookEventName, hook_count: usize) -> tracing::Span {
    tracing::info_span!(
        "hooks.dispatch",
        hook_event = %event,
        hook_count = hook_count as i64,
        num_success = tracing::field::Empty,
        num_failed = tracing::field::Empty,
        num_blocking = tracing::field::Empty,
        num_skipped = tracing::field::Empty,
        total_duration_ms = tracing::field::Empty,
    )
}

/// Disabled/trust-disabled specs record a `Skipped` result; a matcher miss
/// records nothing.
fn eligible_or_record_skip(
    spec: &HookSpec,
    match_value: Option<&str>,
    results: &mut Vec<HookRunResult>,
) -> bool {
    if !spec.enabled || crate::trust::is_hook_disabled(&spec.name) {
        tracing::info!(hook_name = %spec.name, "hook skipped (disabled)");
        results.push(HookRunResult::Skipped {
            hook_name: spec.name.clone(),
        });
        return false;
    }
    crate::matcher::matcher_allows(spec.matcher.as_ref(), match_value)
}

/// A tool-input rewrite tagged with the hook that produced it.
pub struct InputRewrite {
    pub hook_name: String,
    pub input: serde_json::Value,
}

/// Result of a `pre_tool_use` dispatch: the final decision plus per-hook
/// execution details (for scrollback enrichment).
pub struct PreToolUseResult {
    pub decision: HookDecision,
    pub updated_input: Option<InputRewrite>,
    pub results: Vec<HookRunResult>,
}

/// Dispatch a `pre_tool_use` event against all matching hooks.
///
/// Runs hooks sequentially in config order. Only an explicit `deny`
/// decision from a hook stops the chain and blocks the tool call.
///
/// Hook failures (timeouts, crashes, command-not-found, env-var
/// pre-spawn refusals, malformed output) are **fail-open**: the failure
/// is logged and surfaced in the per-hook results for the UI scrollback,
/// but the tool call continues as if the hook had allowed it. EchoAgent
/// runs in protected environments where induced-failure bypass of
/// security hooks is not part of the threat model; the previous
/// fail-closed posture over-blocked innocent tool calls when
/// hooks timed out or had unrelated configuration errors.
///
/// Returns `Allow` if no hooks match, all hooks allow, or all failing
/// hooks are non-blocking by virtue of this fail-open policy.
pub async fn dispatch_pre_tool_use(
    registry: &HookRegistry,
    envelope: &HookEventEnvelope,
    ctx: &RunContext<'_>,
) -> PreToolUseResult {
    let hooks = registry.hooks_for(HookEventName::PreToolUse);
    if hooks.is_empty() {
        return PreToolUseResult {
            decision: HookDecision::Allow,
            updated_input: None,
            results: Vec::new(),
        };
    }

    let span = dispatch_span(HookEventName::PreToolUse, hooks.len());
    let _enter = span.enter();

    let match_value = envelope.payload.match_value().map(str::to_string);
    let mut run_results = Vec::new();
    let mut updated_input: Option<InputRewrite> = None;

    for spec in hooks {
        if !eligible_or_record_skip(spec, match_value.as_deref(), &mut run_results) {
            continue;
        }

        let _hook_span = tracing::info_span!(
            "hook.run",
            hook_name = %spec.name,
            hook_event = %HookEventName::PreToolUse,
        )
        .entered();

        let (result, elapsed, http_info) =
            runner::run_hook(spec, envelope, ctx, GateKind::Tool).await;

        match result {
            HookRunnerResult::Deny { reason, .. } => {
                tracing::info!(
                    hook_name = %spec.name,
                    elapsed_ms = elapsed.as_millis() as u64,
                    reason = %reason,
                    "hook denied"
                );
                run_results.push(HookRunResult::Blocked {
                    hook_name: spec.name.clone(),
                    detail: format!("denied: {reason}"),
                    elapsed,
                    http_info,
                });
                record_dispatch_counts(&span, &run_results);
                return PreToolUseResult {
                    decision: HookDecision::Deny {
                        reason,
                        hook_name: spec.name.clone(),
                    },
                    updated_input: None,
                    results: run_results,
                };
            }
            HookRunnerResult::Allow {
                updated_input: hook_updated_input,
            } => {
                tracing::info!(
                    hook_name = %spec.name,
                    elapsed_ms = elapsed.as_millis() as u64,
                    updated_input = hook_updated_input.is_some(),
                    "hook allowed"
                );
                if let Some(input) = hook_updated_input {
                    updated_input = Some(InputRewrite {
                        hook_name: spec.name.clone(),
                        input,
                    });
                }
                run_results.push(HookRunResult::Success {
                    hook_name: spec.name.clone(),
                    elapsed,
                    http_info,
                });
            }
            HookRunnerResult::Failed(err) => {
                // `hook_failure` (not `error`) on purpose: failure detail can
                // carry hook-authored stderr text, and `error` is on the OTEL
                // export allowlist. Remote traces keep their failure signal
                // via the text-free HookExecuted telemetry event.
                tracing::warn!(
                    hook_name = %spec.name,
                    elapsed_ms = elapsed.as_millis() as u64,
                    hook_failure = %err,
                    "hook failed; ignoring (fail-open)"
                );
                run_results.push(HookRunResult::Failed {
                    hook_name: spec.name.clone(),
                    error: err.clone(),
                    elapsed,
                    http_info,
                });
            }
            HookRunnerResult::Success | HookRunnerResult::Stop(_) => {
                tracing::info!(
                    hook_name = %spec.name,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "hook completed"
                );
                run_results.push(HookRunResult::Success {
                    hook_name: spec.name.clone(),
                    elapsed,
                    http_info,
                });
            }
        }
    }

    record_dispatch_counts(&span, &run_results);
    PreToolUseResult {
        decision: HookDecision::Allow,
        updated_input,
        results: run_results,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopBlock {
    pub hook_name: String,
    pub reason: String,
}

/// Aggregated signals from a `Stop`/`SubagentStop` gate dispatch.
#[derive(Debug, Default)]
pub struct StopDispatchResult {
    pub blocks: Vec<StopBlock>,
    pub additional_context: Vec<String>,
    /// First `continue: false` wins and overrides any blocks.
    pub prevent_continuation: Option<StopBlock>,
    pub results: Vec<HookRunResult>,
}

impl StopDispatchResult {
    pub fn wants_continuation(&self) -> bool {
        self.prevent_continuation.is_none()
            && (!self.blocks.is_empty() || !self.additional_context.is_empty())
    }

    /// The first force-stop wins (later ones are dropped); blocks and context
    /// accumulate in call order.
    pub fn absorb(&mut self, hook_name: &str, signals: StopSignals) {
        if let Some(reason) = signals.stop_reason
            && self.prevent_continuation.is_none()
        {
            self.prevent_continuation = Some(StopBlock {
                hook_name: hook_name.to_string(),
                reason,
            });
        }
        if let Some(reason) = signals.block_reason {
            self.blocks.push(StopBlock {
                hook_name: hook_name.to_string(),
                reason,
            });
        }
        if let Some(context) = signals.additional_context {
            self.additional_context.push(context);
        }
    }
}

/// One hook's stop signals, normalized for [`StopDispatchResult::absorb`].
/// A `Some` in `stop_reason` is what marks the hook as force-stopping.
#[derive(Debug, Default)]
pub struct StopSignals {
    pub block_reason: Option<String>,
    pub stop_reason: Option<String>,
    pub additional_context: Option<String>,
}

/// Scrollback detail for a stop signal, shared by the file and client gates so
/// the wording can't drift. A force-stop wins over a block; its reason may be absent.
pub fn stop_detail(
    prevented: bool,
    prevent_reason: Option<&str>,
    block_reason: Option<&str>,
) -> Option<String> {
    if prevented {
        return Some(match prevent_reason {
            Some(reason) => format!("prevented continuation: {reason}"),
            None => "prevented continuation".to_string(),
        });
    }
    block_reason.map(|reason| format!("blocked stop: {reason}"))
}

fn stop_outcome_detail(outcome: &crate::result::StopHookOutcome) -> Option<String> {
    stop_detail(
        outcome.force_stop.is_some(),
        outcome
            .force_stop
            .as_ref()
            .and_then(|f| f.reason.as_deref()),
        outcome.block_reason.as_deref(),
    )
}

/// Dispatch a `Stop` or `SubagentStop` gate against all matching hooks.
///
/// Every hook runs (no short-circuit) so the model sees all block reasons and
/// additional context at once. Hook failures (timeouts, crashes, malformed
/// output) are fail-open: recorded for the UI but contribute no signal, so the
/// agent stops normally.
pub async fn dispatch_stop(
    registry: &HookRegistry,
    event: HookEventName,
    envelope: &HookEventEnvelope,
    ctx: &RunContext<'_>,
) -> StopDispatchResult {
    if event.traits().gate != GateKind::Stop {
        debug_assert!(false, "dispatch_stop called with non-stop event {event:?}");
        tracing::error!(%event, "dispatch_stop called with a non-stop event; ignoring");
        return StopDispatchResult::default();
    }
    let event = event.canonical();
    let hooks = registry.hooks_for_canonical(event);
    if hooks.is_empty() {
        return StopDispatchResult::default();
    }

    let span = dispatch_span(event, hooks.len());
    let _enter = span.enter();

    let mut out = StopDispatchResult::default();
    let match_value = envelope.payload.match_value().map(str::to_string);

    for spec in hooks {
        if !eligible_or_record_skip(spec, match_value.as_deref(), &mut out.results) {
            continue;
        }

        let _hook_span = tracing::info_span!(
            "hook.run",
            hook_name = %spec.name,
            hook_event = %event,
        )
        .entered();

        let (result, elapsed, http_info) =
            runner::run_hook(spec, envelope, ctx, GateKind::Stop).await;

        match result {
            HookRunnerResult::Stop(outcome) => {
                tracing::info!(
                    hook_name = %spec.name,
                    elapsed_ms = elapsed.as_millis() as u64,
                    block = outcome.block_reason.is_some(),
                    additional_context = outcome.additional_context.is_some(),
                    prevent_continuation = outcome.force_stop.is_some(),
                    "stop hook completed"
                );
                match stop_outcome_detail(&outcome) {
                    Some(detail) => {
                        out.results.push(HookRunResult::Blocked {
                            hook_name: spec.name.clone(),
                            detail,
                            elapsed,
                            http_info,
                        });
                    }
                    None => out.results.push(HookRunResult::Success {
                        hook_name: spec.name.clone(),
                        elapsed,
                        http_info,
                    }),
                }
                out.absorb(
                    &spec.name,
                    StopSignals {
                        block_reason: outcome.block_reason,
                        stop_reason: outcome.force_stop.map(|force| {
                            force
                                .reason
                                .unwrap_or_else(|| "stopped by hook".to_string())
                        }),
                        additional_context: outcome.additional_context,
                    },
                );
            }
            HookRunnerResult::Failed(err) => {
                // `hook_failure`, not the exported `error` key (see the
                // pre_tool_use arm).
                tracing::warn!(
                    hook_name = %spec.name,
                    elapsed_ms = elapsed.as_millis() as u64,
                    hook_failure = %err,
                    "stop hook failed; ignoring (fail-open)"
                );
                out.results.push(HookRunResult::Failed {
                    hook_name: spec.name.clone(),
                    error: err,
                    elapsed,
                    http_info,
                });
            }
            HookRunnerResult::Success
            | HookRunnerResult::Allow { .. }
            | HookRunnerResult::Deny { .. } => {
                out.results.push(HookRunResult::Success {
                    hook_name: spec.name.clone(),
                    elapsed,
                    http_info,
                });
            }
        }
    }

    record_dispatch_counts(&span, &out.results);
    out
}

/// Dispatch an observe-only event against all matching hooks; never denies.
pub async fn dispatch_non_blocking(
    registry: &HookRegistry,
    event: HookEventName,
    envelope: &HookEventEnvelope,
    ctx: &RunContext<'_>,
) -> Vec<HookRunResult> {
    debug_assert!(
        event.traits().gate == GateKind::Observe,
        "dispatch_non_blocking called with gate event {event:?}"
    );
    let hooks = registry.hooks_for_canonical(event);
    if hooks.is_empty() {
        return Vec::new();
    }

    let span = dispatch_span(event, hooks.len());
    let _enter = span.enter();

    let match_value = envelope.payload.match_value().map(str::to_string);
    let mut results = Vec::with_capacity(hooks.len());

    for spec in hooks {
        if !eligible_or_record_skip(spec, match_value.as_deref(), &mut results) {
            continue;
        }

        let _hook_span = tracing::info_span!(
            "hook.run",
            hook_name = %spec.name,
            hook_event = %event,
        )
        .entered();

        let (result, elapsed, http_info) =
            runner::run_hook(spec, envelope, ctx, GateKind::Observe).await;

        match result {
            HookRunnerResult::Success => {
                tracing::info!(
                    hook_name = %spec.name,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "hook completed"
                );
                results.push(HookRunResult::Success {
                    hook_name: spec.name.clone(),
                    elapsed,
                    http_info,
                });
            }
            HookRunnerResult::Failed(err) => {
                // `hook_failure`, not the exported `error` key (see the
                // pre_tool_use arm).
                tracing::warn!(
                    hook_name = %spec.name,
                    elapsed_ms = elapsed.as_millis() as u64,
                    hook_failure = %err,
                    "hook failed"
                );
                results.push(HookRunResult::Failed {
                    hook_name: spec.name.clone(),
                    error: err,
                    elapsed,
                    http_info,
                });
            }
            HookRunnerResult::Allow { .. }
            | HookRunnerResult::Deny { .. }
            | HookRunnerResult::Stop(_) => {
                tracing::info!(
                    hook_name = %spec.name,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "hook completed"
                );
                results.push(HookRunResult::Success {
                    hook_name: spec.name.clone(),
                    elapsed,
                    http_info,
                });
            }
        }
    }

    record_dispatch_counts(&span, &results);

    results
}

fn record_dispatch_counts(span: &tracing::Span, results: &[HookRunResult]) {
    let mut num_success = 0i64;
    let mut num_failed = 0i64;
    let mut num_skipped = 0i64;
    let mut total_duration_ms = 0i64;
    let mut num_blocked = 0i64;
    for r in results {
        match r {
            HookRunResult::Success { elapsed, .. } => {
                num_success += 1;
                total_duration_ms += elapsed.as_millis() as i64;
            }
            HookRunResult::Blocked { elapsed, .. } => {
                num_blocked += 1;
                total_duration_ms += elapsed.as_millis() as i64;
            }
            HookRunResult::Failed { elapsed, .. } => {
                num_failed += 1;
                total_duration_ms += elapsed.as_millis() as i64;
            }
            HookRunResult::Skipped { .. } => num_skipped += 1,
        }
    }
    span.record("num_success", num_success);
    span.record("num_failed", num_failed);
    span.record("num_blocking", num_blocked);
    span.record("num_skipped", num_skipped);
    span.record("total_duration_ms", total_duration_ms);
}

/// `"hook.<snake_case_event_name>"` for hub-forwarded events, or `None` for
/// local-only events (`PreToolUse`).
pub fn hub_hook_kind(event: HookEventName) -> Option<String> {
    event.traits().hub_forward.then(|| format!("hook.{event}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HookSpec;
    use crate::event::{HookEventEnvelope, HookEventName, HookPayload};
    use crate::matcher::HookMatcher;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn pre_tool_use_envelope(tool_name: &str) -> HookEventEnvelope {
        HookEventEnvelope {
            hook_event_name: HookEventName::PreToolUse,
            session_id: "test-session".into(),
            cwd: "/tmp".into(),
            workspace_root: "/tmp".into(),
            timestamp: "2025-01-01T00:00:00Z".into(),
            transcript_path: None,
            client_identifier: None,
            prompt_id: None,
            permission_mode: None,
            payload: HookPayload::PreToolUse {
                tool_name: tool_name.into(),
                tool_use_id: "tu-1".into(),
                tool_input: serde_json::json!({"command": "ls"}),
                tool_input_truncated: false,
                subagent_type: None,
            },
        }
    }

    fn session_start_envelope() -> HookEventEnvelope {
        HookEventEnvelope {
            hook_event_name: HookEventName::SessionStart,
            session_id: "test-session".into(),
            cwd: "/tmp".into(),
            workspace_root: "/tmp".into(),
            timestamp: "2025-01-01T00:00:00Z".into(),
            transcript_path: None,
            client_identifier: None,
            prompt_id: None,
            permission_mode: None,
            payload: HookPayload::SessionStart {
                source: "new".into(),
                model_id: None,
                agent_type: None,
            },
        }
    }

    fn run_ctx() -> RunContext<'static> {
        RunContext {
            session_id: "test-session",
            workspace_root: "/tmp",
            process_scope: None,
        }
    }

    /// Helper: create a HookSpec pointing at `sh -c '<script>'` that prints
    /// the given JSON and exits with the given code.
    fn make_command_spec(
        name: &str,
        matcher: Option<&str>,
        enabled: bool,
        script: &str,
    ) -> HookSpec {
        HookSpec {
            name: name.into(),
            event: HookEventName::PreToolUse,
            handler_type: crate::config::HandlerType::Command,
            configured_matcher: matcher.map(|s| s.to_string()),
            matcher: matcher.map(|s| HookMatcher::new(s).unwrap()),
            enabled,
            command: Some(PathBuf::from(script)),
            command_raw: Some(script.to_string()),
            url: None,
            url_raw: None,
            timeout_ms: 5000,
            source_dir: PathBuf::from("/tmp"),
            extra_env: HashMap::new(),
            layer: crate::config::HookProvenance::File,
        }
    }

    fn registry_from_specs(specs: Vec<HookSpec>) -> HookRegistry {
        let (mut registry, _) = crate::discovery::load_hooks(None, None);
        registry.append_specs(specs);
        registry
    }

    #[test]
    fn match_value_extracts_per_payload_field() {
        assert_eq!(
            pre_tool_use_envelope("run_terminal_cmd")
                .payload
                .match_value(),
            Some("run_terminal_cmd")
        );
        assert_eq!(session_start_envelope().payload.match_value(), Some("new"));

        let notification = HookPayload::Notification {
            notification_type: "permission_prompt".into(),
            message: None,
            title: None,
            level: None,
        };
        assert_eq!(notification.match_value(), Some("permission_prompt"));
    }

    /// An empty subagent type (parent-side fire with no spawn record) yields
    /// `None` so matchers fire-all instead of silently matching nothing.
    #[test]
    fn subagent_match_value_is_none_when_type_empty() {
        let mut envelope = stop_envelope();
        envelope.hook_event_name = HookEventName::SubagentStop;
        let payload = |subagent_type: &str| HookPayload::SubagentStop {
            phase: crate::event::SubagentStopPhase::Observe,
            subagent_id: "sub-1".into(),
            subagent_type: subagent_type.into(),
            stop_hook_active: None,
            last_assistant_message: None,
        };
        envelope.payload = payload("explore");
        assert_eq!(envelope.payload.match_value(), Some("explore"));
        envelope.payload = payload("");
        assert_eq!(envelope.payload.match_value(), None);
    }

    #[tokio::test]
    async fn empty_registry_allows() {
        let registry = registry_from_specs(vec![]);
        let envelope = pre_tool_use_envelope("run_terminal_cmd");
        let result = dispatch_pre_tool_use(&registry, &envelope, &run_ctx()).await;
        assert_eq!(result.decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn pre_tool_use_carries_last_updated_input() {
        let first = make_command_spec(
            "first",
            Some("run_terminal_cmd"),
            true,
            "echo '{\"hookSpecificOutput\":{\"updatedInput\":{\"command\":\"one\"}}}'",
        );
        let second = make_command_spec(
            "second",
            Some("run_terminal_cmd"),
            true,
            "echo '{\"hookSpecificOutput\":{\"updatedInput\":{\"command\":\"two\"}}}'",
        );
        let registry = registry_from_specs(vec![first, second]);
        let result = dispatch_pre_tool_use(
            &registry,
            &pre_tool_use_envelope("run_terminal_cmd"),
            &run_ctx(),
        )
        .await;
        assert_eq!(result.decision, HookDecision::Allow);
        let rewrite = result.updated_input.expect("updatedInput carried");
        assert_eq!(rewrite.input["command"], "two");
        assert_eq!(rewrite.hook_name, "second");
    }

    #[tokio::test]
    async fn single_deny_hook() {
        let spec = make_command_spec(
            "deny-hook",
            None,
            true,
            "echo '{\"decision\":\"deny\",\"reason\":\"blocked\"}'; exit 2",
        );
        let registry = registry_from_specs(vec![spec]);
        let envelope = pre_tool_use_envelope("run_terminal_cmd");
        let result = dispatch_pre_tool_use(&registry, &envelope, &run_ctx()).await;
        match result.decision {
            HookDecision::Deny {
                ref reason,
                ref hook_name,
            } => {
                assert_eq!(reason, "blocked");
                assert_eq!(hook_name, "deny-hook");
            }
            ref other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disabled_hook_is_skipped_allows() {
        let spec = make_command_spec(
            "disabled-deny",
            None,
            false, // disabled!
            "echo '{\"decision\":\"deny\",\"reason\":\"should not run\"}'; exit 2",
        );
        let registry = registry_from_specs(vec![spec]);
        let envelope = pre_tool_use_envelope("run_terminal_cmd");
        let result = dispatch_pre_tool_use(&registry, &envelope, &run_ctx()).await;
        assert_eq!(result.decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn matcher_filters_by_tool() {
        let spec = make_command_spec(
            "bash-deny",
            Some("run_terminal_cmd"),
            true,
            "echo '{\"decision\":\"deny\",\"reason\":\"bash blocked\"}'; exit 2",
        );
        let registry = registry_from_specs(vec![spec]);

        let fired = dispatch_pre_tool_use(
            &registry,
            &pre_tool_use_envelope("run_terminal_cmd"),
            &run_ctx(),
        )
        .await;
        match fired.decision {
            HookDecision::Deny { ref reason, .. } => assert_eq!(reason, "bash blocked"),
            ref other => panic!("expected Deny, got {other:?}"),
        }

        let skipped =
            dispatch_pre_tool_use(&registry, &pre_tool_use_envelope("read_file"), &run_ctx()).await;
        assert_eq!(skipped.decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn first_deny_wins_short_circuits() {
        let deny_spec = make_command_spec(
            "first-deny",
            None,
            true,
            "echo '{\"decision\":\"deny\",\"reason\":\"first says no\"}'; exit 2",
        );
        let allow_spec = make_command_spec(
            "second-allow",
            None,
            true,
            "echo '{\"decision\":\"allow\"}'",
        );
        let registry = registry_from_specs(vec![deny_spec, allow_spec]);
        let envelope = pre_tool_use_envelope("run_terminal_cmd");
        let result = dispatch_pre_tool_use(&registry, &envelope, &run_ctx()).await;
        match result.decision {
            HookDecision::Deny {
                ref reason,
                ref hook_name,
                ..
            } => {
                assert_eq!(reason, "first says no");
                assert_eq!(hook_name, "first-deny");
            }
            ref other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn allow_then_deny_denies() {
        let allow_spec =
            make_command_spec("broad-allow", None, true, "echo '{\"decision\":\"allow\"}'");
        let deny_spec = make_command_spec(
            "strict-deny",
            None,
            true,
            "echo '{\"decision\":\"deny\",\"reason\":\"strict policy\"}'; exit 2",
        );
        let registry = registry_from_specs(vec![allow_spec, deny_spec]);
        let envelope = pre_tool_use_envelope("run_terminal_cmd");
        let result = dispatch_pre_tool_use(&registry, &envelope, &run_ctx()).await;
        match result.decision {
            HookDecision::Deny {
                ref reason,
                ref hook_name,
                ..
            } => {
                assert_eq!(reason, "strict policy");
                assert_eq!(hook_name, "strict-deny");
            }
            ref other => panic!("expected Deny from strict filter, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fail_open_on_hook_crash() {
        let spec = make_command_spec("crasher", None, true, "exit 1");
        let registry = registry_from_specs(vec![spec]);
        let envelope = pre_tool_use_envelope("run_terminal_cmd");
        let result = dispatch_pre_tool_use(&registry, &envelope, &run_ctx()).await;
        assert_eq!(
            result.decision,
            HookDecision::Allow,
            "fail-open: a crashing hook must not block the tool call"
        );
        assert_eq!(result.results.len(), 1);
        assert!(
            matches!(&result.results[0], HookRunResult::Failed { hook_name, .. } if hook_name == "crasher"),
            "the failure must still appear in run_results for UI scrollback, got {:?}",
            result.results
        );
    }

    #[tokio::test]
    async fn fail_open_then_deny_lets_deny_win() {
        let crash_spec = make_command_spec("crasher", None, true, "exit 1");
        let deny_spec = make_command_spec(
            "denier",
            None,
            true,
            "echo '{\"decision\":\"deny\",\"reason\":\"nope\"}'; exit 2",
        );
        let registry = registry_from_specs(vec![crash_spec, deny_spec]);
        let envelope = pre_tool_use_envelope("run_terminal_cmd");
        let result = dispatch_pre_tool_use(&registry, &envelope, &run_ctx()).await;
        match result.decision {
            HookDecision::Deny {
                ref hook_name,
                ref reason,
            } => {
                assert_eq!(hook_name, "denier");
                assert_eq!(reason, "nope");
            }
            ref other => panic!("expected Deny from explicit denier, got {other:?}"),
        }
        assert_eq!(result.results.len(), 2);
        assert!(
            matches!(&result.results[1], HookRunResult::Blocked { detail, .. }
                if detail == "denied: nope"),
            "a deny is the hook's decision, not a failure: {:?}",
            result.results[1]
        );
    }

    fn stop_envelope() -> HookEventEnvelope {
        HookEventEnvelope {
            hook_event_name: HookEventName::Stop,
            session_id: "test-session".into(),
            cwd: "/tmp".into(),
            workspace_root: "/tmp".into(),
            timestamp: "2025-01-01T00:00:00Z".into(),
            transcript_path: None,
            client_identifier: None,
            prompt_id: None,
            permission_mode: None,
            payload: HookPayload::Stop {
                reason: "end_turn".into(),
                stop_hook_active: false,
                last_assistant_message: Some("done".into()),
                background_tasks: None,
                session_crons: None,
            },
        }
    }

    fn stop_spec(name: &str, script: &str) -> HookSpec {
        let mut spec = make_command_spec(name, None, true, script);
        spec.event = HookEventName::Stop;
        spec
    }

    #[test]
    fn absorb_folds_signals_with_first_force_stop_winning() {
        let mut out = StopDispatchResult::default();
        out.absorb(
            "b1",
            StopSignals {
                block_reason: Some("first block".into()),
                ..Default::default()
            },
        );
        out.absorb(
            "s1",
            StopSignals {
                stop_reason: Some("stop now".into()),
                additional_context: Some("ctx".into()),
                ..Default::default()
            },
        );
        out.absorb(
            "s2",
            StopSignals {
                stop_reason: Some("too late".into()),
                block_reason: Some("second block".into()),
                ..Default::default()
            },
        );

        assert!(!out.wants_continuation(), "a force-stop overrides blocks");
        assert_eq!(
            out.blocks
                .iter()
                .map(|b| b.reason.as_str())
                .collect::<Vec<_>>(),
            ["first block", "second block"]
        );
        assert_eq!(out.additional_context, ["ctx"]);
        let prevent = out
            .prevent_continuation
            .as_ref()
            .expect("force-stop captured");
        assert_eq!(prevent.hook_name, "s1");
        assert_eq!(prevent.reason, "stop now");
    }

    #[test]
    fn absorb_empty_wants_no_continuation() {
        let out = StopDispatchResult::default();
        assert!(!out.wants_continuation());
        assert!(out.prevent_continuation.is_none());
    }

    #[tokio::test]
    async fn stop_collects_all_blocks() {
        let registry = registry_from_specs(vec![
            stop_spec("b1", "echo '{\"decision\":\"block\",\"reason\":\"first\"}'"),
            stop_spec("allow", "echo ok"),
            stop_spec(
                "b2",
                "echo '{\"decision\":\"block\",\"reason\":\"second\"}'",
            ),
        ]);
        let result =
            dispatch_stop(&registry, HookEventName::Stop, &stop_envelope(), &run_ctx()).await;
        assert!(result.wants_continuation());
        assert_eq!(
            result
                .blocks
                .iter()
                .map(|b| b.reason.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(result.results.len(), 3, "all hooks must have run");
    }

    #[tokio::test]
    async fn stop_prevent_continuation_overrides_blocks() {
        let registry = registry_from_specs(vec![
            stop_spec(
                "blocker",
                "echo '{\"decision\":\"block\",\"reason\":\"keep going\"}'",
            ),
            stop_spec(
                "stopper",
                "echo '{\"continue\":false,\"stopReason\":\"enough\"}'",
            ),
        ]);
        let result =
            dispatch_stop(&registry, HookEventName::Stop, &stop_envelope(), &run_ctx()).await;
        assert!(!result.wants_continuation());
        let prevent = result
            .prevent_continuation
            .expect("continue:false captured");
        assert_eq!(prevent.hook_name, "stopper");
        assert_eq!(prevent.reason, "enough");
        assert_eq!(result.blocks.len(), 1);
    }

    #[tokio::test]
    async fn stop_exit2_fail_open_and_context() {
        let registry = registry_from_specs(vec![
            stop_spec("exit2", "echo 'fix the build' >&2; exit 2"),
            stop_spec("crasher", "exit 1"),
            stop_spec(
                "ctx",
                "echo '{\"hookSpecificOutput\":{\"additionalContext\":\"note\"}}'",
            ),
        ]);
        let result =
            dispatch_stop(&registry, HookEventName::Stop, &stop_envelope(), &run_ctx()).await;
        assert!(result.wants_continuation());
        assert_eq!(result.blocks.len(), 1);
        assert_eq!(result.blocks[0].reason, "fix the build");
        assert_eq!(result.additional_context, ["note"]);
    }

    #[tokio::test]
    async fn stop_additional_context_only_keeps_working() {
        let registry = registry_from_specs(vec![stop_spec(
            "ctx",
            "echo '{\"hookSpecificOutput\":{\"additionalContext\":\"run the tests\"}}'",
        )]);
        let result =
            dispatch_stop(&registry, HookEventName::Stop, &stop_envelope(), &run_ctx()).await;
        assert!(
            result.wants_continuation(),
            "context alone must keep working"
        );
        assert!(result.blocks.is_empty());
        assert!(result.prevent_continuation.is_none());
        assert_eq!(result.additional_context, ["run the tests"]);
    }

    /// A timed-out stop hook fails open: stdout of a killed hook is never
    /// interpreted, so a block written before hanging is ignored.
    #[tokio::test]
    async fn stop_timeout_fails_open() {
        let mut spec = stop_spec(
            "slow",
            "echo '{\"decision\":\"block\",\"reason\":\"late\"}'; sleep 5",
        );
        spec.timeout_ms = 200;
        let registry = registry_from_specs(vec![spec]);
        let result =
            dispatch_stop(&registry, HookEventName::Stop, &stop_envelope(), &run_ctx()).await;
        assert!(
            !result.wants_continuation(),
            "timeout must not block the stop"
        );
        assert!(
            matches!(&result.results[0], HookRunResult::Failed { .. }),
            "the timeout is recorded as a failure, got {:?}",
            result.results[0]
        );
    }

    #[tokio::test]
    async fn stop_empty_and_allowing_registries_allow_stop() {
        let registry = registry_from_specs(vec![]);
        let result =
            dispatch_stop(&registry, HookEventName::Stop, &stop_envelope(), &run_ctx()).await;
        assert!(!result.wants_continuation());
        assert!(result.results.is_empty());

        let registry = registry_from_specs(vec![stop_spec("ok", "echo done")]);
        let result =
            dispatch_stop(&registry, HookEventName::Stop, &stop_envelope(), &run_ctx()).await;
        assert!(!result.wants_continuation());
    }

    #[tokio::test]
    async fn subagent_stop_consults_alias_specs() {
        let mut canonical = make_command_spec(
            "canonical",
            None,
            true,
            "echo '{\"decision\":\"block\",\"reason\":\"from canonical\"}'",
        );
        canonical.event = HookEventName::SubagentStop;
        let mut alias = make_command_spec(
            "alias",
            None,
            true,
            "echo '{\"decision\":\"block\",\"reason\":\"from alias\"}'",
        );
        alias.event = HookEventName::SubagentEnd;
        let registry = registry_from_specs(vec![canonical, alias]);

        let mut envelope = stop_envelope();
        envelope.hook_event_name = HookEventName::SubagentStop;
        envelope.payload = HookPayload::SubagentStop {
            phase: crate::event::SubagentStopPhase::Gate,
            subagent_id: "sub-1".into(),
            subagent_type: "explore".into(),
            stop_hook_active: Some(false),
            last_assistant_message: None,
        };
        let result = dispatch_stop(
            &registry,
            HookEventName::SubagentStop,
            &envelope,
            &run_ctx(),
        )
        .await;
        assert_eq!(result.blocks.len(), 2);
    }

    #[tokio::test]
    async fn subagent_stop_matcher_filters_by_agent_type() {
        let mut reviewer = make_command_spec(
            "reviewer",
            Some("code-reviewer"),
            true,
            "echo '{\"decision\":\"block\",\"reason\":\"from reviewer\"}'",
        );
        reviewer.event = HookEventName::SubagentStop;
        let mut explorer = make_command_spec(
            "explorer",
            Some("explore"),
            true,
            "echo '{\"decision\":\"block\",\"reason\":\"from explorer\"}'",
        );
        explorer.event = HookEventName::SubagentStop;
        let registry = registry_from_specs(vec![reviewer, explorer]);

        let mut envelope = stop_envelope();
        envelope.hook_event_name = HookEventName::SubagentStop;
        envelope.payload = HookPayload::SubagentStop {
            phase: crate::event::SubagentStopPhase::Gate,
            subagent_id: "sub-1".into(),
            subagent_type: "explore".into(),
            stop_hook_active: Some(false),
            last_assistant_message: None,
        };
        let result = dispatch_stop(
            &registry,
            HookEventName::SubagentStop,
            &envelope,
            &run_ctx(),
        )
        .await;
        assert_eq!(result.blocks.len(), 1, "only the matching spec runs");
        assert_eq!(result.blocks[0].reason, "from explorer");
    }

    #[tokio::test]
    async fn non_blocking_empty_registry() {
        let registry = registry_from_specs(vec![]);
        let envelope = session_start_envelope();
        let results = dispatch_non_blocking(
            &registry,
            HookEventName::SessionStart,
            &envelope,
            &run_ctx(),
        )
        .await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn non_blocking_disabled_hook_skipped() {
        let mut spec = make_command_spec("disabled", None, false, "echo ok");
        spec.event = HookEventName::SessionStart;
        let registry = registry_from_specs(vec![spec]);
        let envelope = session_start_envelope();
        let results = dispatch_non_blocking(
            &registry,
            HookEventName::SessionStart,
            &envelope,
            &run_ctx(),
        )
        .await;
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], HookRunResult::Skipped { .. }));
    }

    #[tokio::test]
    async fn non_blocking_failure_does_not_stop_chain() {
        let mut spec1 = make_command_spec("crasher", None, true, "exit 1");
        spec1.event = HookEventName::SessionStart;
        let mut spec2 = make_command_spec("ok", None, true, "echo ok");
        spec2.event = HookEventName::SessionStart;
        let registry = registry_from_specs(vec![spec1, spec2]);
        let envelope = session_start_envelope();
        let results = dispatch_non_blocking(
            &registry,
            HookEventName::SessionStart,
            &envelope,
            &run_ctx(),
        )
        .await;
        assert_eq!(results.len(), 2);
        assert!(matches!(results[0], HookRunResult::Failed { .. }));
        assert!(matches!(results[1], HookRunResult::Success { .. }));
    }

    #[test]
    fn hub_hook_kind_maps_all_hub_forwarded_events() {
        assert_eq!(hub_hook_kind(HookEventName::PreToolUse), None);

        let cases: &[(HookEventName, &str)] = &[
            (HookEventName::SessionStart, "hook.session_start"),
            (HookEventName::SessionEnd, "hook.session_end"),
            (HookEventName::Stop, "hook.stop"),
            (HookEventName::StopFailure, "hook.stop_failure"),
            (HookEventName::StopCancelled, "hook.stop_cancelled"),
            (HookEventName::PostToolUse, "hook.post_tool_use"),
            (
                HookEventName::PostToolUseFailure,
                "hook.post_tool_use_failure",
            ),
            (HookEventName::PermissionDenied, "hook.permission_denied"),
            (HookEventName::UserPromptSubmit, "hook.user_prompt_submit"),
            (HookEventName::Notification, "hook.notification"),
            (HookEventName::SubagentStart, "hook.subagent_start"),
            (HookEventName::SubagentStop, "hook.subagent_stop"),
            (HookEventName::SubagentEnd, "hook.subagent_stop"),
            (HookEventName::PreCompact, "hook.pre_compact"),
            (HookEventName::PostCompact, "hook.post_compact"),
        ];

        // Exhaustive match: adding a new HookEventName variant causes a
        // compiler error here, forcing this test to be updated.
        let total_variants = |e: HookEventName| -> usize {
            match e {
                HookEventName::SessionStart
                | HookEventName::SessionEnd
                | HookEventName::Stop
                | HookEventName::StopFailure
                | HookEventName::StopCancelled
                | HookEventName::PreToolUse
                | HookEventName::PostToolUse
                | HookEventName::PostToolUseFailure
                | HookEventName::PermissionDenied
                | HookEventName::UserPromptSubmit
                | HookEventName::Notification
                | HookEventName::SubagentStart
                | HookEventName::SubagentStop
                | HookEventName::SubagentEnd
                | HookEventName::PreCompact
                | HookEventName::PostCompact => 16,
            }
        };
        assert_eq!(
            cases.len() + 1, // +1 for PreToolUse (blocking, tested separately)
            total_variants(HookEventName::SessionStart),
            "update hub_hook_kind test when new HookEventName variants are added"
        );

        for (event, expected) in cases {
            let kind = hub_hook_kind(*event);
            assert_eq!(
                kind.as_deref(),
                Some(*expected),
                "hub_hook_kind wrong for {event:?}"
            );
        }
    }
}
