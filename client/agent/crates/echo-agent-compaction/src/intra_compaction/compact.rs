//! Main orchestration entry point: [`apply_intra_compaction()`] — the
//! `select → sample → guard → commit` skeleton, generic over
//! [`CompactionItemBuilder`].
//!
//! Public surface (called from harness wrappers / the agent loop):
//!
//! - [`apply_intra_compaction`] — top-level orchestrator. Reads
//!   [`IntraCompactionConfig::mode`] and dispatches to one of the
//!   same-level per-target helpers below.
//! - [`apply_steps_compaction`] — run a single pass on the agent loop's
//!   accumulated step turns.
//! - [`apply_history_compaction`] — run a single pass on prior
//!   conversation-history turns.
//!
//! All three are public so callers can either let the orchestrator pick
//! based on policy (`apply_intra_compaction`) or force a specific target
//! (`apply_steps_compaction` / `apply_history_compaction`).
//!
//! Per-harness inputs are injected through seams: token counting via
//! [`ItemTokenCounter`], metrics via [`IntraCompactionObserver`], the LLM
//! call via [`CompactionSampler`], and state commit via
//! [`CompactionStreamProc`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use super::config::{IntraCompactionConfig, IntraCompactionMode, IntraSummarizer};
use super::fit::fit_turns_for_summarizer;
use super::observer::IntraCompactionObserver;
use super::traits::{CompactionStreamProc, CompactionTarget};
use super::trigger::{IntraCompactionError, IntraCompactionResult, IntraCompactionTrigger};
// The `Shared` summarizer reuses echo-agent-build's full-replace summarization core
// (the shared summarization core lives in `code_compaction`); intra_compaction intentionally
// depends on `code_compaction` for it.
use crate::code_compaction::{
    SampleRetryError, SampledSummary, build_summary_prompt, format_compact_summary,
    sample_summary_with_retries,
};
use crate::history::filter::{
    assemble_user_queries_preamble, extract_user_queries_from_turns, separate_prior_user_queries,
};
use crate::history::prompt::{format_compaction_developer_prompt, format_compaction_user_prompt};
use crate::item::CompactionItemBuilder;
use crate::prompt::CompactionPrompt;
use crate::sampler::{CompactionSampleError, CompactionSampler};
use crate::select::select_turns_to_compact;
use crate::steps::format_compaction_prompt;
use crate::token::ItemTokenCounter;

/// Top-level intra-compaction entry point.
///
/// Reads [`IntraCompactionConfig::mode`] and dispatches to the same-level
/// per-target helpers:
///
/// - [`IntraCompactionMode::FullReplace`] (default) →
///   [`apply_full_replace_compaction`]
/// - [`IntraCompactionMode::StepsOnly`] → [`apply_steps_compaction`]
/// - [`IntraCompactionMode::HistoryOnly`] → [`apply_history_compaction`]
/// - [`IntraCompactionMode::HistoryThenSteps`] →
///   [`apply_history_compaction`] first, then [`apply_steps_compaction`]
///   only if the post-history accumulated step tokens still exceed
///   `policy.steps_trigger_ratio` of the history token count.
///
/// `active_reminder` is an optional harness-supplied `<system-reminder>`
/// (e.g. running sub-agents) appended verbatim to the summary on the
/// `FullReplace` path, so in-flight state survives the dropped tail. Ignored
/// by the partial modes (they keep a tail).
///
/// `summarizer_input_budget` is the max tokens of conversation turns fed to
/// the FullReplace summarizer (already net of response reserve + prompt
/// overhead). When `Some`, FullReplace runs [`fit_turns_for_summarizer`]
/// first. `None` skips fit (tests / callers that pre-fit).
///
/// On any error, parser state is left unchanged (the per-target helpers
/// guard this). Every terminal outcome — success or any error variant —
/// is reported to `observer` with a `status` label, so failure rates are
/// observable in harness metrics.
pub async fn apply_intra_compaction<T, S, P>(
    stream_proc: &S,
    sampler: &P,
    policy: &IntraCompactionConfig,
    trigger: IntraCompactionTrigger,
    token_counter: &dyn ItemTokenCounter<T>,
    observer: &dyn IntraCompactionObserver,
    active_reminder: Option<&str>,
    summarizer_input_budget: Option<u32>,
) -> Result<IntraCompactionResult, IntraCompactionError>
where
    T: CompactionItemBuilder + Send + Sync,
    S: CompactionStreamProc<Item = T> + ?Sized,
    P: CompactionSampler<Item = T> + ?Sized,
{
    let result = match policy.mode {
        IntraCompactionMode::FullReplace => {
            apply_full_replace_compaction(
                stream_proc,
                sampler,
                policy,
                &trigger,
                token_counter,
                observer,
                active_reminder,
                summarizer_input_budget,
            )
            .await
        }
        IntraCompactionMode::StepsOnly => {
            apply_steps_compaction(
                stream_proc,
                sampler,
                policy,
                &trigger,
                token_counter,
                observer,
            )
            .await
        }
        IntraCompactionMode::HistoryOnly => {
            apply_history_compaction(
                stream_proc,
                sampler,
                policy,
                &trigger,
                token_counter,
                observer,
            )
            .await
        }
        IntraCompactionMode::HistoryThenSteps => {
            apply_history_then_steps(
                stream_proc,
                sampler,
                policy,
                &trigger,
                token_counter,
                observer,
            )
            .await
        }
    };
    if let Err(ref e) = result {
        observer.on_error(error_status_label(e));
    }
    result
}

/// Same-level per-target helper: run one **steps** compaction pass on the
/// accumulated step turns of the given stream processor. Reads turns,
/// chooses a split point, calls the LLM (with retries), and applies the
/// result via [`CompactionStreamProc::replace_with_compaction`].
pub async fn apply_steps_compaction<T, S, P>(
    stream_proc: &S,
    sampler: &P,
    policy: &IntraCompactionConfig,
    trigger: &IntraCompactionTrigger,
    token_counter: &dyn ItemTokenCounter<T>,
    observer: &dyn IntraCompactionObserver,
) -> Result<IntraCompactionResult, IntraCompactionError>
where
    T: CompactionItemBuilder + Send + Sync,
    S: CompactionStreamProc<Item = T> + ?Sized,
    P: CompactionSampler<Item = T> + ?Sized,
{
    compact_one_pass(
        stream_proc,
        sampler,
        policy,
        trigger,
        CompactionTarget::Steps,
        token_counter,
        observer,
    )
    .await
}

/// Same-level per-target helper: run one **history** compaction pass on
/// the prior conversation-history turns of the given stream processor.
/// Same shape as [`apply_steps_compaction`] but uses the coarser history
/// prompt and reads from `get_history_turns_for_compaction`.
pub async fn apply_history_compaction<T, S, P>(
    stream_proc: &S,
    sampler: &P,
    policy: &IntraCompactionConfig,
    trigger: &IntraCompactionTrigger,
    token_counter: &dyn ItemTokenCounter<T>,
    observer: &dyn IntraCompactionObserver,
) -> Result<IntraCompactionResult, IntraCompactionError>
where
    T: CompactionItemBuilder + Send + Sync,
    S: CompactionStreamProc<Item = T> + ?Sized,
    P: CompactionSampler<Item = T> + ?Sized,
{
    compact_one_pass(
        stream_proc,
        sampler,
        policy,
        trigger,
        CompactionTarget::History,
        token_counter,
        observer,
    )
    .await
}

/// `FullReplace` strategy (default): echo-agent-build's full-replace — summarize the
/// *whole* conversation (prior history + accumulated steps) in one pass and
/// rebuild context from scratch via [`CompactionTarget::FullReplace`].
///
/// Unlike the partial modes there is no tail-keep selection and no
/// `<echo_agent_user_queries>` preamble: the shared `code_compaction` summarizer
/// (always [`IntraSummarizer::Shared`] here, regardless of `policy.summarizer`)
/// preserves user intent itself, matching echo-agent-build. The reduction and
/// `min_compactable_tokens` guards are kept for parity with the partial modes.
///
/// When `summarizer_input_budget` is `Some`, turns are **fitted** via the
/// ordered ladder (history drop → tool truncate → step drop → emergency;
/// later only if earlier still insufficient) before sampling so the compact
/// model never sees a multi-100k prompt. Reduction guard still uses the
/// **raw** pre-fit token count so a successful recovery is not discarded.
pub async fn apply_full_replace_compaction<T, S, P>(
    stream_proc: &S,
    sampler: &P,
    policy: &IntraCompactionConfig,
    trigger: &IntraCompactionTrigger,
    token_counter: &dyn ItemTokenCounter<T>,
    observer: &dyn IntraCompactionObserver,
    active_reminder: Option<&str>,
    summarizer_input_budget: Option<u32>,
) -> Result<IntraCompactionResult, IntraCompactionError>
where
    T: CompactionItemBuilder + Send + Sync,
    S: CompactionStreamProc<Item = T> + ?Sized,
    P: CompactionSampler<Item = T> + ?Sized,
{
    let start = Instant::now();

    // 1. Read history and steps separately (fit ladder needs the split).
    let history_turns = stream_proc.get_history_turns_for_compaction().await;
    let step_turns = stream_proc.get_accumulated_turns_for_compaction().await;
    let total_turns = history_turns.len() + step_turns.len();
    if total_turns == 0 {
        return Err(IntraCompactionError::NothingToCompact);
    }
    let tokens_before: u32 = history_turns
        .iter()
        .chain(step_turns.iter())
        .map(|t| token_counter.count_item_tokens(t))
        .sum();
    if tokens_before < policy.min_compactable_tokens {
        return Err(IntraCompactionError::NothingToCompact);
    }

    // 1b. Fit summarizer input via ordered ladder (later only if earlier
    // insufficient): HistoryTurnSelected → ToolTruncated → StepTurnsSelected
    // → Emergency. See `fit::FitRung` / module docs.
    let (llm_turns, fit_rung) = if let Some(budget) = summarizer_input_budget {
        let plan = fit_turns_for_summarizer(&history_turns, &step_turns, token_counter, budget);
        // Fit's Emergency keeps the newest original item when the ladder
        // emptied mid-way. Reject empty plans and zero-token fit (e.g. a
        // media-only tool turn clipped to empty text) so we do not feed the
        // summarizer nothing and commit a hallucination over the conversation.
        if plan.llm_turns.is_empty() || plan.tokens_fit == 0 {
            return Err(IntraCompactionError::NothingToCompact);
        }
        (plan.llm_turns, Some(plan.rung))
    } else {
        let mut all = history_turns;
        all.extend(step_turns);
        (all, None)
    };

    info!(
        target = CompactionTarget::FullReplace.label(),
        step = trigger.step,
        percent = trigger.percent,
        total_turns,
        llm_turns = llm_turns.len(),
        tokens_before,
        fit_rung = fit_rung.map(|r| r.as_str()).unwrap_or("none"),
        summarizer_input_budget = ?summarizer_input_budget,
        "[IntraCompaction] starting full replace"
    );

    // 2. Summarize (possibly fitted) turns through echo-agent-build's shared core.
    //    FullReplace always uses the shared summarizer (it *is* the
    //    `code_compaction` path); `policy.summarizer` is ignored for this mode.
    let summary_text = sample_shared_summary_with_retries(sampler, &llm_turns, policy).await?;

    // 2b. Preserve in-flight active agent state (e.g. running sub-agents) across
    //     the compaction. FullReplace drops the working tail, so append the
    //     harness-supplied `<system-reminder>` (verbatim ids) to the summary so
    //     the model can keep polling/cancelling them. Empty/None → no change.
    //     Shared with EchoAgent chat inter-compaction via `append_reminder_block` so
    //     both inject the reminder into the summary text identically, before the
    //     reduction guard below counts it.
    let summary_text = crate::append_reminder_block(summary_text, active_reminder);

    // 3. Build the replacement developer turn. Snapshot the summary as a
    //    cheap-to-clone `Arc<str>` before moving the owned text into the item.
    let summary: Arc<str> = Arc::from(summary_text.as_str());
    let compaction_turn = T::compaction_summary_item(summary_text);
    let tokens_after = token_counter.count_item_tokens(&compaction_turn);

    // 4. Guard: don't apply if compaction didn't help.
    if tokens_before > 0
        && tokens_after > (tokens_before as f64 * policy.max_reduction_ratio) as u32
    {
        warn!(
            target = CompactionTarget::FullReplace.label(),
            tokens_before,
            tokens_after,
            ratio = (tokens_after as f64 / tokens_before as f64),
            "[IntraCompaction] insufficient reduction, discarding"
        );
        return Err(IntraCompactionError::InsufficientReduction {
            tokens_before,
            tokens_after,
        });
    }

    // 5. Commit: replace the entire conversation with the single summary turn.
    let turns_compacted = total_turns;
    stream_proc
        .replace_with_compaction(
            CompactionTarget::FullReplace,
            turns_compacted,
            compaction_turn,
        )
        .await?;

    let elapsed = start.elapsed();
    observer.on_success(
        CompactionTarget::FullReplace,
        tokens_before,
        tokens_after,
        turns_compacted as u32,
        elapsed,
    );

    info!(
        target = CompactionTarget::FullReplace.label(),
        step = trigger.step,
        tokens_before,
        tokens_after,
        turns_compacted,
        elapsed_ms = elapsed.as_millis() as u64,
        "[IntraCompaction] completed"
    );

    Ok(IntraCompactionResult {
        tokens_before,
        tokens_after,
        turns_compacted: turns_compacted as u32,
        elapsed,
        summary,
    })
}

/// `HistoryThenSteps` strategy: compact history first; then, only if the
/// accumulated step turns still account for at least `steps_trigger_ratio`
/// of history tokens, also compact the current loop's steps.
///
/// The returned [`IntraCompactionResult`] aggregates both passes when
/// steps compaction also fires. A history-only result is returned when
/// steps compaction is gated off by the ratio.
async fn apply_history_then_steps<T, S, P>(
    stream_proc: &S,
    sampler: &P,
    policy: &IntraCompactionConfig,
    trigger: &IntraCompactionTrigger,
    token_counter: &dyn ItemTokenCounter<T>,
    observer: &dyn IntraCompactionObserver,
) -> Result<IntraCompactionResult, IntraCompactionError>
where
    T: CompactionItemBuilder + Send + Sync,
    S: CompactionStreamProc<Item = T> + ?Sized,
    P: CompactionSampler<Item = T> + ?Sized,
{
    let history_result = apply_history_compaction(
        stream_proc,
        sampler,
        policy,
        trigger,
        token_counter,
        observer,
    )
    .await;

    // If history compaction reported nothing to compact, we still try
    // steps compaction below (it may still be worthwhile). Any other
    // history error is bubbled up; the caller treats it as non-fatal.
    let history_result = match history_result {
        Ok(r) => Some(r),
        Err(IntraCompactionError::NothingToCompact) => None,
        Err(e) => return Err(e),
    };

    // Decide whether steps compaction is worth running. The threshold is
    // expressed as a ratio of step-turn tokens to history-turn tokens
    // (taken after the history compaction pass).
    let accumulated = stream_proc.get_accumulated_turns_for_compaction().await;
    let history = stream_proc.get_history_turns_for_compaction().await;
    let steps_tokens: u64 = accumulated
        .iter()
        .map(|t| token_counter.count_item_tokens(t) as u64)
        .sum();
    let history_tokens: u64 = history
        .iter()
        .map(|t| token_counter.count_item_tokens(t) as u64)
        .sum();
    let ratio_threshold_tokens = (history_tokens as f64 * policy.steps_trigger_ratio).ceil() as u64;
    let should_compact_steps = steps_tokens > ratio_threshold_tokens;

    info!(
        step = trigger.step,
        steps_tokens,
        history_tokens,
        ratio_threshold_tokens,
        should_compact_steps,
        history_compacted = history_result.is_some(),
        "[IntraCompaction] history_then_steps: post-history decision"
    );

    if !should_compact_steps {
        return history_result.ok_or(IntraCompactionError::NothingToCompact);
    }

    let steps_result = apply_steps_compaction(
        stream_proc,
        sampler,
        policy,
        trigger,
        token_counter,
        observer,
    )
    .await;

    match (history_result, steps_result) {
        (Some(h), Ok(s)) => Ok(IntraCompactionResult {
            tokens_before: h.tokens_before.saturating_add(s.tokens_before),
            tokens_after: h.tokens_after.saturating_add(s.tokens_after),
            turns_compacted: h.turns_compacted.saturating_add(s.turns_compacted),
            elapsed: h.elapsed + s.elapsed,
            // Both passes each committed their own summary turn; join them so
            // the result carries the full compacted content. Single-pass cases
            // move the existing `Arc` (no copy).
            summary: match (h.summary.is_empty(), s.summary.is_empty()) {
                (false, false) => Arc::from(format!("{}\n\n{}", h.summary, s.summary)),
                (false, true) => h.summary,
                (true, _) => s.summary,
            },
        }),
        (Some(h), Err(IntraCompactionError::NothingToCompact)) => Ok(h),
        (None, Ok(s)) => Ok(s),
        // Steps-error after a successful history pass: history is already
        // applied — surface the steps error so the caller can log it, but
        // history work is not lost (mutation is durable on the parser).
        (_, Err(e)) => Err(e),
    }
}

/// Shared implementation called by both [`apply_steps_compaction`] and
/// [`apply_history_compaction`]. Selects turns from the target's view,
/// calls the LLM with retries, and commits the result via the stream
/// processor's single mutator.
async fn compact_one_pass<T, S, P>(
    stream_proc: &S,
    sampler: &P,
    policy: &IntraCompactionConfig,
    trigger: &IntraCompactionTrigger,
    target: CompactionTarget,
    token_counter: &dyn ItemTokenCounter<T>,
    observer: &dyn IntraCompactionObserver,
) -> Result<IntraCompactionResult, IntraCompactionError>
where
    T: CompactionItemBuilder + Send + Sync,
    S: CompactionStreamProc<Item = T> + ?Sized,
    P: CompactionSampler<Item = T> + ?Sized,
{
    let start = Instant::now();

    // 1. Read source turns for this target. `FullReplace` never reaches
    //    `compact_one_pass` — it has a dedicated `apply_full_replace_compaction`
    //    (no tail-keep), so the orchestrator only routes `Steps`/`History` here.
    let source_turns = match target {
        CompactionTarget::Steps => stream_proc.get_accumulated_turns_for_compaction().await,
        CompactionTarget::History => stream_proc.get_history_turns_for_compaction().await,
        CompactionTarget::FullReplace => {
            unreachable!("FullReplace uses apply_full_replace_compaction")
        }
    };
    if source_turns.is_empty() {
        return Err(IntraCompactionError::NothingToCompact);
    }
    let token_counts: Vec<u32> = source_turns
        .iter()
        .map(|t| token_counter.count_item_tokens(t))
        .collect();

    // 2. Choose split point.
    let target_tokens =
        (trigger.context_window as u64 * policy.target_threshold_percent as u64 / 100) as u32;
    let plan = select_turns_to_compact(
        &token_counts,
        &source_turns,
        target_tokens,
        policy.min_compactable_tokens,
    )
    .ok_or(IntraCompactionError::NothingToCompact)?;

    let turns_to_compact: Vec<T> = source_turns[..plan.split_idx].to_vec();

    info!(
        target = target.label(),
        step = trigger.step,
        percent = trigger.percent,
        total_turns = source_turns.len(),
        turns_to_compact = plan.split_idx,
        tokens_to_compact = plan.tokens_to_compact,
        target_tokens,
        "[IntraCompaction] starting"
    );

    // 3a. For `History` target, split prior `<echo_agent_user_queries>` blocks
    //     out of any prior compaction summary items before sampling — same
    //     primitive inter-compaction uses, so the LLM never sees
    //     `<echo_agent_user_queries>` and won't re-emit it (which would snowball
    //     with our explicit preamble across re-compactions). `Steps` target
    //     has no user-queries semantics and skips this entirely.
    let (turns_for_llm, prior_user_queries) = match target {
        CompactionTarget::History => {
            let separated = separate_prior_user_queries(&turns_to_compact);
            (separated.turns_for_llm, separated.prior_user_queries)
        }
        CompactionTarget::Steps => (turns_to_compact.clone(), None),
        CompactionTarget::FullReplace => {
            unreachable!("FullReplace uses apply_full_replace_compaction")
        }
    };

    // 3b. Sample the summary. The *summarization algorithm* is switchable via
    //     `policy.summarizer`; everything around it — tail selection, the
    //     reduction guard, the prefix-replace commit, the Steps/History modes,
    //     and the `<echo_agent_user_queries>` preamble below — stays intra's.
    let summary_text = match policy.summarizer {
        // Previous intra algorithm: per-target prompt, bounded retry, and NO
        // output cleaning — the raw model text flows straight to the preamble.
        IntraSummarizer::Legacy => {
            let prompt = build_prompt_for_target(target)?;
            let timeout = Duration::from_secs(policy.sampling_timeout_secs);
            sample_compaction_with_retries(sampler, &turns_for_llm, &prompt, timeout, policy)
                .await?
        }
        // New (default): echo-agent-build's shared summarization core from
        // `code_compaction` — `build_summary_prompt` + degenerate-reject +
        // `format_compact_summary` cleaning — run intra-locally.
        IntraSummarizer::Shared => {
            sample_shared_summary_with_retries(sampler, &turns_for_llm, policy).await?
        }
    };

    // 3c. For `History` target, prepend a `<echo_agent_user_queries>` preamble so
    //     the original user messages + attachment refs survive the
    //     summarization. Carries forward both prior (from earlier
    //     compactions) and current (from this round's `User` turns) via
    //     the same `assemble_user_queries_preamble` helper that inter
    //     uses. (Legacy feeds the raw summary text here; Shared feeds the
    //     already-cleaned summary.)
    let final_summary_text = match target {
        CompactionTarget::History => {
            // Current user queries come from `turns_to_compact` (the
            // unstripped view) because intra has no `raw_request`.
            let current_user_queries = extract_user_queries_from_turns(
                &turns_to_compact,
                policy.user_message_truncate_chars,
            );
            let preamble = assemble_user_queries_preamble(prior_user_queries, current_user_queries);
            if preamble.is_empty() {
                summary_text
            } else {
                format!("{}{}", preamble, summary_text)
            }
        }
        CompactionTarget::Steps => summary_text,
        CompactionTarget::FullReplace => {
            unreachable!("FullReplace uses apply_full_replace_compaction")
        }
    };

    // 4. Build the replacement item. Carries category metadata so that
    //    subsequent compaction passes (inter or intra) treat it as
    //    already-compacted content. Snapshot the (possibly large) summary as a
    //    cheap-to-clone `Arc<str>` for the result before moving the owned text
    //    into the item.
    let summary: Arc<str> = Arc::from(final_summary_text.as_str());
    let compaction_turn = T::compaction_summary_item(final_summary_text);
    let tokens_after = token_counter.count_item_tokens(&compaction_turn);

    // 5. Guard: don't apply if compaction didn't help.
    if plan.tokens_to_compact > 0
        && tokens_after > (plan.tokens_to_compact as f64 * policy.max_reduction_ratio) as u32
    {
        warn!(
            target = target.label(),
            tokens_before = plan.tokens_to_compact,
            tokens_after,
            ratio = (tokens_after as f64 / plan.tokens_to_compact as f64),
            "[IntraCompaction] insufficient reduction, discarding"
        );
        return Err(IntraCompactionError::InsufficientReduction {
            tokens_before: plan.tokens_to_compact,
            tokens_after,
        });
    }

    // 6. Commit the LLM-produced summary into parser state. The trait
    //    method dispatches internally on `target` (Steps view vs History
    //    view) and rebuilds any derived state (e.g. SglangEngine).
    stream_proc
        .replace_with_compaction(target, plan.split_idx, compaction_turn)
        .await?;

    let elapsed = start.elapsed();
    observer.on_success(
        target,
        plan.tokens_to_compact,
        tokens_after,
        plan.split_idx as u32,
        elapsed,
    );

    info!(
        target = target.label(),
        step = trigger.step,
        tokens_before = plan.tokens_to_compact,
        tokens_after,
        turns_compacted = plan.split_idx,
        elapsed_ms = elapsed.as_millis() as u64,
        "[IntraCompaction] completed"
    );

    Ok(IntraCompactionResult {
        tokens_before: plan.tokens_to_compact,
        tokens_after,
        turns_compacted: plan.split_idx as u32,
        elapsed,
        summary,
    })
}

/// Build the prompt pair for the given compaction target (Legacy summarizer).
fn build_prompt_for_target(
    target: CompactionTarget,
) -> Result<CompactionPrompt, IntraCompactionError> {
    match target {
        CompactionTarget::Steps => Ok(format_compaction_prompt()),
        CompactionTarget::History => {
            let system = format_compaction_developer_prompt()
                .map_err(|e| IntraCompactionError::SamplerBuild(e.to_string()))?;
            let user = format_compaction_user_prompt()
                .map_err(|e| IntraCompactionError::SamplerBuild(e.to_string()))?;
            Ok(CompactionPrompt { system, user })
        }
        CompactionTarget::FullReplace => {
            unreachable!("FullReplace uses apply_full_replace_compaction")
        }
    }
}

/// `Shared` summarizer (default): sample through the shared retry loop
/// [`sample_summary_with_retries`](crate::code_compaction::sample_summary_with_retries)
/// — echo-agent-build's summarization core (`build_summary_prompt` + bounded retry +
/// degenerate-reject + `format_compact_summary` cleaning) — then map the
/// structured outcome onto [`IntraCompactionError`] and return the *cleaned*
/// summary on success.
///
/// The classification (degenerate/empty = transient; deterministic vs transient
/// sampler errors, incl. context-length overflow) lives in the shared loop, so
/// intra and echo-agent-build stay in lock-step. Outcome mapping:
/// - exhausted empty/degenerate run → [`IntraCompactionError::EmptyResponse`];
/// - deterministic sampler error (incl. context overflow) →
///   [`IntraCompactionError::SamplerBuild`] (terminal);
/// - transient sampler error that exhausts retries →
///   [`IntraCompactionError::SamplerStream`].
///
/// Intra observes terminally (via [`IntraCompactionObserver`]), not per-attempt,
/// so it passes the no-op `()` observer to the shared loop.
async fn sample_shared_summary_with_retries<T, P>(
    sampler: &P,
    turns: &[T],
    policy: &IntraCompactionConfig,
) -> Result<String, IntraCompactionError>
where
    T: Send + Sync,
    P: CompactionSampler<Item = T> + ?Sized,
{
    // echo-agent-build appends the summarization prompt as the final user message;
    // there is no separate system prompt for the compaction call.
    let prompt = CompactionPrompt {
        system: String::new(),
        user: build_summary_prompt(None),
    };
    let timeout = Duration::from_secs(policy.sampling_timeout_secs);

    match sample_summary_with_retries(
        sampler,
        turns,
        &prompt,
        policy.max_attempts,
        Duration::from_secs(policy.retry_delay_secs),
        timeout,
        &(),
    )
    .await
    {
        // echo-agent-build returns the raw summary and cleans it in its assembler;
        // intra has no assembler, so it cleans here (pre-refactor behavior).
        Ok(SampledSummary { summary, .. }) => Ok(format_compact_summary(&summary)),
        Err(SampleRetryError::Empty { .. }) => Err(IntraCompactionError::EmptyResponse),
        Err(SampleRetryError::Failure {
            message,
            deterministic,
            ..
        }) => {
            if deterministic {
                Err(IntraCompactionError::SamplerBuild(message))
            } else {
                Err(IntraCompactionError::SamplerStream(message))
            }
        }
    }
}

/// Map an [`IntraCompactionError`] to a stable, low-cardinality `status`
/// metric label. Keep these in sync with the doc string on EchoAgent chat's
/// `IntraCompactionCount` metric.
pub fn error_status_label(err: &IntraCompactionError) -> &'static str {
    match err {
        IntraCompactionError::NothingToCompact => "nothing_to_compact",
        IntraCompactionError::Timeout => "timeout",
        IntraCompactionError::EmptyResponse => "empty_response",
        IntraCompactionError::InsufficientReduction { .. } => "insufficient_reduction",
        IntraCompactionError::InvalidSplit { .. } => "invalid_split",
        IntraCompactionError::Unsupported => "unsupported",
        IntraCompactionError::SamplerBuild(_) => "sampler_build",
        IntraCompactionError::SamplerStart(_) => "sampler_start",
        IntraCompactionError::SamplerStream(_) => "sampler_stream",
        IntraCompactionError::Apply(_) => "apply",
    }
}

/// Call `sampler.sample_compaction` with bounded retries on transient failures
/// (Legacy summarizer). Returns only the response text (thinking channel is
/// discarded for intra-compaction).
async fn sample_compaction_with_retries<T, P>(
    sampler: &P,
    turns: &[T],
    prompt: &CompactionPrompt,
    timeout: Duration,
    policy: &IntraCompactionConfig,
) -> Result<String, IntraCompactionError>
where
    T: Send + Sync,
    P: CompactionSampler<Item = T> + ?Sized,
{
    let max_attempts = policy.max_attempts.max(1);
    let mut last_err: Option<IntraCompactionError> = None;
    for attempt in 1..=max_attempts {
        match sampler.sample_compaction(turns, prompt, timeout).await {
            Ok(output) => return Ok(output.response),
            Err(e) => {
                let intra_err = compaction_sample_error_to_intra(e);
                if is_transient(&intra_err) && attempt < max_attempts {
                    warn!(
                        attempt,
                        error = %intra_err,
                        "[IntraCompaction] transient sampler error, retrying"
                    );
                    tokio::time::sleep(Duration::from_secs(policy.retry_delay_secs)).await;
                    last_err = Some(intra_err);
                } else {
                    return Err(intra_err);
                }
            }
        }
    }
    Err(last_err.unwrap_or(IntraCompactionError::EmptyResponse))
}

/// Convert a [`CompactionSampleError`] to [`IntraCompactionError`] (Legacy
/// summarizer).
///
/// Structured variants map directly. The `Other` fallback string-matches
/// the literal error messages produced by the EchoAgent chat sampler —
/// keep these in sync if either side changes (the
/// `compaction_sample_error_to_intra*` tests below guard the mapping).
fn compaction_sample_error_to_intra(err: CompactionSampleError) -> IntraCompactionError {
    match err {
        CompactionSampleError::Timeout { .. } => IntraCompactionError::Timeout,
        CompactionSampleError::Build(msg) => IntraCompactionError::SamplerBuild(msg),
        CompactionSampleError::Start(msg) => IntraCompactionError::SamplerStart(msg),
        CompactionSampleError::EmptyResponse => IntraCompactionError::EmptyResponse,
        CompactionSampleError::Other(e) => {
            let msg = e.to_string();
            if msg.contains("Failed to build") {
                IntraCompactionError::SamplerBuild(msg)
            } else if msg.contains("Failed to start") {
                IntraCompactionError::SamplerStart(msg)
            } else if msg.contains("no response channel content") {
                IntraCompactionError::EmptyResponse
            } else {
                IntraCompactionError::SamplerStream(msg)
            }
        }
    }
}

/// Whether this error is worth retrying (Legacy summarizer).
fn is_transient(err: &IntraCompactionError) -> bool {
    matches!(
        err,
        IntraCompactionError::Timeout
            | IntraCompactionError::EmptyResponse
            | IntraCompactionError::SamplerStream(_)
            | IntraCompactionError::SamplerStart(_)
    )
    // Deterministic: SamplerBuild (config error), Unsupported, InvalidSplit,
    // InsufficientReduction, NothingToCompact, Apply.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use crate::item::{CompactionFileRef, CompactionItem, CompactionRole};
    use crate::sampler::LlmCompactionOutput;

    /// A non-degenerate mock summary. The default `Shared` summarizer rejects
    /// summaries whose cleaned seed is shorter than
    /// [`crate::code_compaction::MIN_SUMMARY_SEED_CHARS`] (500), so intra tests that
    /// expect a *successful* sample under `Shared` must return at least that
    /// much.
    fn long_summary() -> String {
        format!("Summary of the work so far. {}", "detail ".repeat(100))
    }

    /// Guards against accidentally adding a new `IntraCompactionError` variant
    /// without giving it a metric label. The `match` in `error_status_label`
    /// is exhaustive (no `_ =>`), so the compiler will fail this test on a
    /// missing arm.
    #[test]
    fn every_error_variant_has_a_label() {
        let cases = [
            (IntraCompactionError::NothingToCompact, "nothing_to_compact"),
            (IntraCompactionError::Timeout, "timeout"),
            (IntraCompactionError::EmptyResponse, "empty_response"),
            (
                IntraCompactionError::InsufficientReduction {
                    tokens_before: 1,
                    tokens_after: 2,
                },
                "insufficient_reduction",
            ),
            (
                IntraCompactionError::InvalidSplit {
                    requested: 1,
                    available: 0,
                },
                "invalid_split",
            ),
            (IntraCompactionError::Unsupported, "unsupported"),
            (
                IntraCompactionError::SamplerBuild("x".into()),
                "sampler_build",
            ),
            (
                IntraCompactionError::SamplerStart("x".into()),
                "sampler_start",
            ),
            (
                IntraCompactionError::SamplerStream("x".into()),
                "sampler_stream",
            ),
            (IntraCompactionError::Apply("x".into()), "apply"),
        ];
        for (err, expected) in cases {
            assert_eq!(error_status_label(&err), expected, "label for {err:?}");
        }
    }

    // ── Legacy summarizer: error mapping ──

    #[test]
    fn compaction_sample_error_to_intra_maps_timeout() {
        let intra = compaction_sample_error_to_intra(CompactionSampleError::Timeout {
            timeout_secs: 60,
            collected_bytes: 0,
        });
        assert!(matches!(intra, IntraCompactionError::Timeout));
    }

    #[test]
    fn compaction_sample_error_to_intra_maps_sampler_build() {
        let intra = compaction_sample_error_to_intra(CompactionSampleError::Other(
            anyhow::anyhow!("Failed to build AgenticScheduler: config error"),
        ));
        assert!(matches!(intra, IntraCompactionError::SamplerBuild(_)));
    }

    #[test]
    fn compaction_sample_error_to_intra_maps_sampler_start() {
        let intra = compaction_sample_error_to_intra(CompactionSampleError::Other(
            anyhow::anyhow!("Failed to start compaction sample: stream error"),
        ));
        assert!(matches!(intra, IntraCompactionError::SamplerStart(_)));
    }

    #[test]
    fn compaction_sample_error_to_intra_maps_empty_response() {
        // The literal message emitted by the EchoAgent chat sampler when the
        // response channel produces no content.
        let intra = compaction_sample_error_to_intra(CompactionSampleError::Other(
            anyhow::anyhow!("Compaction scheduler returned no response channel content"),
        ));
        assert!(
            matches!(intra, IntraCompactionError::EmptyResponse),
            "expected EmptyResponse, got {intra:?}"
        );
    }

    #[test]
    fn compaction_sample_error_to_intra_maps_other_to_sampler_stream() {
        let intra = compaction_sample_error_to_intra(CompactionSampleError::Other(
            anyhow::anyhow!("Compaction scheduler error: kind=Parser, message=boom"),
        ));
        assert!(matches!(intra, IntraCompactionError::SamplerStream(_)));
    }

    #[test]
    fn compaction_sample_error_to_intra_maps_structured_variants() {
        assert!(matches!(
            compaction_sample_error_to_intra(CompactionSampleError::Build("bad config".into())),
            IntraCompactionError::SamplerBuild(_)
        ));
        assert!(matches!(
            compaction_sample_error_to_intra(CompactionSampleError::Start("no stream".into())),
            IntraCompactionError::SamplerStart(_)
        ));
        assert!(matches!(
            compaction_sample_error_to_intra(CompactionSampleError::EmptyResponse),
            IntraCompactionError::EmptyResponse
        ));
    }

    #[test]
    fn structured_variants_are_classified_for_retry() {
        // Build is deterministic (no retry); Start/EmptyResponse transient.
        assert!(!is_transient(&compaction_sample_error_to_intra(
            CompactionSampleError::Build("x".into())
        )));
        assert!(is_transient(&compaction_sample_error_to_intra(
            CompactionSampleError::Start("x".into())
        )));
        assert!(is_transient(&compaction_sample_error_to_intra(
            CompactionSampleError::EmptyResponse
        )));
    }

    // ── generic orchestrator over a pure mock item ─────────────────────

    /// Mock item: a `(role, text)` pair with deterministic token counting.
    #[derive(Debug, Clone)]
    struct MockItem {
        role: CompactionRole,
        text: String,
        summary: bool,
    }

    impl MockItem {
        fn user(text: &str) -> Self {
            Self {
                role: CompactionRole::User,
                text: text.to_string(),
                summary: false,
            }
        }
    }

    impl CompactionItem for MockItem {
        fn role(&self) -> CompactionRole {
            self.role
        }
        fn text(&self) -> Option<String> {
            Some(self.text.clone())
        }
        fn has_tool_requests(&self) -> bool {
            false
        }
        fn is_compaction_summary(&self) -> bool {
            self.summary
        }
        fn attachment_refs(&self) -> Vec<CompactionFileRef> {
            Vec::new()
        }
    }

    impl CompactionItemBuilder for MockItem {
        fn compaction_summary_item(text: String) -> Self {
            Self {
                role: CompactionRole::Developer,
                text,
                summary: true,
            }
        }
        fn strip_tool_content(&self) -> Option<Self> {
            Some(self.clone())
        }
    }

    /// Deterministic counter: 1 token per 4 chars (min 1).
    struct CharCounter;
    impl ItemTokenCounter<MockItem> for CharCounter {
        fn count_item_tokens(&self, item: &MockItem) -> u32 {
            (item.text.chars().count() as u32 / 4).max(1)
        }
    }

    /// Mock stream proc backed by an in-memory Vec (steps view only).
    struct MockStreamProc {
        turns: Mutex<Vec<MockItem>>,
        applied: Mutex<Option<(usize, MockItem)>>,
    }

    impl MockStreamProc {
        fn with_turns(turns: Vec<MockItem>) -> Self {
            Self {
                turns: Mutex::new(turns),
                applied: Mutex::new(None),
            }
        }
        fn was_applied(&self) -> bool {
            self.applied.lock().unwrap().is_some()
        }
    }

    #[async_trait]
    impl CompactionStreamProc for MockStreamProc {
        type Item = MockItem;

        async fn get_accumulated_turns_for_compaction(&self) -> Vec<MockItem> {
            self.turns.lock().unwrap().clone()
        }
        async fn replace_with_compaction(
            &self,
            target: CompactionTarget,
            n: usize,
            turn: MockItem,
        ) -> Result<(), IntraCompactionError> {
            let mut guard = self.turns.lock().unwrap();
            match target {
                CompactionTarget::Steps => {
                    if n > guard.len() {
                        return Err(IntraCompactionError::InvalidSplit {
                            requested: n,
                            available: guard.len(),
                        });
                    }
                    let kept: Vec<_> = guard.split_off(n);
                    *guard = std::iter::once(turn.clone()).chain(kept).collect();
                }
                // FullReplace: drop the whole conversation, keep only the summary.
                CompactionTarget::FullReplace => {
                    *guard = vec![turn.clone()];
                }
                CompactionTarget::History => return Err(IntraCompactionError::Unsupported),
            }
            *self.applied.lock().unwrap() = Some((n, turn));
            Ok(())
        }
    }

    /// Mock sampler with scripted responses.
    struct MockSampler {
        responses: Mutex<Vec<Result<String, CompactionSampleError>>>,
        calls: Mutex<usize>,
    }

    impl MockSampler {
        fn returns(text: &str) -> Self {
            Self {
                responses: Mutex::new(vec![Ok(text.to_string())]),
                calls: Mutex::new(0),
            }
        }
        fn fails_then(errors: Vec<CompactionSampleError>, success: &str) -> Self {
            let mut responses: Vec<_> = errors.into_iter().map(Err).collect();
            responses.push(Ok(success.to_string()));
            Self {
                responses: Mutex::new(responses),
                calls: Mutex::new(0),
            }
        }
        fn call_count(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl CompactionSampler for MockSampler {
        type Item = MockItem;

        async fn sample_compaction(
            &self,
            _turns: &[MockItem],
            _prompt: &CompactionPrompt,
            _timeout: Duration,
        ) -> Result<LlmCompactionOutput, CompactionSampleError> {
            *self.calls.lock().unwrap() += 1;
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err(CompactionSampleError::Other(anyhow::anyhow!(
                    "no more mock responses"
                )));
            }
            responses.remove(0).map(|text| LlmCompactionOutput {
                response: text,
                thinking: String::new(),
            })
        }
    }

    fn enabled_policy() -> IntraCompactionConfig {
        IntraCompactionConfig {
            enabled: true,
            // These tests exercise the steps (tail-keep) path; pin the mode so
            // they stay independent of the crate default (now `FullReplace`).
            mode: IntraCompactionMode::StepsOnly,
            trigger_threshold_percent: 85,
            target_threshold_percent: 50,
            min_steps_before_compact: 1,
            min_compactable_tokens: 1,
            max_reduction_ratio: 0.8,
            max_attempts: 2,
            retry_delay_secs: 0,
            sampling_timeout_secs: 5,
            ..Default::default()
        }
    }

    fn trigger() -> IntraCompactionTrigger {
        IntraCompactionTrigger {
            last_prompt_tokens: 900,
            context_window: 1000,
            percent: 90,
            step: 5,
        }
    }

    /// Recording observer.
    #[derive(Default)]
    struct RecordingObserver {
        errors: Mutex<Vec<&'static str>>,
        successes: Mutex<Vec<CompactionTarget>>,
    }
    impl IntraCompactionObserver for RecordingObserver {
        fn on_error(&self, status: &'static str) {
            self.errors.lock().unwrap().push(status);
        }
        fn on_success(&self, target: CompactionTarget, _b: u32, _a: u32, _t: u32, _e: Duration) {
            self.successes.lock().unwrap().push(target);
        }
    }

    #[tokio::test]
    async fn compact_replaces_turns_on_success_and_notifies_observer() {
        // 6 turns × 500 tokens (2000 chars / 4); ctx 1000, target 50% → 500
        // → keep the newest 1 turn, compact the oldest 5 (2500 tokens). The
        // summary must be non-degenerate (>= 500 cleaned chars) for the shared
        // sampler to accept it, yet still pass the reduction guard (≤ 2000).
        let turns: Vec<_> = (0..6).map(|_| MockItem::user(&"x".repeat(2000))).collect();
        let sp = MockStreamProc::with_turns(turns);
        let sampler = MockSampler::returns(&long_summary());
        let obs = RecordingObserver::default();

        let result = apply_intra_compaction(
            &sp,
            &sampler,
            &enabled_policy(),
            trigger(),
            &CharCounter,
            &obs,
            None,
            None,
        )
        .await;
        let r = result.expect("should succeed");
        assert!(r.turns_compacted > 0);
        // The result carries the LLM summary (the developer-turn content).
        assert!(
            !r.summary.is_empty(),
            "result should carry the summary text"
        );
        assert!(sp.was_applied());
        assert_eq!(
            obs.successes.lock().unwrap().as_slice(),
            &[CompactionTarget::Steps]
        );
        assert!(obs.errors.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn compact_skips_on_insufficient_reduction() {
        let turns: Vec<_> = (0..8).map(|_| MockItem::user(&"y".repeat(400))).collect();
        let sp = MockStreamProc::with_turns(turns);
        // Summary is bigger than the originals → ratio > 0.8 → reject.
        let sampler = MockSampler::returns(&"z".repeat(8000));
        let obs = RecordingObserver::default();

        let result = apply_intra_compaction(
            &sp,
            &sampler,
            &enabled_policy(),
            trigger(),
            &CharCounter,
            &obs,
            None,
            None,
        )
        .await;
        assert!(
            matches!(
                result,
                Err(IntraCompactionError::InsufficientReduction { .. })
            ),
            "expected InsufficientReduction, got {result:?}"
        );
        assert!(!sp.was_applied(), "state must not be mutated on failure");
        assert_eq!(
            obs.errors.lock().unwrap().as_slice(),
            &["insufficient_reduction"]
        );
    }

    #[tokio::test]
    async fn compact_returns_nothing_to_compact_on_empty() {
        let sp = MockStreamProc::with_turns(vec![]);
        let sampler = MockSampler::returns("anything");
        let result = apply_intra_compaction(
            &sp,
            &sampler,
            &enabled_policy(),
            trigger(),
            &CharCounter,
            &(),
            None,
            None,
        )
        .await;
        assert!(matches!(
            result,
            Err(IntraCompactionError::NothingToCompact)
        ));
        assert_eq!(sampler.call_count(), 0, "LLM should not be called");
    }

    #[tokio::test]
    async fn compact_retries_transient_errors() {
        let turns: Vec<_> = (0..6).map(|_| MockItem::user(&"x".repeat(2000))).collect();
        let sp = MockStreamProc::with_turns(turns);
        let after_retry = long_summary();
        let sampler = MockSampler::fails_then(
            vec![CompactionSampleError::Timeout {
                timeout_secs: 60,
                collected_bytes: 0,
            }],
            &after_retry,
        );
        let policy = IntraCompactionConfig {
            max_attempts: 3,
            ..enabled_policy()
        };

        let result = apply_intra_compaction(
            &sp,
            &sampler,
            &policy,
            trigger(),
            &CharCounter,
            &(),
            None,
            None,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(sampler.call_count(), 2, "should retry once then succeed");
    }

    #[tokio::test]
    async fn compact_does_not_retry_deterministic_errors() {
        let turns: Vec<_> = (0..6).map(|_| MockItem::user(&"x".repeat(400))).collect();
        let sp = MockStreamProc::with_turns(turns);
        let sampler = MockSampler::fails_then(
            vec![
                CompactionSampleError::Build("config error".into()),
                CompactionSampleError::Build("config error".into()),
            ],
            "never reached",
        );

        let result = apply_intra_compaction(
            &sp,
            &sampler,
            &enabled_policy(),
            trigger(),
            &CharCounter,
            &(),
            None,
            None,
        )
        .await;
        assert!(matches!(result, Err(IntraCompactionError::SamplerBuild(_))));
        assert_eq!(
            sampler.call_count(),
            1,
            "deterministic error should not retry"
        );
    }

    #[tokio::test]
    async fn compact_propagates_apply_failure() {
        // 2000-char turns so the (non-degenerate) summary still passes the
        // reduction guard and reaches the commit step that fails.
        let turns: Vec<_> = (0..6).map(|_| MockItem::user(&"x".repeat(2000))).collect();

        struct FailingApply {
            turns: Mutex<Vec<MockItem>>,
        }
        #[async_trait]
        impl CompactionStreamProc for FailingApply {
            type Item = MockItem;
            async fn get_accumulated_turns_for_compaction(&self) -> Vec<MockItem> {
                self.turns.lock().unwrap().clone()
            }
            async fn replace_with_compaction(
                &self,
                _target: CompactionTarget,
                _n: usize,
                _t: MockItem,
            ) -> Result<(), IntraCompactionError> {
                Err(IntraCompactionError::Apply("engine rebuild failed".into()))
            }
        }
        let sp = FailingApply {
            turns: Mutex::new(turns),
        };
        let sampler = MockSampler::returns(&long_summary());

        let result = apply_intra_compaction(
            &sp,
            &sampler,
            &enabled_policy(),
            trigger(),
            &CharCounter,
            &(),
            None,
            None,
        )
        .await;
        assert!(matches!(result, Err(IntraCompactionError::Apply(_))));
    }

    // ── FullReplace mode (default): whole-conversation replace ────────

    fn full_replace_policy() -> IntraCompactionConfig {
        IntraCompactionConfig {
            mode: IntraCompactionMode::FullReplace,
            ..enabled_policy()
        }
    }

    #[tokio::test]
    async fn full_replace_compacts_whole_conversation_and_notifies_observer() {
        // 6 turns × 500 tokens (2000 chars / 4) = 3000 tokens. FullReplace
        // summarizes *all* of them (no tail-keep) into one developer turn.
        let turns: Vec<_> = (0..6).map(|_| MockItem::user(&"x".repeat(2000))).collect();
        let sp = MockStreamProc::with_turns(turns);
        let sampler = MockSampler::returns(&long_summary());
        let obs = RecordingObserver::default();

        let r = apply_intra_compaction(
            &sp,
            &sampler,
            &full_replace_policy(),
            trigger(),
            &CharCounter,
            &obs,
            None,
            None,
        )
        .await
        .expect("should succeed");

        // Every turn was folded into the summary; one sampling pass.
        assert_eq!(r.turns_compacted, 6);
        assert!(!r.summary.is_empty());
        assert_eq!(sampler.call_count(), 1);
        // The mock now holds only the single summary turn.
        assert_eq!(sp.turns.lock().unwrap().len(), 1);
        // Observer recorded a `FullReplace` success (drives the
        // `target="full_replace"` metric).
        assert_eq!(
            obs.successes.lock().unwrap().as_slice(),
            &[CompactionTarget::FullReplace]
        );
        assert!(obs.errors.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn full_replace_appends_active_reminder_to_summary() {
        // The harness-supplied `<system-reminder>` (e.g. running sub-agents) is
        // appended verbatim to the FullReplace summary developer turn.
        let turns: Vec<_> = (0..6).map(|_| MockItem::user(&"x".repeat(2000))).collect();
        let sp = MockStreamProc::with_turns(turns);
        let sampler = MockSampler::returns(&long_summary());
        let obs = RecordingObserver::default();
        let reminder =
            "<system-reminder>\n## Running Subagents\n- subagent_id: `sa-123`\n</system-reminder>";

        let r = apply_intra_compaction(
            &sp,
            &sampler,
            &full_replace_policy(),
            trigger(),
            &CharCounter,
            &obs,
            Some(reminder),
            None,
        )
        .await
        .expect("should succeed");

        // The committed summary developer turn carries the verbatim id.
        assert!(
            r.summary.contains("sa-123") && r.summary.contains("## Running Subagents"),
            "summary missing reminder: {}",
            r.summary
        );
        let committed = sp.turns.lock().unwrap();
        assert_eq!(committed.len(), 1);
        assert!(committed[0].text().unwrap().contains("sa-123"));
    }

    #[tokio::test]
    async fn full_replace_nothing_to_compact_on_empty() {
        let sp = MockStreamProc::with_turns(vec![]);
        let sampler = MockSampler::returns(&long_summary());
        let result = apply_intra_compaction(
            &sp,
            &sampler,
            &full_replace_policy(),
            trigger(),
            &CharCounter,
            &(),
            None,
            None,
        )
        .await;
        assert!(matches!(
            result,
            Err(IntraCompactionError::NothingToCompact)
        ));
        assert_eq!(sampler.call_count(), 0, "LLM should not be called");
    }

    #[tokio::test]
    async fn full_replace_skips_below_min_compactable_tokens() {
        // 2 turns × 1 token = 2 tokens, below `min_compactable_tokens`.
        let turns: Vec<_> = (0..2).map(|_| MockItem::user("x")).collect();
        let sp = MockStreamProc::with_turns(turns);
        let sampler = MockSampler::returns(&long_summary());
        let policy = IntraCompactionConfig {
            min_compactable_tokens: 1_000,
            ..full_replace_policy()
        };
        let result = apply_intra_compaction(
            &sp,
            &sampler,
            &policy,
            trigger(),
            &CharCounter,
            &(),
            None,
            None,
        )
        .await;
        assert!(matches!(
            result,
            Err(IntraCompactionError::NothingToCompact)
        ));
        assert_eq!(sampler.call_count(), 0, "LLM should not be called");
        assert!(!sp.was_applied());
    }

    // ── Shared summarizer (default): direct helper coverage ───────────

    #[tokio::test]
    async fn shared_summarizer_rejects_degenerate_then_errors() {
        // A too-short summary is degenerate; `Shared` retries (max_attempts=2)
        // and, when retries are exhausted, errors as EmptyResponse.
        let sampler = MockSampler {
            responses: Mutex::new(vec![Ok("short".to_string()), Ok("still short".to_string())]),
            calls: Mutex::new(0),
        };
        let policy = IntraCompactionConfig {
            max_attempts: 2,
            ..enabled_policy()
        };
        let turns = vec![MockItem::user("hi")];
        let result = sample_shared_summary_with_retries(&sampler, &turns, &policy).await;
        assert!(
            matches!(result, Err(IntraCompactionError::EmptyResponse)),
            "expected EmptyResponse, got {result:?}"
        );
        assert_eq!(
            sampler.call_count(),
            2,
            "degenerate summary should be retried"
        );
    }

    #[tokio::test]
    async fn shared_summarizer_bails_on_deterministic_error() {
        // A deterministic (Build) failure short-circuits without retrying and
        // maps to SamplerBuild (terminal).
        let sampler = MockSampler::fails_then(
            vec![CompactionSampleError::Build("config".into())],
            "unused",
        );
        let turns = vec![MockItem::user("hi")];
        let result = sample_shared_summary_with_retries(&sampler, &turns, &enabled_policy()).await;
        assert!(
            matches!(result, Err(IntraCompactionError::SamplerBuild(_))),
            "expected SamplerBuild, got {result:?}"
        );
        assert_eq!(
            sampler.call_count(),
            1,
            "deterministic error must not retry"
        );
    }

    #[tokio::test]
    async fn shared_summarizer_cleans_successful_summary() {
        // A non-degenerate summary wrapped in <analysis>/<summary> is cleaned:
        // scratchpad stripped, tags neutralized, "Summary:" heading produced.
        let raw = format!(
            "<analysis>\nthinking\n</analysis>\n<summary>\n1. Primary Request: {}\n</summary>",
            "detail ".repeat(100)
        );
        let sampler = MockSampler::returns(&raw);
        let turns = vec![MockItem::user("hi")];
        let cleaned = sample_shared_summary_with_retries(&sampler, &turns, &enabled_policy())
            .await
            .expect("non-degenerate summary should succeed");
        assert!(
            cleaned.starts_with("Summary:"),
            "expected cleaned heading: {cleaned:?}"
        );
        assert!(!cleaned.contains("thinking"), "scratchpad must be stripped");
        assert!(!cleaned.contains("<summary>"), "tags must be neutralized");
    }

    // ── Legacy summarizer: end-to-end switch ──────────────────────────

    #[tokio::test]
    async fn legacy_summarizer_accepts_short_uncleaned_summary() {
        // Legacy has no degenerate floor and does NO cleaning: a short raw
        // summary is accepted verbatim (would be rejected under `Shared`).
        let turns: Vec<_> = (0..6).map(|_| MockItem::user(&"x".repeat(400))).collect();
        let sp = MockStreamProc::with_turns(turns);
        let sampler = MockSampler::returns("compacted summary");
        let policy = IntraCompactionConfig {
            summarizer: IntraSummarizer::Legacy,
            ..enabled_policy()
        };
        let result = apply_intra_compaction(
            &sp,
            &sampler,
            &policy,
            trigger(),
            &CharCounter,
            &(),
            None,
            None,
        )
        .await;
        assert!(
            result.is_ok(),
            "legacy short summary should succeed: {result:?}"
        );
        assert!(sp.was_applied());
        assert_eq!(sampler.call_count(), 1);
    }

    /// `Arc<MockItem>` also satisfies the builder bound via the blanket impl
    /// — guards the forwarding that EchoAgent chat (`Arc<EchoAgentTurn>`) relies on.
    #[test]
    fn arc_blanket_impl_forwards_builder_methods() {
        let item = Arc::new(MockItem::user("hello"));
        assert_eq!(item.role(), CompactionRole::User);
        assert!(!item.is_compaction_summary());
        let summary =
            <Arc<MockItem> as CompactionItemBuilder>::compaction_summary_item("sum".to_string());
        assert!(summary.is_compaction_summary());
        assert!(item.strip_tool_content().is_some());
    }
}
