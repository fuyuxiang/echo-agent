//! Compaction methods for `SessionActor`.
//!
//! This module contains all compaction-related methods: manual `/compact`,
//! auto-compact threshold checks, inline auto-compact with auto-continue,
//! error-recovery compaction, preflight overflow detection, and checkpoint
//! persistence. These methods form a second `impl SessionActor` block that
//! lives alongside the primary one in `acp_session.rs`.
use super::SessionActor;
use super::is_project_instructions;
use crate::remote::DEFAULT_CONTEXT_WINDOW;
use crate::session::compaction_config::{
    AsyncCompactionCache, SUPPRESS_AUTH, SUPPRESS_NONE, SUPPRESS_STICKY, SUPPRESS_TURN,
    SUPPRESS_UNTIL_SUCCESS,
};
use crate::session::helpers::CompactionStateContext;
use crate::session::helpers::compaction_context::CompactionInputs;
use crate::session::helpers::compaction_context::to_system_reminder;
use crate::session::helpers::session_compact::{
    CompactOutput, CompactionOutcome, build_two_pass_compaction_prompt, generate_session_compact,
    is_context_length_error,
};
use crate::session::persistence::PersistenceMsg;
use crate::session::two_pass::{
    TWO_PASS_DEFAULT_SPLIT_FRACTION, build_two_pass_pass1_history, build_two_pass_pass2_history,
    note_for_two_pass_pass2, split_conversation_for_two_pass,
};
use agent_client_protocol as acp;
use echo_agent_chat_state::compaction_utils::{
    CompactedHistoryInput, CompactionAttempt, build_compacted_history, is_degenerate_summary,
    prepare_conversation_for_verbatim_summarization, sanitize_compacted_history,
    validate_compacted_history,
};
use echo_agent_sampling_types::{ApiBackend, ConversationItem};
use std::sync::Arc;
/// Default percentage points below the auto-compact threshold at which prefire
/// (background pass-1) starts, giving pass-1 runway to finish before the limit.
/// Override with `ECHO_AGENT_PREFIRE_LEAD_PERCENT`.
const DEFAULT_PREFIRE_LEAD_PERCENT: u64 = 10;
fn prefire_lead_percent() -> u64 {
    std::env::var("ECHO_AGENT_PREFIRE_LEAD_PERCENT")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_PREFIRE_LEAD_PERCENT)
}
fn compaction_mode_label(
    mode: echo_agent_chat_state::CompactionMode,
) -> echo_agent_telemetry::events::CompactionModeLabel {
    use echo_agent_chat_state::CompactionMode;
    use echo_agent_telemetry::events::CompactionModeLabel;
    match mode {
        CompactionMode::Summary => CompactionModeLabel::Summary,
        CompactionMode::Transcript => CompactionModeLabel::Transcript,
        CompactionMode::Segments(_) => CompactionModeLabel::Segments,
    }
}
/// Cheap fingerprint of a conversation prefix for prefire NOTE₁ validity. A
/// mismatch means the prefix changed (edit / rewind / branch) since pass-1, so
/// the cached NOTE₁ no longer summarizes the current prefix and must be dropped.
fn fingerprint_prefix(items: &[ConversationItem]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    items.len().hash(&mut h);
    for it in items {
        let tag: u8 = match it {
            ConversationItem::System(_) => 0,
            ConversationItem::User(_) => 1,
            ConversationItem::Assistant(_) => 2,
            ConversationItem::ToolResult(_) => 3,
            ConversationItem::BackendToolCall(_) => 4,
            ConversationItem::Reasoning(_) => 5,
        };
        tag.hash(&mut h);
        it.text_content().hash(&mut h);
    }
    h.finish()
}
/// Outcome of a background prefire pass-1 run, recorded on the
/// `session.prefire_pass1` span as `compaction_prefire_outcome`.
/// [`PrefireOutcome::as_str`] values are stable telemetry keys
/// (telemetry/dashboards key off them) — don't rename the strings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PrefireOutcome {
    Cached,
    Disabled,
    DebugFailPass1,
    TooSmall,
    EmptySplit,
    SampleFailed,
    EmptyNote1,
}
impl PrefireOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cached => "cached",
            Self::Disabled => "disabled",
            Self::DebugFailPass1 => "debug_fail_pass1",
            Self::TooSmall => "too_small",
            Self::EmptySplit => "empty_split",
            Self::SampleFailed => "sample_failed",
            Self::EmptyNote1 => "empty_note1",
        }
    }
}
/// Telemetry from one prefire pass-1 run; recorded onto the
/// `session.prefire_pass1` span by [`SessionActor::run_prefire_pass1`].
/// `None` fields = the run exited before that stage.
struct PrefirePass1Run {
    outcome: PrefireOutcome,
    prefix_len: Option<usize>,
    prefix_est_tokens: Option<u64>,
    pass1_latency_ms: Option<u64>,
    note1_chars: Option<usize>,
}
impl From<PrefireOutcome> for PrefirePass1Run {
    /// A run that exited before splitting/sampling — outcome only.
    fn from(outcome: PrefireOutcome) -> Self {
        Self {
            outcome,
            prefix_len: None,
            prefix_est_tokens: None,
            pass1_latency_ms: None,
            note1_chars: None,
        }
    }
}
#[cfg(test)]
#[path = "compaction_two_pass_prefire_helper_tests.rs"]
mod two_pass_prefire_helper_tests;
impl SessionActor {
    /// Two-pass active for this session: flag resolved on at build AND not an
    /// agent that keeps its single short self-summary.
    pub(crate) fn two_pass_active(&self) -> bool {
        let agent = self.agent.borrow();
        agent.compaction_policy().two_pass_enabled
    }
    /// Run one summarization sample over a fully-built two-pass history (the
    /// prompt is already embedded, so this bypasses the single-pass sampler and
    /// calls `generate_session_compact` directly). Returns `None` on any error
    /// so callers fall back to single-pass.
    ///
    /// Agent `RefCell` borrows are only taken for synchronous snapshots (never
    /// held across `.await`). Prefire is `spawn_local` on the same LocalSet as
    /// the turn loop; a long-lived borrow would race with turn/compact/cancel
    /// and panic on double-borrow.
    async fn two_pass_sample(&self, history: Vec<ConversationItem>) -> Option<CompactOutput> {
        let sampling_config = self.reconstruct_full_config().await;
        let client = match self.prepare_chat_completion(false).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "two_pass: failed to prepare sampling client");
                return None;
            }
        };
        let tool_defs = self.prepare_tool_definitions().await;
        let tools = self.turn_base_tool_specs(&tool_defs);
        let compaction_tool_tokens = echo_agent_chat_state::estimate_tool_specs_tokens(&tools);
        let wall_clock_budget_secs = self
            .agent
            .borrow()
            .compaction_policy()
            .wall_clock_budget_secs;
        let hosted_tools = self.hosted_tools_for_turn();
        let (cancel, _cancel_scope) = self.compaction.cancel.enter();
        match generate_session_compact(
            history,
            compaction_tool_tokens,
            tools,
            hosted_tools,
            client,
            self.session_info.id.clone(),
            &sampling_config,
            self.inference_idle_timeout,
            wall_clock_budget_secs,
            self.compaction.tool_choice,
            &cancel,
        )
        .await
        {
            Ok(out) => Some(out),
            Err(e) => {
                tracing::warn!(error = ?e, "two_pass: summarization sample failed");
                None
            }
        }
    }
    /// Per-turn prefire decision: usage has reached `threshold - lead` (so there
    /// is still runway before the hard auto-compact line at `threshold`).
    pub(crate) async fn should_prefire_two_pass(&self) -> bool {
        let sampling_cfg = self.chat_state_handle.get_sampling_config().await;
        let Some(cw) = sampling_cfg.as_ref().map(|c| c.context_window.get()) else {
            return false;
        };
        let estimated_total = self.chat_state_handle.get_estimated_total_tokens().await;
        let threshold = self.compaction.threshold_percent.get() as u64;
        let start_pct = threshold.saturating_sub(prefire_lead_percent());
        echo_agent_token_estimation::exceeds_threshold(estimated_total, cw, start_pct as u8)
    }
    /// Background pass-1: summarize the ~95% prefix → NOTE₁ and cache it for a
    /// later pass-2 apply. Always releases the in-flight guard. Spawned via
    /// `spawn_local` from the turn loop; reads a conversation snapshot and does
    /// not mutate session state. The span makes speculative pass-1 spend
    /// measurable (hit rate, wasted input tokens) ahead of the fleet-wide ramp.
    #[tracing::instrument(
        name = "session.prefire_pass1",
        skip_all,
        fields(
            session_id = %self.session_info.id.0,
            compaction_prefire_outcome = tracing::field::Empty,
            compaction_pass1_latency_ms = tracing::field::Empty,
            compaction_prefire_prefix_len = tracing::field::Empty,
            compaction_prefire_prefix_est_tokens = tracing::field::Empty,
            compaction_prefire_note1_chars = tracing::field::Empty,
        )
    )]
    pub(crate) async fn run_prefire_pass1(self: &Arc<Self>) {
        struct InFlightGuard<'a>(&'a crate::session::compaction_config::PrefireState);
        impl Drop for InFlightGuard<'_> {
            fn drop(&mut self) {
                self.0.finish();
            }
        }
        let _guard = InFlightGuard(&self.compaction.prefire);
        let run = self.run_prefire_pass1_inner().await;
        let span = tracing::Span::current();
        span.record("compaction_prefire_outcome", run.outcome.as_str());
        if let Some(v) = run.prefix_len {
            span.record("compaction_prefire_prefix_len", v as i64);
        }
        if let Some(v) = run.prefix_est_tokens {
            span.record("compaction_prefire_prefix_est_tokens", v as i64);
        }
        if let Some(v) = run.pass1_latency_ms {
            span.record("compaction_pass1_latency_ms", v as i64);
        }
        if let Some(v) = run.note1_chars {
            span.record("compaction_prefire_note1_chars", v as i64);
        }
    }
    async fn run_prefire_pass1_inner(self: &Arc<Self>) -> PrefirePass1Run {
        if !self.two_pass_active() {
            return PrefireOutcome::Disabled.into();
        }
        if std::env::var("ECHO_AGENT_DEBUG_TWO_PASS_FAIL_PASS1")
            .is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
        {
            tracing::info!(
                target: "two_pass",
                "two_pass: DEBUG ECHO_AGENT_DEBUG_TWO_PASS_FAIL_PASS1 — prefire pass1 produces no cache"
            );
            return PrefireOutcome::DebugFailPass1.into();
        }
        let conversation = self.chat_state_handle.get_conversation().await;
        if conversation.len() < 4 {
            return PrefireOutcome::TooSmall.into();
        }
        let split = split_conversation_for_two_pass(&conversation, TWO_PASS_DEFAULT_SPLIT_FRACTION);
        if split.prefix.is_empty() || split.tail.is_empty() {
            return PrefireOutcome::EmptySplit.into();
        }
        let sampling_cfg = self.chat_state_handle.get_sampling_config().await;
        let strips = sampling_cfg
            .as_ref()
            .map(|c| c.api_backend == ApiBackend::Messages)
            .unwrap_or(false);
        let model_slug = sampling_cfg
            .as_ref()
            .map(|c| c.model.to_string())
            .unwrap_or_default();
        let prefix_prepared =
            prepare_conversation_for_verbatim_summarization(split.prefix.to_vec(), strips);
        let prefix_est_tokens = prefix_prepared
            .iter()
            .map(echo_agent_chat_state::estimate_item_tokens)
            .sum::<u64>();
        let prompt = build_two_pass_compaction_prompt(None);
        let pass1_history = build_two_pass_pass1_history(&prefix_prepared, &prompt);
        let started = std::time::Instant::now();
        let out = self.two_pass_sample(pass1_history).await;
        let pass1_latency_ms = started.elapsed().as_millis() as u64;
        let attempted = |outcome: PrefireOutcome, note1_chars: Option<usize>| PrefirePass1Run {
            outcome,
            prefix_len: Some(split.split_idx),
            prefix_est_tokens: Some(prefix_est_tokens),
            pass1_latency_ms: Some(pass1_latency_ms),
            note1_chars,
        };
        let Some(out) = out else {
            return attempted(PrefireOutcome::SampleFailed, None);
        };
        let note1 = note_for_two_pass_pass2(&out.content);
        if note1.trim().is_empty() {
            return attempted(PrefireOutcome::EmptyNote1, None);
        }
        let note1_chars = note1.chars().count();
        let cache = AsyncCompactionCache {
            note1,
            prefix_len: split.split_idx,
            fingerprint: fingerprint_prefix(&conversation[..split.split_idx]),
            model_slug,
            pass1_latency_ms,
        };
        tracing::info!(
            target: "two_pass",
            prefix_len = cache.prefix_len,
            pass1_latency_ms = cache.pass1_latency_ms,
            "two_pass: prefire pass1 cached NOTE1"
        );
        self.compaction.prefire.store(cache);
        attempted(PrefireOutcome::Cached, Some(note1_chars))
    }
    /// Pass-2 apply: if a valid cached NOTE₁ exists for the current conversation,
    /// summarize (NOTE₁ + recent tail + special prompt) → final summary and
    /// return its `CompactOutput`. `None` → caller runs the single-pass path.
    ///
    /// **telemetry / `session.compact_inner` latency:** the returned `CompactOutput`
    /// stream timings are what land on `compaction_ttft_ms` /
    /// `compaction_stream_ms`. Those reflect **user-visible sync wait only**:
    /// - background pass-1 that already finished before compact is *not*
    ///   included (prefire hid that cost);
    /// - if pass-1 is still in flight we **do** add that await into
    ///   `ttft_ms` (time until first token of the final summary), because the
    ///   user is blocked on it;
    /// - `stream_ms` / `delta_count` / `itl_max_ms` are always pass-2 only
    ///   (the only sample that streams the successor-visible summary).
    async fn try_two_pass_pass2_apply(
        &self,
        user_context: Option<&str>,
        strips_reasoning: bool,
    ) -> Option<CompactOutput> {
        if !self.two_pass_active() {
            return None;
        }
        let mut prefire_waited_ms = 0u64;
        if let Some(handle) = self.compaction.prefire.take_handle() {
            let was_in_flight = self.compaction.prefire.is_in_flight();
            let waited = std::time::Instant::now();
            let _ = handle.await;
            if was_in_flight {
                prefire_waited_ms = waited.elapsed().as_millis() as u64;
                tracing::Span::current()
                    .record("compaction_prefire_waited_ms", prefire_waited_ms as i64);
                tracing::info!(
                    target: "two_pass",
                    wait_ms = prefire_waited_ms,
                    "two_pass: waited for in-flight prefire pass1 before pass2"
                );
            }
        }
        let cache = self.compaction.prefire.take()?;
        let live = self.chat_state_handle.get_conversation().await;
        let model_slug = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .map(|c| c.model.to_string())
            .unwrap_or_default();
        if cache.prefix_len == 0
            || cache.prefix_len > live.len()
            || cache.model_slug != model_slug
            || fingerprint_prefix(&live[..cache.prefix_len]) != cache.fingerprint
        {
            tracing::Span::current().record("compaction_prefire_stale", true);
            tracing::info!(
                target: "two_pass",
                "two_pass: cached NOTE1 stale or model changed; falling back to single-pass"
            );
            return None;
        }
        let prefix = &live[..cache.prefix_len];
        let tail = &live[cache.prefix_len..];
        let prepared_tail =
            prepare_conversation_for_verbatim_summarization(tail.to_vec(), strips_reasoning);
        let prompt = build_two_pass_compaction_prompt(user_context);
        let pass2_history =
            build_two_pass_pass2_history(prefix, &prepared_tail, &cache.note1, &prompt);
        let started = std::time::Instant::now();
        let mut out = self.two_pass_sample(pass2_history).await?;
        if is_degenerate_summary(&out.content) {
            tracing::Span::current().record("compaction_prefire_stale", true);
            tracing::info!(
                target: "two_pass",
                "two_pass: pass2 summary empty/degenerate; falling back to single-pass"
            );
            return None;
        }
        let pass2_latency_ms = started.elapsed().as_millis() as u64;
        if prefire_waited_ms > 0 {
            out.ttft_ms = Some(out.ttft_ms.unwrap_or(0).saturating_add(prefire_waited_ms));
        }
        let span = tracing::Span::current();
        span.record("compaction_two_pass_used", true);
        span.record("compaction_prefire_hit", true);
        span.record("compaction_pass2_latency_ms", pass2_latency_ms as i64);
        tracing::info!(
            target: "two_pass",
            prefix_len = cache.prefix_len,
            tail_len = tail.len(),
            prefire_waited_ms,
            pass2_latency_ms,
            pass1_bg_latency_ms = cache.pass1_latency_ms,
            "two_pass: pass2 applied cached NOTE1 (prefire hit)"
        );
        Some(out)
    }
}
/// Trigger info for auto-compact decisions.
pub(crate) struct AutoCompactTriggerInfo {
    pub tokens_used: u64,
    pub context_window: u64,
    pub percentage: u8,
    pub reason: &'static str,
}
/// The "always fits" lossy summarization budget (~70% of window, minus tool
/// definitions); shared by the ladder's Lossy step and the cold Lossy start.
fn lossy_input_budget(context_window: u64, tool_tokens: u64) -> u64 {
    (context_window.saturating_mul(7) / 10).saturating_sub(tool_tokens)
}
/// Why auto-compaction was suppressed after a deterministic failure.
/// [`SuppressReason::as_str`] is a stable telemetry value (BQ/OTLP/dashboards key
/// off it) — don't rename the strings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SuppressReason {
    CreditBlock,
    Size,
    Auth,
    Schema,
    Other,
}
impl SuppressReason {
    fn as_str(self) -> &'static str {
        match self {
            SuppressReason::CreditBlock => "credit_block",
            SuppressReason::Size => "size",
            SuppressReason::Auth => "auth",
            SuppressReason::Schema => "schema",
            SuppressReason::Other => "other",
        }
    }
    /// Suppression scope for this reason:
    /// - `size | schema` → [`SUPPRESS_STICKY`]: cleared only on a context-budget change.
    /// - `credit_block` → [`SUPPRESS_UNTIL_SUCCESS`]: wait for a model `200`.
    /// - `auth` → [`SUPPRESS_AUTH`]: clear on login/token refresh (not 200 — over-window deadlock).
    /// - `other` → [`SUPPRESS_TURN`]: optimistic per-turn retry.
    fn suppress_state(self) -> u8 {
        match self {
            SuppressReason::Size | SuppressReason::Schema => SUPPRESS_STICKY,
            SuppressReason::CreditBlock => SUPPRESS_UNTIL_SUCCESS,
            SuppressReason::Auth => SUPPRESS_AUTH,
            SuppressReason::Other => SUPPRESS_TURN,
        }
    }
}
/// Splice the preserved prefix (`conversation[0..prefix_len]`) onto the compacted
/// suffix, dropping the suffix's leading System and — if the prefix already has an
/// AGENTS.md item — its re-injected AGENTS.md too (else the model sees it twice).
/// Returns `Err(compacted_history)` unchanged when `prefix_len` is 0 or out of range.
fn preserve_inherited_prefix(
    conversation: &[ConversationItem],
    compacted_history: Vec<ConversationItem>,
    prefix_len: usize,
) -> Result<Vec<ConversationItem>, Vec<ConversationItem>> {
    if prefix_len == 0 || prefix_len > conversation.len() {
        return Err(compacted_history);
    }
    let inherited = &conversation[..prefix_len];
    let drop_reinjected_agents_md = inherited.iter().any(is_project_instructions);
    let mut preserved = inherited.to_vec();
    let child_items = compacted_history
        .into_iter()
        .skip_while(|i| matches!(i, ConversationItem::System(_)))
        .filter(|i| !(drop_reinjected_agents_md && is_project_instructions(i)));
    preserved.extend(child_items);
    Ok(preserved)
}
/// Project the token count a re-pinned (preserved) history would reseed to, so the
/// release decision compares against the same threshold the auto-compact trigger
/// applies next turn. This only APPROXIMATES the compaction reseed
/// (`echo-agent-chat-state` `replace_conversation`, the authority): it matches the reseed's
/// round-and-cap but divides by the current conversation estimate, not the reseed's
/// frozen `estimate_at_last_response`. The conversation only grows, so the current
/// estimate is >= that frozen value; this therefore under-estimates the reseed (a
/// lower bound) and can lean toward preserve. That never re-loops: the post-replace
/// `exceeds_threshold` check on the real reseeded total still sets sticky Size
/// suppression if a preserve leaves the fork over budget.
fn project_preserved_reseed_tokens(
    preserved_estimate: u64,
    tokens_before: u64,
    full_conv_estimate: u64,
) -> u64 {
    let ratio = tokens_before as f64 / full_conv_estimate.max(1) as f64;
    ((preserved_estimate as f64 * ratio).round() as u64).min(tokens_before)
}
impl SessionActor {
    /// Where the transcript would be, without asking the filesystem: callers on
    /// a hot path do the `exists()` themselves, off the actor's thread.
    pub(crate) fn transcript_path(&self) -> std::path::PathBuf {
        crate::session::persistence::session_dir(&self.session_info).join("updates.jsonl")
    }
    /// Path to the raw `updates.jsonl` transcript if it exists, else `None`.
    /// `pub(crate)` so the `Transcript`-mode dispatch in `compaction_segments`
    /// and transcript-location pointers can both reuse it.
    ///
    /// The `path.exists()` guard keeps the pointer safe when a session (e.g. a
    /// nested sub-agent) never wrote one -- the hint is simply omitted rather
    /// than dangling.
    pub(crate) fn get_transcript_path(&self) -> Option<String> {
        let path = self.transcript_path();
        if path.exists() {
            Some(path.to_string_lossy().into_owned())
        } else {
            None
        }
    }
    /// Increment the compaction counter and complete a pre-compaction memory flush.
    ///
    /// The counter is incremented before the flush check so the once-per-cycle
    /// guard does not suppress the first eligible flush.
    async fn maybe_pre_compaction_flush(
        self: &Arc<Self>,
        total_tokens: u64,
        context_window: u64,
        trigger: &'static str,
    ) {
        let compaction_count = self
            .compaction
            .count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if !self.agent.borrow().compaction_policy().memory_flush_enabled {
            return;
        }
        let last_flush = self
            .memory
            .last_flush_compaction
            .load(std::sync::atomic::Ordering::Relaxed);
        if echo_agent_memory::flush::should_flush(
            total_tokens,
            context_window,
            self.compaction.threshold_percent.get(),
            &self.memory.flush_config,
            last_flush,
            compaction_count,
        ) {
            let snapshot = self.snapshot_memory_flush_state().await;
            if self.run_memory_flush(trigger, Some(snapshot)).await {
                self.memory
                    .last_flush_compaction
                    .store(compaction_count, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    /// Tag the current `session.compact` span with `mode` (and `detail`, for
    /// `segments`) — the A/B variant key for grouping outcomes in telemetry.
    fn record_compaction_variant(&self) {
        let mode = self.compaction.compaction_mode;
        let span = tracing::Span::current();
        span.record("mode", tracing::field::display(mode));
        if let Some(detail) = mode.segment_detail() {
            span.record("detail", tracing::field::display(detail));
        }
    }
    /// Runs the compact operation over here which compresses the current conversation
    /// and helps with saving the context for the model
    #[tracing::instrument(
        name = "session.compact",
        skip_all,
        fields(
            session_id = %self.session_info.id.0,
            trigger = "manual",
            mode = tracing::field::Empty,
            detail = tracing::field::Empty,
            pre_tokens = tracing::field::Empty,
            post_tokens = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    pub(crate) async fn run_compact(
        self: &Arc<Self>,
        user_context: Option<String>,
    ) -> Result<(), acp::Error> {
        let (_cancel, _cancel_scope) = self.compaction.cancel.enter();
        self.record_compaction_variant();
        let total_tokens = self.chat_state_handle.get_total_tokens().await;
        tracing::Span::current().record("pre_tokens", total_tokens as i64);
        let sampling_config = self.chat_state_handle.get_sampling_config().await;
        let context_window = sampling_config
            .as_ref()
            .map(|c| c.context_window.get())
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        self.maybe_pre_compaction_flush(total_tokens, context_window, "pre_compaction")
            .await;
        if let Err(e) = self
            .run_compact_inner(
                user_context,
                None,
                echo_agent_telemetry::events::CompactionTrigger::Manual,
                false,
            )
            .await
        {
            let span = tracing::Span::current();
            span.record("success", false);
            span.record("error", e.to_string().as_str());
            return Err(e);
        }
        use crate::extensions::notification::SessionUpdate as EchoAgentSessionUpdate;
        let tokens_after = self.chat_state_handle.get_total_tokens().await;
        let span = tracing::Span::current();
        span.record("post_tokens", tokens_after as i64);
        span.record("success", true);
        self.send_echo_agent_notification(EchoAgentSessionUpdate::AutoCompactCompleted {
            tokens_before: Some(total_tokens),
            tokens_after,
            elapsed_ms: None,
            summary_preview: None,
        })
        .await;
        self.emit_status_snapshot_detached();
        Ok(())
    }
    async fn emit_compact_cancelled(&self, auto_trigger: bool) -> Result<(), acp::Error> {
        if auto_trigger {
            use crate::extensions::notification::SessionUpdate as EchoAgentSessionUpdate;
            self.send_echo_agent_notification(EchoAgentSessionUpdate::AutoCompactCancelled {
                reason: crate::extensions::notification::AutoCompactCancelReason::UserCancelled,
            })
            .await;
        }
        Err(crate::session::helpers::session_compact::CompactFailure::cancelled_error())
    }
    /// Suppress AUTO compaction after a deterministic failure. Scope depends on
    /// the reason (see [`SuppressReason::suppress_state`]): size/schema sticky,
    /// credit until 200, auth until credentials recover, other clears next turn.
    /// Telemetry + one notification per transition; manual `/compact` exempt.
    async fn suppress_auto_compaction(
        &self,
        reason: SuppressReason,
        estimated_tokens: u64,
        context_window: u64,
    ) {
        let new_state = reason.suppress_state();
        if self
            .compaction
            .auto_compact_suppressed
            .compare_exchange(
                SUPPRESS_NONE,
                new_state,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
        {
            tracing::warn!(
                suppress_reason = reason.as_str(),
                estimated_tokens,
                context_window,
                "auto-compaction suppressed after deterministic compaction failure"
            );
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::AutoCompactSuppressed {
                    reason: reason.as_str(),
                    estimated_tokens,
                    context_window,
                },
            );
            let message = match reason {
                SuppressReason::CreditBlock => {
                    "out of credits or over your spending limit. Add credits and retry."
                }
                SuppressReason::Auth => {
                    "authentication problem — re-authenticate using /login and retry."
                }
                SuppressReason::Size => "this conversation is too large to compact.",
                SuppressReason::Schema => "this conversation can't be summarized.",
                SuppressReason::Other => {
                    "it'll retry on the next turn, or start a new session using /new."
                }
            };
            self.send_echo_agent_notification(
                crate::extensions::notification::SessionUpdate::AutoCompactFailed {
                    error: message.to_string(),
                },
            )
            .await;
        }
    }
    /// Map a deterministic failure's error text to a fixed, content-free
    /// [`SuppressReason`] (drives telemetry + sticky-vs-per-turn scope).
    fn classify_suppress_reason(error_msg: &str) -> SuppressReason {
        let m = error_msg.to_ascii_lowercase();
        if m.contains("spending-limit")
            || m.contains("spending limit")
            || m.contains("out of credits")
            || m.contains("usage balance exhausted")
            || m.contains("usage limit reached")
        {
            SuppressReason::CreditBlock
        } else if is_context_length_error(&m) {
            SuppressReason::Size
        } else if m.contains("status 401") || m.contains("unauthorized") {
            SuppressReason::Auth
        } else if m.contains("invalid_request_error") {
            SuppressReason::Schema
        } else {
            SuppressReason::Other
        }
    }
    /// ACP error payload string (plain string or `{message, ...}`).
    fn acp_error_message(err: &acp::Error) -> String {
        match err.data.as_ref() {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(obj) => obj
                .get("message")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| obj.to_string()),
            None => err.message.clone(),
        }
    }
    /// Auth/401 compact failure — abort for reauth resubmit; don't sample oversized.
    pub(crate) fn is_auth_compact_error(err: &acp::Error) -> bool {
        matches!(
            Self::classify_suppress_reason(&Self::acp_error_message(err)),
            SuppressReason::Auth
        )
    }
    /// Terminal auth compact failure: emit RetryState auth (reauth stash) + auth_required.
    /// Separate from `AutoCompactFailed` (user-facing); this aborts the turn.
    pub(crate) async fn surface_compact_auth_failure(&self, err: acp::Error) -> acp::Error {
        use crate::extensions::notification::SessionUpdate as EchoAgentSessionUpdate;
        let detailed = Self::acp_error_message(&err);
        let message = if detailed.to_ascii_lowercase().contains("unauthorized") {
            detailed
        } else {
            format!(
                "Unauthorized (401): compaction failed — re-authenticate with /login \
                 and retry. ({detailed})"
            )
        };
        tracing::warn!(
            session_id = %self.session_info.id.0,
            error = %message,
            "auto-compact auth failure: aborting turn for re-auth"
        );
        echo_agent_telemetry::unified_log::warn(
            "auto-compact auth failure: aborting turn for re-auth",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "message": crate::util::truncate(&message, 300),
            })),
        );
        self.send_echo_agent_notification(EchoAgentSessionUpdate::RetryState(
            crate::extensions::notification::RetryState::Failed {
                error_type: "auth".to_string(),
                message: message.clone(),
            },
        ))
        .await;
        acp::Error::auth_required().data(crate::sampling::error::terminal_error_data(
            message,
            Some(401),
            echo_agent_sampler::SamplingErrorKind::Auth,
        ))
    }
    /// Clear [`SUPPRESS_AUTH`] on login/token refresh (credit suppress waits for a 200).
    pub(crate) fn clear_auth_compact_suppression(&self) {
        let _ = self.compaction.auto_compact_suppressed.compare_exchange(
            SUPPRESS_AUTH,
            SUPPRESS_NONE,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    /// Credit or auth suppress — a model switch cannot clear these.
    fn is_account_state_suppressed(&self) -> bool {
        matches!(
            self.compaction
                .auto_compact_suppressed
                .load(std::sync::atomic::Ordering::Relaxed),
            SUPPRESS_UNTIL_SUCCESS | SUPPRESS_AUTH
        )
    }
    /// Choose the post-compaction history for a forked session: re-pin the inherited
    /// prefix, or release it (fall back to the self-contained summary the summarizer
    /// already built from the whole conversation) when re-pinning would leave the fork
    /// at/over the auto-compact threshold. On release, sets the sticky flag and records
    /// the release span field (this runs within the `run_compact_inner` span).
    ///
    /// This runtime release compensates for a verbatim mirror-fork that pinned its whole
    /// parent transcript; bounding the inherited prefix at fork admission is the
    /// structural alternative that would remove this path.
    async fn resolve_forked_compacted_history(
        &self,
        compacted_history: Vec<ConversationItem>,
        prefix_len: usize,
        tokens_before: u64,
        context_window: u64,
    ) -> Vec<ConversationItem> {
        let full_conv = self.chat_state_handle.get_conversation().await;
        let compacted_len = compacted_history.len();
        let release_candidate = compacted_history.clone();
        match preserve_inherited_prefix(&full_conv, compacted_history, prefix_len) {
            Ok(preserved) => {
                let projected_preserved = project_preserved_reseed_tokens(
                    echo_agent_chat_state::estimate_conversation_tokens(&preserved),
                    tokens_before,
                    echo_agent_chat_state::estimate_conversation_tokens(&full_conv),
                );
                if echo_agent_token_estimation::exceeds_threshold(
                    projected_preserved,
                    context_window,
                    self.compaction.threshold_percent.get(),
                ) {
                    self.compaction
                        .prefix_released
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    tracing::Span::current().record("compaction_prefix_released", true);
                    tracing::info!(
                        session_id = %self.session_info.id.0,
                        prefix_len,
                        projected_preserved,
                        "compaction: releasing inherited prefix under pressure"
                    );
                    release_candidate
                } else {
                    tracing::info!(
                        session_id = %self.session_info.id.0,
                        prefix_len,
                        compacted_len,
                        "Preserving inherited prefix across compaction"
                    );
                    preserved
                }
            }
            Err(original) => {
                tracing::warn!(
                    session_id = %self.session_info.id.0,
                    prefix_len,
                    conversation_len = full_conv.len(),
                    "Inherited prefix invalid, using compacted history as-is"
                );
                original
            }
        }
    }
    /// Inner implementation of compaction that supports an optional `auto_continue`
    /// payload for the checkpoint.
    #[tracing::instrument(
        name = "session.compact_inner",
        skip_all,
        fields(
            session_id = %self.session_info.id.0,
            compaction_tokens_before = tracing::field::Empty,
            compaction_tokens_after = tracing::field::Empty,
            compaction_summary_chars = tracing::field::Empty,
            compaction_degenerate_rejections = tracing::field::Empty,
            compaction_input_overflow_rejections = tracing::field::Empty,
            compaction_deterministic_rejections = tracing::field::Empty,
            compaction_transient_rejections = tracing::field::Empty,
            compaction_attempts = tracing::field::Empty,
            compaction_trigger = tracing::field::Empty,
            compaction_trigger_pct = tracing::field::Empty,
            compaction_threshold_pct = tracing::field::Empty,
            compaction_outcome = tracing::field::Empty,
            compaction_stop_reason = tracing::field::Empty,
            compaction_ttft_ms = tracing::field::Empty,
            compaction_stream_ms = tracing::field::Empty,
            compaction_delta_count = tracing::field::Empty,
            compaction_itl_max_ms = tracing::field::Empty,
            compaction_two_pass_used = tracing::field::Empty,
            compaction_prefire_hit = tracing::field::Empty,
            compaction_pass2_latency_ms = tracing::field::Empty,
            compaction_prefire_waited_ms = tracing::field::Empty,
            compaction_prefire_stale = tracing::field::Empty,
            compaction_prefix_released = tracing::field::Empty,
        )
    )]
    async fn run_compact_inner(
        &self,
        user_context: Option<String>,
        auto_continue: Option<crate::extensions::notification::AutoContinueInfo>,
        trigger: echo_agent_telemetry::events::CompactionTrigger,
        lossy_input: bool,
    ) -> Result<(), acp::Error> {
        let (cancel, _cancel_scope) = self.compaction.cancel.enter();
        let tokens_before = self.chat_state_handle.get_total_tokens().await;
        tracing::Span::current().record("compaction_tokens_before", tokens_before as i64);
        let trigger_str = match trigger {
            echo_agent_telemetry::events::CompactionTrigger::Manual => "manual",
            echo_agent_telemetry::events::CompactionTrigger::Auto => "auto",
        };
        let sampling_config = self.chat_state_handle.get_sampling_config().await;
        let context_window = sampling_config
            .as_ref()
            .map(|c| c.context_window.get())
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        {
            let span = tracing::Span::current();
            let trigger_pct = if context_window == 0 {
                0
            } else {
                ((tokens_before as f64 / context_window as f64) * 100.0).round() as i64
            };
            span.record("compaction_trigger_pct", trigger_pct);
            span.record(
                "compaction_threshold_pct",
                self.compaction.threshold_percent.get() as i64,
            );
            span.record("compaction_trigger", trigger_str);
        }
        let summary_strips_reasoning = sampling_config
            .as_ref()
            .map(|c| c.api_backend == ApiBackend::Messages)
            .unwrap_or(false);
        let model_id = sampling_config.map(|c| c.model).unwrap_or_default();
        let compaction = echo_agent_telemetry::events::CompactionScope::begin(
            echo_agent_telemetry::events::CompactionBeginParams {
                trigger,
                tokens_used: tokens_before,
                context_window,
                model_id: model_id.clone(),
                user_context_provided: user_context.is_some(),
                compaction_mode: compaction_mode_label(self.compaction.compaction_mode),
                two_pass_enabled: self.two_pass_active(),
                is_subagent: self.startup_hints.is_subagent,
            },
        );
        let compact_source = trigger_str;
        self.dispatch_hook(
            echo_agent_hooks::event::HookEventName::PreCompact,
            echo_agent_hooks::event::HookPayload::PreCompact {
                source: compact_source.into(),
            },
            None,
            None,
        )
        .await;
        let max_retries = 3u32;
        let retry_delay_secs = 3u64;
        let (conv_len, system_message, full_conversation) = tokio::join!(
            self.chat_state_handle.get_conversation_len(),
            self.chat_state_handle.get_system_message(),
            self.chat_state_handle.get_conversation(),
        );
        let assembly_start = std::time::Instant::now();
        let segment_messages = if self.compaction.compaction_mode.writes_segments() {
            echo_agent_chat_state::compaction_utils::prepare_conversation_for_segment(
                full_conversation.clone(),
            )
        } else {
            Vec::new()
        };
        const SUMMARY_BUDGET_RESERVE_TOKENS: u64 = 32_768;
        let verbatim_input_enabled = self.compaction.verbatim_input && !lossy_input;
        let mut simplified_messages = if verbatim_input_enabled {
            echo_agent_chat_state::compaction_utils::prepare_conversation_for_verbatim_summarization(
                full_conversation,
                summary_strips_reasoning,
            )
        } else {
            echo_agent_chat_state::compaction_utils::prepare_conversation_for_summarization(
                full_conversation,
            )
        };
        let pre_compaction_ms = assembly_start.elapsed().as_millis() as u64;
        if conv_len == 0 {
            tracing::error!(
                session_id = %self.session_info.id.0,
                "Compaction failed: conversation is empty (ChatStateActor may have died)"
            );
            return Err(
                acp::Error::internal_error().data("Compaction failed: conversation is empty")
            );
        }
        let system_message = match system_message {
            Some(msg) => msg,
            None => {
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    conversation_len = conv_len,
                    "Compaction failed: no system message in conversation history"
                );
                return Err(acp::Error::internal_error()
                    .data("Compaction failed: no system message in conversation history"));
            }
        };
        if simplified_messages.is_empty() {
            tracing::error!(
                session_id = %self.session_info.id.0,
                conversation_len = conv_len,
                "Compaction failed: simplified conversation is empty"
            );
            return Err(acp::Error::internal_error()
                .data("Compaction failed: simplified conversation is empty"));
        }
        if !simplified_messages
            .iter()
            .any(|msg| matches!(msg, ConversationItem::System(_)))
        {
            tracing::error!(
                session_id = %self.session_info.id.0,
                conversation_len = conv_len,
                simplified_len = simplified_messages.len(),
                "Compaction failed: no system message in simplified conversation"
            );
            return Err(acp::Error::internal_error()
                .data("Compaction failed: no system message in simplified conversation"));
        }
        let sampling_config = self.reconstruct_full_config().await;
        let sampling_client = self.prepare_chat_completion(false).await?;
        let backend_search_active = self.backend_search_active();
        let effective_tool_defs: Vec<echo_agent_sampling_types::ToolDefinition> = self
            .prepare_tool_definitions()
            .await
            .into_iter()
            .filter(|td| !backend_search_active || td.function.name != "web_search")
            .collect();
        let compaction_tool_tokens =
            echo_agent_chat_state::estimate_tool_definitions_tokens(&effective_tool_defs);
        let compaction_tools: Vec<echo_agent_sampling_types::ToolSpec> = effective_tool_defs
            .into_iter()
            .map(echo_agent_sampling_types::ToolSpec::from)
            .collect();
        let compaction_hosted_tools: Vec<echo_agent_sampling_types::HostedTool> =
            self.hosted_tools_for_turn();
        if lossy_input {
            simplified_messages =
                echo_agent_chat_state::compaction_utils::fit_conversation_to_budget(
                    simplified_messages,
                    lossy_input_budget(context_window, compaction_tool_tokens),
                );
        }
        tracing::info!(
            num_tools = compaction_tools.len(),
            tool_tokens = compaction_tool_tokens,
            "Running compact with model '{}' (user model: '{}')",
            &sampling_config.model,
            &sampling_config.model
        );
        let mut last_error: Option<acp::Error> = None;
        let mut last_failure_outcome = CompactionOutcome::Failed;
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum InputStage {
            Verbatim,
            VerbatimFitted,
            Lossy,
        }
        impl InputStage {
            fn as_str(self) -> &'static str {
                match self {
                    Self::Verbatim => "verbatim",
                    Self::VerbatimFitted => "verbatim_fitted",
                    Self::Lossy => "lossy",
                }
            }
        }
        let mut input_stage = if verbatim_input_enabled {
            InputStage::Verbatim
        } else {
            InputStage::Lossy
        };
        let use_short_prompt = false;
        let started_at = chrono::Utc::now().to_rfc3339();
        let estimated_input_tokens =
            echo_agent_chat_state::estimate_conversation_tokens(&simplified_messages);
        let auto_trigger = matches!(
            trigger,
            echo_agent_telemetry::events::CompactionTrigger::Auto
        );
        let wall_clock_budget_secs = self
            .agent
            .borrow()
            .compaction_policy()
            .wall_clock_budget_secs;
        let sampler = crate::session::helpers::full_replace_compaction::ShellCompactionSampler::new(
            use_short_prompt,
            user_context.clone(),
            compaction_tools.clone(),
            compaction_hosted_tools.clone(),
            compaction_tool_tokens,
            sampling_client,
            self.session_info.id.clone(),
            sampling_config.clone(),
            self.inference_idle_timeout,
            wall_clock_budget_secs,
            self.compaction.tool_choice,
            cancel.clone(),
        );
        let observer =
            crate::session::helpers::full_replace_compaction::ShellFullReplaceObserver::new(
                trigger,
                context_window,
                compaction.compaction_id.clone(),
                self.session_info.id.0.to_string(),
                estimated_input_tokens,
                retry_delay_secs,
            );
        let fr_config = echo_agent_compaction::FullReplaceConfig {
            max_attempts: max_retries,
            retry_delay_secs,
            sampling_timeout_secs: 0,
        };
        let mut request_turns = simplified_messages.clone();
        let mut input_overflow_rejections: u32 = 0;
        let two_pass_output = self
            .try_two_pass_pass2_apply(user_context.as_deref(), summary_strips_reasoning)
            .await;
        let two_pass_used = two_pass_output.is_some();
        let mut compact_summary: Option<String> =
            two_pass_output.as_ref().map(|o| o.content.clone());
        while compact_summary.is_none() {
            match echo_agent_compaction::sample_full_replace_summary(
                &sampler,
                &request_turns,
                user_context.as_deref(),
                &fr_config,
                &observer,
            )
            .await
            {
                Ok(summary) => {
                    compact_summary = Some(summary.summary);
                    break;
                }
                Err(echo_agent_compaction::FullReplaceError::NothingToCompact) => {
                    last_error = Some(
                        acp::Error::internal_error().data("compact failed: nothing to compact"),
                    );
                    break;
                }
                Err(echo_agent_compaction::FullReplaceError::EmptyResponse) => {
                    last_failure_outcome = if observer.degenerate_seen() {
                        CompactionOutcome::Degenerate
                    } else {
                        CompactionOutcome::Transient
                    };
                    last_error = Some(acp::Error::internal_error().data(
                        observer.last_error_message().unwrap_or_else(|| {
                            "compact failed: model returned empty response".to_string()
                        }),
                    ));
                    break;
                }
                Err(echo_agent_compaction::FullReplaceError::Sampler {
                    message,
                    deterministic,
                    context_overflow,
                }) => {
                    if cancel.is_cancelled()
                        || message.contains(
                            crate::session::helpers::session_compact::COMPACT_CANCELLED_MSG,
                        )
                    {
                        return self.emit_compact_cancelled(auto_trigger).await;
                    }
                    if context_overflow {
                        let next_stage = match input_stage {
                            InputStage::Verbatim => Some(InputStage::VerbatimFitted),
                            InputStage::VerbatimFitted => Some(InputStage::Lossy),
                            InputStage::Lossy => None,
                        };
                        if let Some(stage) = next_stage {
                            input_overflow_rejections += 1;
                            echo_agent_telemetry::session_ctx::log_event(
                                echo_agent_telemetry::events::CompactionRetryDegraded {
                                    trigger,
                                    reason: "input_overflow",
                                    from_stage: Some(input_stage.as_str()),
                                    to_stage: Some(stage.as_str()),
                                    summary_chars: None,
                                    attempt: observer.attempt_count(),
                                    context_window,
                                    compaction_id: compaction.compaction_id.clone(),
                                },
                            );
                            tracing::warn!(
                                session_id = %self.session_info.id.0,
                                ?stage,
                                error = %message,
                                "Compaction input overflowed deterministically; stepping down the input ladder to avoid an incompactable state"
                            );
                            let conv = self.chat_state_handle.get_conversation().await;
                            request_turns = match stage {
                                InputStage::VerbatimFitted => {
                                    let budget = context_window
                                        .saturating_sub(SUMMARY_BUDGET_RESERVE_TOKENS)
                                        .saturating_sub(compaction_tool_tokens);
                                    let verbatim = echo_agent_chat_state::compaction_utils::prepare_conversation_for_verbatim_summarization(
                                        conv,
                                        summary_strips_reasoning,
                                    );
                                    echo_agent_chat_state::compaction_utils::fit_conversation_to_budget(
                                        verbatim,
                                        budget,
                                    )
                                }
                                InputStage::Lossy => {
                                    echo_agent_chat_state::compaction_utils::fit_conversation_to_budget(
                                        echo_agent_chat_state::compaction_utils::prepare_conversation_for_summarization(
                                            conv,
                                        ),
                                        lossy_input_budget(context_window, compaction_tool_tokens),
                                    )
                                }
                                InputStage::Verbatim => {
                                    unreachable!("ladder only steps forward")
                                }
                            };
                            input_stage = stage;
                            continue;
                        }
                        last_failure_outcome = CompactionOutcome::Deterministic;
                        if auto_trigger {
                            self.suppress_auto_compaction(
                                SuppressReason::Size,
                                estimated_input_tokens,
                                context_window,
                            )
                            .await;
                        }
                        last_error = Some(acp::Error::internal_error().data(message));
                        break;
                    }
                    if deterministic {
                        last_failure_outcome = CompactionOutcome::Deterministic;
                        if auto_trigger {
                            let reason = Self::classify_suppress_reason(&message);
                            self.suppress_auto_compaction(
                                reason,
                                estimated_input_tokens,
                                context_window,
                            )
                            .await;
                        }
                        last_error = Some(acp::Error::internal_error().data(message));
                        break;
                    }
                    last_failure_outcome = CompactionOutcome::Transient;
                    last_error = Some(acp::Error::internal_error().data(message));
                    break;
                }
            }
        }
        let telemetry = observer.into_telemetry();
        if two_pass_output.is_none()
            && let Some(request_chat_history) = sampler.take_last_attempted_items()
        {
            self.persist_compaction_request_artifact(
                request_chat_history,
                compaction_tools,
                user_context.as_deref(),
                use_short_prompt,
                &sampling_config.model,
                trigger,
                compact_summary
                    .as_deref()
                    .or(telemetry.last_rejected_summary.as_deref()),
                last_error.as_ref(),
                telemetry.attempts,
                telemetry.attempt_details,
                started_at,
            );
        }
        let compact_output = match compact_summary {
            Some(_) => match two_pass_output {
                Some(tp) => tp,
                None => sampler
                    .take_last_success()
                    .expect("a successful full-replace sample stashes its CompactOutput"),
            },
            None => {
                let span = tracing::Span::current();
                span.record("compaction_attempts", telemetry.attempts as i64);
                span.record(
                    "compaction_degenerate_rejections",
                    telemetry.degenerate_rejections as i64,
                );
                span.record(
                    "compaction_input_overflow_rejections",
                    input_overflow_rejections as i64,
                );
                span.record(
                    "compaction_deterministic_rejections",
                    telemetry.deterministic_rejections as i64,
                );
                span.record(
                    "compaction_transient_rejections",
                    telemetry.transient_rejections as i64,
                );
                span.record("compaction_outcome", last_failure_outcome.as_str());
                return Err(last_error.unwrap_or_else(|| {
                    acp::Error::internal_error().data("compaction failed: unknown error")
                }));
            }
        };
        let generate_session_compact = compact_output.content.clone();
        let user_message_prefix = self.build_user_message_prefix().await;
        let conversation = self.chat_state_handle.get_conversation().await;
        let (discovered_agents_md, all_skills_for_compaction, _agent_edited_paths, state_context) =
            if use_short_prompt {
                let empty_edited: std::collections::BTreeSet<String> = Default::default();
                let ctx =
                    CompactionStateContext::build(&conversation, CompactionInputs::default()).await;
                (Vec::<std::path::PathBuf>::new(), vec![], empty_edited, ctx)
            } else {
                let agents_md: Vec<std::path::PathBuf> = self
                    .agent
                    .borrow()
                    .tool_bridge()
                    .agents_md_reminded_paths()
                    .await
                    .into_iter()
                    .collect();
                let skills = self.slash_skills_for_resolve().await;
                let edited_paths = self.chat_state_handle.get_agent_edited_paths().await;
                let ctx = {
                    let bridge_tasks = self
                        .agent
                        .borrow()
                        .tool_bridge()
                        .list_background_tasks()
                        .await;
                    let pending_tasks: Vec<_> =
                        bridge_tasks.into_iter().filter(|t| !t.completed).collect();
                    let (execute_tool_name, monitor_tool_name) = if pending_tasks.is_empty() {
                        (None, None)
                    } else {
                        let agent_ref = self.agent.borrow();
                        let bridge = agent_ref.tool_bridge();
                        let empty = serde_json::json!({});
                        let execute = bridge
                            .render_prompt("${{ tools.by_kind.execute }}", &empty)
                            .await
                            .filter(|s| !s.is_empty() && !s.contains("by_kind"));
                        let monitor = bridge
                            .render_prompt("${{ tools.by_kind.monitor }}", &empty)
                            .await
                            .filter(|s| !s.is_empty() && !s.contains("by_kind"));
                        (execute, monitor)
                    };
                    let running_tasks: Vec<_> = pending_tasks
                        .into_iter()
                        .map(|t| {
                            let tool_name = match t.kind {
                                echo_agent_tools::computer::types::TaskKind::Monitor => {
                                    monitor_tool_name.clone()
                                }
                                echo_agent_tools::computer::types::TaskKind::Bash => {
                                    execute_tool_name.clone()
                                }
                            };
                            CompactionStateContext::task_summary(
                                t.task_id, t.command, "running", tool_name,
                            )
                        })
                        .collect();
                    let running_subagents = if let Some(ref event_tx) =
                        self.tool_context.subagent_event_tx
                    {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        use echo_agent_tools::implementations::echo_agent_build::task::types::{
                            SubagentEvent, SubagentListActiveRequest,
                        };
                        let _ =
                            event_tx.send(SubagentEvent::ListActive(SubagentListActiveRequest {
                                parent_session_id: self.session_id_string(),
                                respond_to: tx,
                            }));
                        rx.await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|s| crate::session::helpers::compaction_context::RunningSubagentSummary {
                            subagent_id: s.subagent_id,
                            subagent_type: s.subagent_type,
                            description: s.description,
                            elapsed_ms: s.elapsed_ms,
                        })
                        .collect()
                    } else {
                        vec![]
                    };
                    let connected_mcp_servers = {
                        use crate::session::helpers::compaction_context::CompactionServerSummary;
                        use echo_agent_tools::implementations::search_tool::{
                            sanitize_description, truncate_description,
                        };
                        self.connected_server_summaries()
                            .into_iter()
                            .map(|s| {
                                let desc = s
                                    .description
                                    .map(|d| truncate_description(&sanitize_description(&d)))
                                    .filter(|d| !d.is_empty());
                                CompactionServerSummary {
                                    name: s.name,
                                    tool_count: s.tool_count,
                                    description: desc,
                                }
                            })
                            .collect()
                    };
                    let todos = {
                        use crate::session::helpers::compaction_context::{
                            TodoSummary, TodoSummaryStatus,
                        };
                        use crate::tools::todo::{TodoState, TodoStatus};
                        use echo_agent_tools::types::resources::State;
                        let bridge = self.agent.borrow().tool_bridge().clone();
                        bridge
                            .read_resource::<State<TodoState>>()
                            .await
                            .map(|s| {
                                s.0.todo_items_with_ids()
                                    .map(|(id, item)| TodoSummary {
                                        id: id.clone(),
                                        content: item.content.clone(),
                                        status: match item.status {
                                            TodoStatus::Pending => TodoSummaryStatus::Pending,
                                            TodoStatus::InProgress => TodoSummaryStatus::InProgress,
                                            TodoStatus::Completed => TodoSummaryStatus::Completed,
                                            TodoStatus::Cancelled => TodoSummaryStatus::Cancelled,
                                        },
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                    CompactionStateContext::build(
                        &conversation,
                        CompactionInputs {
                            running_tasks,
                            running_subagents,
                            agent_edited_paths: edited_paths.clone(),
                            connected_mcp_servers,
                            todos,
                            ..Default::default()
                        },
                    )
                    .await
                };
                (agents_md, skills, edited_paths, ctx)
            };
        use crate::session::helpers::compaction_context::SubagentToolNames;
        let subagent_tool_names: Option<SubagentToolNames> =
            if use_short_prompt || state_context.running_subagents.is_empty() {
                None
            } else {
                let agent_ref = self.agent.borrow();
                let bridge = agent_ref.tool_bridge();
                let empty = serde_json::json!({});
                let poll_name = bridge
                    .render_prompt("${{ tools.by_kind.background_task_action }}", &empty)
                    .await
                    .filter(|s| !s.is_empty() && !s.contains("by_kind"));
                let cancel_name = bridge
                    .render_prompt("${{ tools.by_kind.kill_task_action }}", &empty)
                    .await
                    .filter(|s| !s.is_empty() && !s.contains("by_kind"));
                match (poll_name, cancel_name) {
                    (Some(poll), Some(cancel)) => Some(SubagentToolNames { poll, cancel }),
                    (poll, cancel) => {
                        tracing::warn!(
                            session_id = %self.session_info.id.0,
                            poll_resolved = poll.is_some(),
                            cancel_resolved = cancel.is_some(),
                            "could not resolve subagent tool names, \
                             omitting subagent reminder from compacted conversation"
                        );
                        None
                    }
                }
            };
        use crate::session::helpers::compaction_context::McpToolNames;
        let mcp_tool_names: Option<McpToolNames> =
            if use_short_prompt || state_context.connected_mcp_servers.is_empty() {
                None
            } else {
                let agent_ref = self.agent.borrow();
                let bridge = agent_ref.tool_bridge();
                let empty = serde_json::json!({});
                let search_name = bridge
                    .render_prompt("${{ tools.by_kind.search_tool }}", &empty)
                    .await
                    .filter(|s| !s.is_empty() && !s.contains("by_kind"));
                let call_name = bridge
                    .render_prompt("${{ tools.by_kind.use_tool }}", &empty)
                    .await
                    .filter(|s| !s.is_empty() && !s.contains("by_kind"));
                match (search_name, call_name) {
                    (Some(search), Some(call)) => Some(McpToolNames { search, call }),
                    _ => None,
                }
            };
        let memory_backend_impl = {
            let g = self.memory.storage.borrow();
            g.as_ref()
                .zip(self.memory.backend_params.as_ref())
                .map(|(storage, params)| {
                    crate::session::memory::MemoryBackendImpl::from_session_params(
                        storage.clone(),
                        &crate::session::memory::MemoryBackendParams {
                            search_source:
                                crate::session::memory::MemorySearchSource::CompactionRecovery,
                            ..params.clone()
                        },
                    )
                })
        };
        let memory_opt_out = false;
        let memory_ref: Option<&dyn echo_agent_tools::types::memory_backend::MemoryBackend> =
            if memory_opt_out {
                None
            } else {
                memory_backend_impl
                    .as_ref()
                    .map(|b| b as &dyn echo_agent_tools::types::memory_backend::MemoryBackend)
            };
        let suppress_state_reminder = false;
        let workflow_listing = self.workflow_listing_for_prompt();
        let system_reminder = if suppress_state_reminder {
            workflow_listing.as_deref().map(|listing| {
                let tag = self.reminder_wrapper_tag();
                format!("<{tag}>\n## Available Workflows\n{listing}\n</{tag}>")
            })
        } else {
            to_system_reminder(
                &state_context,
                &discovered_agents_md,
                &all_skills_for_compaction,
                memory_ref,
                subagent_tool_names.as_ref(),
                mcp_tool_names.as_ref(),
                workflow_listing.as_deref(),
            )
            .await
        };
        let system_reminder = {
            let plan_path = {
                let guard = self.plan_mode.lock();
                guard
                    .is_active()
                    .then(|| guard.plan_file_path().to_path_buf())
            };
            if let Some(plan_path) = plan_path {
                let plan_has_content =
                    crate::session::plan_mode::plan_file_has_content(&plan_path).await;
                let template = crate::session::plan_mode::plan_mode_reminder_full_template();
                let wrapper = self.reminder_wrapper_tag();
                let rendered = self
                    .render_plan_template(template, &plan_path, plan_has_content)
                    .await;
                match (system_reminder, rendered) {
                    (Some(mut existing), Some(plan_section)) => {
                        if let Some(pos) = existing.rfind("</system-reminder>") {
                            existing.insert_str(pos, &format!("\n\n{}\n", plan_section));
                        } else {
                            existing.push_str("\n\n");
                            existing.push_str(&plan_section);
                        }
                        Some(existing)
                    }
                    (None, Some(plan_section)) => Some(format!(
                        "<{tag}>\n{body}\n</{tag}>",
                        tag = wrapper,
                        body = plan_section,
                    )),
                    (existing, None) => {
                        tracing::warn!(
                            session_id = %self.session_info.id.0,
                            "compaction: plan mode active but template render failed"
                        );
                        existing
                    }
                }
            } else {
                system_reminder
            }
        };
        if let Some(ref recovery_backend) = memory_backend_impl {
            let n = recovery_backend
                .search_counter
                .load(std::sync::atomic::Ordering::Relaxed);
            if n > 0 {
                self.memory
                    .compaction_recovery_count
                    .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                tracing::debug!(
                    target: echo_agent_telemetry::memory_log::TARGET,
                    count = n,
                    "MEMORY_COMPACTION_RECOVERY: {} search(es) performed",
                    n,
                );
            }
        }
        let agents_md_reminder = self.agent.borrow().agents_md_user_reminder();
        let compaction_context = state_context.for_compaction();
        let compaction_state_context: &CompactionStateContext = &compaction_context;
        let transcript_hint = self.transcript_hint();
        let summary_count = self
            .compaction
            .count
            .load(std::sync::atomic::Ordering::Relaxed);
        let apply_start = std::time::Instant::now();
        let raw_compacted = build_compacted_history(CompactedHistoryInput {
            system_message: system_message.clone(),
            user_message_prefix: user_message_prefix.clone(),
            agents_md_reminder: agents_md_reminder.clone(),
            state_context: compaction_state_context,
            compaction_summary: generate_session_compact.clone(),
            system_reminder: system_reminder.clone(),
            summary_before_recent: use_short_prompt,
            transcript_hint: transcript_hint.clone(),
            summary_count,
        });
        let sanitize_result = sanitize_compacted_history(raw_compacted);
        let compacted_history = if sanitize_result.stripped_tool_call_ids.is_empty() {
            sanitize_result.items
        } else {
            tracing::warn!(
                session_id = %self.session_info.id,
                stripped_count = sanitize_result.stripped_tool_call_ids.len(),
                stripped_ids = ?sanitize_result.stripped_tool_call_ids,
                "compaction: stripped orphaned ToolResults from compacted history"
            );
            sanitize_result.items
        };
        let remaining_violations = validate_compacted_history(&compacted_history);
        let compacted_history = if remaining_violations.is_empty() {
            compacted_history
        } else {
            tracing::error!(
                session_id = %self.session_info.id,
                violation_count = remaining_violations.len(),
                violation_ids = ?remaining_violations,
                "compaction: sanitized history still has invalid ToolResults -- \
                 falling back to minimal compacted history (no recent_messages)"
            );
            build_compacted_history(CompactedHistoryInput {
                system_message,
                user_message_prefix,
                agents_md_reminder,
                state_context: &state_context.for_compaction(),
                compaction_summary: generate_session_compact.clone(),
                system_reminder,
                summary_before_recent: use_short_prompt,
                transcript_hint,
                summary_count,
            })
        };
        let post_compaction_ms = apply_start.elapsed().as_millis() as u64;
        let prompt_index_at_compaction = self.chat_state_handle.get_prompt_index().await;
        let original_user_info = self
            .chat_state_handle
            .get_conversation_item_at(1)
            .await
            .and_then(|item| match item {
                ConversationItem::User(parts) => {
                    parts.content.into_iter().next().and_then(|p| match p {
                        echo_agent_sampling_types::ContentPart::Text { text } => {
                            Some(text.as_ref().to_owned())
                        }
                        _ => None,
                    })
                }
                _ => None,
            });
        if cancel.is_cancelled() {
            return self.emit_compact_cancelled(auto_trigger).await;
        }
        let segments_written = u32::from(
            self.persist_compaction_segment(&segment_messages, &generate_session_compact),
        );
        self.chat_state_handle
            .record_compaction_at(prompt_index_at_compaction);
        self.persist_compaction_checkpoint(
            &compacted_history,
            prompt_index_at_compaction,
            auto_continue,
            original_user_info,
        );
        let prefix_len = if self
            .compaction
            .prefix_released
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            0
        } else {
            self.startup_hints.inherited_prefix_len.unwrap_or(0)
        };
        let compacted_history = if prefix_len == 0 {
            compacted_history
        } else {
            self.resolve_forked_compacted_history(
                compacted_history,
                prefix_len,
                tokens_before,
                context_window,
            )
            .await
        };
        let new_len = compacted_history.len();
        self.chat_state_handle
            .replace_conversation_for_compaction(compacted_history);
        // The actor round trip acknowledges the history replacement before
        // counting it. A later hook/caller cancellation cannot erase success.
        let applied_tokens = self.chat_state_handle.get_total_tokens().await;
        self.signals_handle().record_compaction(tokens_before);
        echo_agent_telemetry::unified_log::info(
            "shell.context.compaction_applied",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "trigger": compact_source,
                "tokens_before": tokens_before,
                "tokens_after": applied_tokens,
                "context_window": context_window,
            })),
        );
        if self.startup_hints.inherited_prefix_len.is_some() {
            let post_replace_tokens = self.chat_state_handle.get_total_tokens().await;
            if echo_agent_token_estimation::exceeds_threshold(
                post_replace_tokens,
                context_window,
                self.compaction.threshold_percent.get(),
            ) {
                self.compaction
                    .auto_compact_suppressed
                    .store(SUPPRESS_STICKY, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(
                    session_id = %self.session_info.id.0,
                    post_replace_tokens,
                    context_window,
                    "compaction: released history still over threshold; suppressing AUTO to avoid a re-loop"
                );
            } else {
                self.compaction
                    .auto_compact_suppressed
                    .store(SUPPRESS_NONE, std::sync::atomic::Ordering::Relaxed);
            }
        } else {
            self.compaction
                .auto_compact_suppressed
                .store(SUPPRESS_NONE, std::sync::atomic::Ordering::Relaxed);
        }
        self.last_idle_flush_conversation_len
            .store(new_len, std::sync::atomic::Ordering::Relaxed);
        self.memory
            .context_injected
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if self.memory.is_enabled() {
            tracing::info!(target: echo_agent_telemetry::memory_log::TARGET, "MEMORY_COMPACT: post-compaction reset, next turn re-checks injection (search only if no block persisted)");
        }
        let _ = self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::PlanState(
                crate::tools::todo::TodoState::default(),
            ));
        self.agent
            .borrow()
            .tool_bridge()
            .on_agents_md_compaction()
            .await;
        self.agent
            .borrow()
            .tool_bridge()
            .on_skill_discovery_compaction()
            .await;
        self.persist_announcement_state().await;
        self.plan_mode.lock().reset_after_compaction();
        self.persist_plan_mode_state();
        self.dispatch_hook(
            echo_agent_hooks::event::HookEventName::PostCompact,
            echo_agent_hooks::event::HookPayload::PostCompact {
                source: compact_source.into(),
            },
            None,
            None,
        )
        .await;
        let tokens_after = self.chat_state_handle.get_total_tokens().await;
        {
            let span = tracing::Span::current();
            span.record("compaction_tokens_after", tokens_after as i64);
            span.record(
                "compaction_summary_chars",
                compact_output.content.chars().count() as i64,
            );
            span.record("compaction_attempts", telemetry.attempts as i64);
            span.record(
                "compaction_degenerate_rejections",
                telemetry.degenerate_rejections as i64,
            );
            span.record(
                "compaction_input_overflow_rejections",
                input_overflow_rejections as i64,
            );
            span.record(
                "compaction_deterministic_rejections",
                telemetry.deterministic_rejections as i64,
            );
            span.record(
                "compaction_transient_rejections",
                telemetry.transient_rejections as i64,
            );
            let stop_reason = compact_output.stop_reason.as_deref().unwrap_or("stop");
            span.record("compaction_stop_reason", stop_reason);
            let outcome = if compact_output.truncated {
                CompactionOutcome::Truncated
            } else {
                CompactionOutcome::Success
            };
            span.record("compaction_outcome", outcome.as_str());
            span.record("compaction_delta_count", compact_output.delta_count as i64);
            if let Some(ms) = compact_output.ttft_ms {
                span.record("compaction_ttft_ms", ms as i64);
            }
            if let Some(ms) = compact_output.stream_ms {
                span.record("compaction_stream_ms", ms as i64);
            }
            if let Some(ms) = compact_output.itl_max_ms {
                span.record("compaction_itl_max_ms", ms as i64);
            }
        }
        compaction.complete(
            echo_agent_telemetry::events::CompactionCompleteStats {
                tokens_after,
                two_pass_used,
                segments_written,
                degenerate_retries: telemetry.degenerate_rejections,
                input_overflow_retries: input_overflow_rejections,
            },
            echo_agent_telemetry::events::CompactionTiming {
                model_wait_ms: compact_output.model_wait_ms(),
                pre_compaction_ms: Some(pre_compaction_ms),
                post_compaction_ms: Some(post_compaction_ms),
            },
        );
        Ok(())
    }
    /// Check if auto-compact should be triggered based on context window usage.
    /// Returns Some(AutoCompactTriggerInfo) if threshold is reached, None otherwise.
    pub(crate) fn should_auto_compact(
        &self,
        total_tokens: u64,
        context_window: std::num::NonZeroU64,
    ) -> Option<AutoCompactTriggerInfo> {
        let cw = context_window.get();
        if echo_agent_token_estimation::exceeds_threshold(
            total_tokens,
            cw,
            self.compaction.threshold_percent.get(),
        ) {
            let percentage = echo_agent_token_estimation::usage_percentage_u8(total_tokens, cw);
            Some(AutoCompactTriggerInfo {
                tokens_used: total_tokens,
                context_window: cw,
                percentage,
                reason: "threshold",
            })
        } else {
            None
        }
    }
    /// Returns true if the error response indicates tokens exceed the
    /// model's context window. Inspects only the model-metadata
    /// portion of the [`SamplingErrorInfo`] (the `context_window`
    /// field) against the session's tracked token estimate.
    ///
    /// Called from `handle_sampling_failure` with the
    /// `SamplingErrorInfo` the sampler hands back.
    pub(crate) async fn should_compact_on_error(
        &self,
        err: &echo_agent_sampler::SamplingErrorInfo,
    ) -> bool {
        if self
            .compaction
            .auto_compact_suppressed
            .load(std::sync::atomic::Ordering::Relaxed)
            != SUPPRESS_NONE
        {
            return false;
        }
        let Some(ref metadata) = err.model_metadata else {
            return false;
        };
        let Some(context_window) = metadata.context_window else {
            return false;
        };
        if context_window == 0 {
            return false;
        }
        let estimated_total = self.chat_state_handle.get_estimated_total_tokens().await;
        estimated_total > context_window
    }
    /// Pre-sampling compaction check. Uses `get_estimated_total_tokens()`
    /// (exact prior count + byte-estimate of items since last response) so
    /// tool results are accounted for. Returns `None` when `is_flushing`.
    pub(crate) async fn check_auto_compact_needed(&self) -> Option<AutoCompactTriggerInfo> {
        if self
            .memory
            .is_flushing
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return None;
        }
        let sampling_cfg = self.chat_state_handle.get_sampling_config().await;
        let context_window = sampling_cfg.as_ref().map(|c| c.context_window)?;
        let cw = context_window.get();
        let model = sampling_cfg
            .as_ref()
            .map(|c| c.model.clone())
            .unwrap_or_default();
        let estimated_total = self.chat_state_handle.get_estimated_total_tokens().await;
        self.signals_handle()
            .update_context_usage(estimated_total, cw);
        if self
            .compaction
            .auto_compact_suppressed
            .load(std::sync::atomic::Ordering::Relaxed)
            != SUPPRESS_NONE
        {
            return None;
        }
        if self
            .compaction
            .force_compact
            .compare_exchange(
                true,
                false,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
        {
            let percentage = echo_agent_token_estimation::usage_percentage_u8(estimated_total, cw);
            tracing::info!(
                "Forced auto-compact trigger (debug): model={model}, \
                 {percentage}% full ({estimated_total}/{cw} tokens)",
            );
            return Some(AutoCompactTriggerInfo {
                tokens_used: estimated_total,
                context_window: cw,
                percentage,
                reason: "forced",
            });
        }
        if let Some(trigger_info) = self.should_auto_compact(estimated_total, context_window) {
            tracing::info!(
                "Pre-sampling auto-compact trigger: model={model}, \
                 {}% full ({}/{} tokens)",
                trigger_info.percentage,
                trigger_info.tokens_used,
                trigger_info.context_window,
            );
            return Some(trigger_info);
        }
        None
    }
    /// Returns `Some` when tool call outputs have pushed the estimated token
    /// count past the context window, indicating pre-emptive compaction is needed.
    pub(crate) async fn check_preflight_overflow(&self) -> Option<AutoCompactTriggerInfo> {
        if self
            .compaction
            .auto_compact_suppressed
            .load(std::sync::atomic::Ordering::Relaxed)
            != SUPPRESS_NONE
        {
            return None;
        }
        let estimated_total = self.chat_state_handle.get_estimated_total_tokens().await;
        let cfg = self.chat_state_handle.get_sampling_config().await?;
        let cw = cfg.context_window.get();
        if estimated_total <= cw {
            return None;
        }
        let overflow = estimated_total.saturating_sub(cw);
        let percentage = echo_agent_token_estimation::usage_percentage_u8(estimated_total, cw);
        tracing::warn!(
            estimated_total,
            context_window = cw,
            overflow,
            model = %cfg.model,
            "CONTEXT_OVERFLOW_PREFLIGHT: estimated tokens exceed context window \
             after tool call outputs"
        );
        Some(AutoCompactTriggerInfo {
            tokens_used: estimated_total,
            context_window: cw,
            percentage,
            reason: "preflight_overflow",
        })
    }
    /// On model change: clear sticky/other suppress and compact if the window shrank.
    /// Leaves credit/auth suppress (a switch can't fix those) and short-circuits.
    /// Auth compact failures abort the turn (same as pre-sampling/preflight).
    pub(crate) async fn maybe_compact_on_model_switch(self: &Arc<Self>) -> Result<(), acp::Error> {
        self.refresh_token_if_expired().await;
        let Some(prev) = self.compaction.previous_model.take() else {
            return Ok(());
        };
        let Some(cfg) = self.chat_state_handle.get_sampling_config().await else {
            return Ok(());
        };
        if cfg.model == prev.model_slug {
            return Ok(());
        }
        if self.is_account_state_suppressed() {
            return Ok(());
        }
        self.compaction
            .auto_compact_suppressed
            .store(SUPPRESS_NONE, std::sync::atomic::Ordering::Relaxed);
        if prev.context_window <= cfg.context_window.get() {
            return Ok(());
        }
        let total_tokens = self.chat_state_handle.get_estimated_total_tokens().await;
        let Some(mut trigger_info) = self.should_auto_compact(total_tokens, cfg.context_window)
        else {
            return Ok(());
        };
        trigger_info.reason = "model_window_shrunk";
        tracing::info!(
            "Proactive model-switch compact: {} ({}) -> {} ({}), {}% full",
            prev.model_slug,
            prev.context_window,
            cfg.model,
            cfg.context_window.get(),
            trigger_info.percentage,
        );
        if let Err(e) = self.run_compact_only(trigger_info, false).await {
            tracing::error!(error = %e, "Model-switch compaction failed");
            if Self::is_auth_compact_error(&e) {
                return Err(self.surface_compact_auth_failure(e).await);
            }
        }
        Ok(())
    }
    /// Record the current model for model-switch detection on the next turn.
    pub(crate) async fn record_turn_model(&self) {
        if let Some(cfg) = self.chat_state_handle.get_sampling_config().await {
            self.compaction.previous_model.set(Some(
                crate::session::compaction_config::PreviousModelInfo {
                    model_slug: cfg.model.clone(),
                    context_window: cfg.context_window.get(),
                },
            ));
        }
    }
    /// Compact without auto-continue. The outer turn loop rebuilds and retries.
    /// Emits telemetry (`auto_compact_fired`) and UI notifications automatically.
    #[tracing::instrument(
        name = "session.compact",
        skip_all,
        fields(
            session_id = %self.session_info.id.0,
            trigger = "auto",
            mode = tracing::field::Empty,
            detail = tracing::field::Empty,
            pre_tokens = tracing::field::Empty,
            post_tokens = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    pub(crate) async fn run_compact_only(
        self: &Arc<Self>,
        trigger_info: AutoCompactTriggerInfo,
        lossy_input: bool,
    ) -> Result<(), acp::Error> {
        use crate::extensions::notification::SessionUpdate as EchoAgentSessionUpdate;
        let (_cancel, _cancel_scope) = self.compaction.cancel.enter();
        self.record_compaction_variant();
        let tokens_before = self.chat_state_handle.get_total_tokens().await;
        tracing::Span::current().record("pre_tokens", tokens_before as i64);
        echo_agent_telemetry::session_ctx::log_event(
            echo_agent_telemetry::events::AutoCompactFired {
                tokens_before: trigger_info.tokens_used,
                percentage: trigger_info.percentage,
            },
        );
        echo_agent_telemetry::unified_log::info(
            "shell.context.compaction_started",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "reason": trigger_info.reason,
                "tokens_used": trigger_info.tokens_used,
                "context_window": trigger_info.context_window,
                "usage_percent": trigger_info.percentage,
                "threshold_percent": self.compaction.threshold_percent.get(),
            })),
        );
        self.send_echo_agent_notification(EchoAgentSessionUpdate::AutoCompactStarted {
            tokens_used: trigger_info.tokens_used,
            context_window: trigger_info.context_window,
            percentage: trigger_info.percentage,
            reason: match trigger_info.reason {
                "input_budget" | "preflight_overflow" => {
                    "本次请求超出可用输入预算，正在整理上下文".to_string()
                }
                "provider_context_error" => "模型拒绝了本次上下文长度，正在整理后重试".to_string(),
                "model_family_switch" | "model_window_shrunk" => {
                    "正在整理上下文以适配切换后的模型".to_string()
                }
                "forced" => "正在按请求整理上下文".to_string(),
                _ => format!("上下文已使用 {}%，正在整理", trigger_info.percentage),
            },
        })
        .await;
        self.maybe_pre_compaction_flush(
            trigger_info.tokens_used,
            trigger_info.context_window,
            "pre_compact_on_error",
        )
        .await;
        let compact_start = std::time::Instant::now();
        let result = self
            .run_compact_inner(
                None,
                None,
                echo_agent_telemetry::events::CompactionTrigger::Auto,
                lossy_input,
            )
            .await;
        let elapsed_ms = compact_start.elapsed().as_millis() as i64;
        match result {
            Ok(()) => {
                let tokens_after = self.chat_state_handle.get_total_tokens().await;
                let span = tracing::Span::current();
                span.record("post_tokens", tokens_after as i64);
                span.record("success", true);
                self.send_echo_agent_notification(EchoAgentSessionUpdate::AutoCompactCompleted {
                    tokens_before: Some(trigger_info.tokens_used),
                    tokens_after,
                    elapsed_ms: Some(elapsed_ms),
                    summary_preview: None,
                })
                .await;
                self.emit_status_snapshot_detached();
                Ok(())
            }
            Err(e) => {
                let span = tracing::Span::current();
                span.record("success", false);
                span.record("error", e.to_string().as_str());
                echo_agent_telemetry::unified_log::info(
                    "shell.context.compaction_failed",
                    Some(self.session_info.id.0.as_ref()),
                    Some(
                        serde_json::json!({"reason": trigger_info.reason, "error": e.to_string(), "elapsed_ms": elapsed_ms}),
                    ),
                );
                let cancelled = self.compaction.cancel.is_cancelled()
                    || e.data.as_ref().and_then(|d| d.as_str()).is_some_and(|s| {
                        s.contains(crate::session::helpers::session_compact::COMPACT_CANCELLED_MSG)
                    });
                if !cancelled
                    && self
                        .compaction
                        .auto_compact_suppressed
                        .load(std::sync::atomic::Ordering::Relaxed)
                        == SUPPRESS_NONE
                {
                    self.send_echo_agent_notification(EchoAgentSessionUpdate::AutoCompactFailed {
                        error: String::new(),
                    })
                    .await;
                }
                Err(e)
            }
        }
    }
    /// Persist a compaction request artifact for offline prompt iteration.
    ///
    /// Writes `{session_dir}/compaction_requests/{request_id}.json` containing
    /// the exact ConversationItem list sent to the compaction model plus the
    /// summary (or final error) it produced. The file rides on
    /// the post-turn session archive to cloud storage via the existing per-turn upload
    /// pipeline — no separate upload path is needed.
    ///
    /// `created_at` is taken from the caller-supplied `started_at` (captured
    /// before the retry loop) rather than `Utc::now()` here, so transient
    /// retries don't skew the timestamp away from when the call actually
    /// started.
    ///
    /// Best-effort: send-failures are logged at `warn` and never surfaced to
    /// the user, because the artifact is purely for offline analysis.
    #[allow(clippy::too_many_arguments)]
    fn persist_compaction_request_artifact(
        &self,
        chat_history: Vec<ConversationItem>,
        tools: Vec<echo_agent_sampling_types::ToolSpec>,
        user_context: Option<&str>,
        use_short_prompt: bool,
        model: &str,
        trigger: echo_agent_telemetry::events::CompactionTrigger,
        summary: Option<&str>,
        error: Option<&acp::Error>,
        attempts: u32,
        attempt_details: Vec<CompactionAttempt>,
        started_at: String,
    ) {
        use crate::extensions::notification::CompactionRequestFile;
        let request_id = uuid::Uuid::new_v4().to_string();
        let trigger_str = match trigger {
            echo_agent_telemetry::events::CompactionTrigger::Manual => "manual",
            echo_agent_telemetry::events::CompactionTrigger::Auto => "auto",
        };
        let prompt_variant = if use_short_prompt {
            "short"
        } else {
            "detailed"
        };
        let error_str = error.map(|e| {
            e.data
                .as_ref()
                .and_then(|d| d.as_str())
                .unwrap_or("<no error data>")
                .to_owned()
        });
        let artifact = CompactionRequestFile {
            schema_version: 2,
            request_id,
            created_at: started_at,
            trigger: trigger_str.to_owned(),
            prompt_variant: prompt_variant.to_owned(),
            model: model.to_owned(),
            user_context: user_context.map(str::to_owned),
            chat_history,
            tools,
            summary: summary.map(str::to_owned),
            error: error_str,
            attempts,
            attempt_details,
        };
        if self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::CompactionRequest(artifact))
            .is_err()
        {
            tracing::warn!(
                session_id = %self.session_info.id.0,
                "Failed to send compaction request artifact to persistence channel"
            );
        }
    }
    /// Persist a compaction checkpoint: writes the compacted history to a separate file
    /// and records a `CompactionCheckpoint` marker in `updates.jsonl`.
    ///
    /// `auto_continue` should be `Some` when this compaction was triggered by auto-compact
    /// and an auto-continue prompt will follow.
    fn persist_compaction_checkpoint(
        &self,
        compacted_history: &[ConversationItem],
        prompt_index_at_compaction: usize,
        auto_continue: Option<crate::extensions::notification::AutoContinueInfo>,
        original_user_info: Option<String>,
    ) {
        use crate::extensions::notification::{
            CompactionCheckpointFile, CompactionCheckpointInfo,
            SessionUpdate as EchoAgentSessionUpdate,
        };
        let checkpoint_id = uuid::Uuid::new_v4().to_string();
        let checkpoint_file = format!("compaction_checkpoints/{checkpoint_id}.json");
        let created_at = chrono::Utc::now().to_rfc3339();
        let file_data = CompactionCheckpointFile {
            checkpoint_id: checkpoint_id.clone(),
            prompt_index_at_compaction,
            compacted_history: compacted_history.to_vec(),
            schema_version: 1,
            created_at: created_at.clone(),
            original_user_info,
            reread_file_paths: vec![],
        };
        if self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::CompactionCheckpoint(file_data))
            .is_err()
        {
            tracing::warn!("Failed to send compaction checkpoint file to persistence channel");
        }
        let info = CompactionCheckpointInfo {
            checkpoint_id,
            prompt_index_at_compaction,
            checkpoint_file,
            auto_continue,
            schema_version: 1,
            created_at,
        };
        self.persist_echo_agent_update_only(EchoAgentSessionUpdate::CompactionCheckpoint(
            Box::new(info),
        ));
        tracing::info!(
            prompt_index_at_compaction,
            "Persisted compaction checkpoint"
        );
    }
}
#[cfg(test)]
#[path = "compaction_inline_auto_compact_flow_tests.rs"]
mod inline_auto_compact_flow_tests;
