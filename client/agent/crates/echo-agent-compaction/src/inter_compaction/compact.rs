//! Inter-compaction chunked pipeline (shared core).
//!
//! Single pipeline shared by both `CompactionStrategy::Basic` and
//! `CompactionStrategy::DivideAndConquer`. The only difference between
//! the two is the per-chunk token budget:
//!
//! - **Basic** → unbounded chunk budget → exactly one chunk.
//! - **DivideAndConquer** → `config.dnc_chunk_token_limit` → N chunks.
//!
//! Everything else — turn filtering, prior-compaction user-query
//! extraction, chunk summarisation, and the final `<echo_agent_user_queries>`
//! + `<chunk_summary>` assembly — is shared. The harness supplies the
//! candidate items, the *current* user-queries preamble (EchoAgent chat
//! extracts it from the raw `ChatCompletionRequest`), the sampler, the
//! token counter, and an observer for metrics.

use std::time::{Duration, Instant};

use tracing::info;

use crate::history::filter::{
    assemble_user_queries_preamble, filter_turns_for_inter_compaction, separate_prior_user_queries,
    wrap_chunk_analysis,
};
use crate::history::prompt::{format_compaction_developer_prompt, format_compaction_user_prompt};
use crate::history::types::CompactionStrategy;
use crate::item::CompactionItemBuilder;
use crate::prompt::CompactionPrompt;
use crate::sampler::{CompactionSampleError, CompactionSampler, LlmCompactionOutput};
use crate::token::ItemTokenCounter;

use super::config::InterCompactionConfig;
use super::observer::InterCompactionObserver;

/// Sentinel chunk budget used by [`CompactionStrategy::Basic`] so the
/// chunking loop emits exactly one chunk.
const UNBOUNDED_CHUNK_LIMIT: u32 = u32::MAX;

/// Output of the shared chunked pipeline — assembled text, not yet wrapped
/// into a harness message type.
#[derive(Debug, Clone)]
pub struct ChunkedCompactionOutput {
    /// `<echo_agent_user_queries>` preamble + `<chunk_summary index="i">` blocks.
    /// The harness wraps this into its summary-carrier message.
    pub combined_text: String,
    /// Thinking-channel output: `<chunk_analysis>` blocks. Empty when the
    /// model produced no thinking output. Stored for audit/debug only.
    pub analysis_text: String,
}

/// Shared chunked pipeline.
///
/// Steps:
/// 1. Filter items with
///    [`filter_turns_for_inter_compaction`](crate::history::filter::filter_turns_for_inter_compaction).
/// 2. [`separate_prior_user_queries`] — split prior `<echo_agent_user_queries>`
///    blocks out of every prior compaction summary item. The LLM never sees
///    them. Shared with intra-compaction's `History` target so both
///    pipelines handle re-compactions identically.
/// 3. Walk the LLM-safe item list. Flush a chunk whenever the running
///    token count would exceed the chunk budget (`UNBOUNDED_CHUNK_LIMIT`
///    for Basic — single chunk).
/// 4. Combine `prior_user_queries + current_user_queries + <chunk_summary>`
///    blocks into the final summary text via
///    [`assemble_user_queries_preamble`]; combine the per-chunk
///    `thinking` channels into the analysis text.
///
/// `current_user_queries` is the harness-extracted preamble for *this*
/// round's user messages (EchoAgent chat: verbatim from the raw request, with
/// attachment refs). `conversation_id` / `response_id` are threaded
/// through for log correlation only.
///
/// Observer events (the EchoAgent chat observer maps them to the
/// pre-unification metrics):
/// - [`InterCompactionObserver::on_recompaction`] when prior-compaction
///   summary items are found.
/// - [`InterCompactionObserver::on_chunk_count`] — chunk count after
///   assembly (always 1 for Basic; N for DnC).
/// - [`InterCompactionObserver::on_chunk_sampled`] — per-chunk LLM latency.
#[allow(clippy::too_many_arguments)]
pub async fn sample_compaction_chunked<T: CompactionItemBuilder + Send + Sync>(
    turns: &[T],
    current_user_queries: Option<String>,
    conversation_id: &str,
    response_id: &str,
    start_response_id: &str,
    config: &InterCompactionConfig,
    sampler: &dyn CompactionSampler<Item = T>,
    token_counter: &dyn ItemTokenCounter<T>,
    observer: &dyn InterCompactionObserver,
) -> Result<ChunkedCompactionOutput, CompactionSampleError> {
    let chunk_token_limit = match config.compaction_strategy {
        CompactionStrategy::Basic => UNBOUNDED_CHUNK_LIMIT,
        CompactionStrategy::DivideAndConquer => config.dnc_chunk_token_limit,
        CompactionStrategy::FullReplace => {
            return Err(CompactionSampleError::Build(
                "full_replace must be routed through the event-proc compact_conversation helper"
                    .to_string(),
            ));
        }
    };
    let strategy_label = config.compaction_strategy.label();

    info!(
        conversation_id = %conversation_id,
        response_id = %response_id,
        strategy = strategy_label,
        num_turns = turns.len(),
        chunk_token_limit,
        user_compact_threshold = config.user_message_compact_threshold,
        "[InterCompaction] starting chunked compaction"
    );

    // Step 1 — filter.
    let filtered = filter_turns_for_inter_compaction(turns);
    info!(
        conversation_id = %conversation_id,
        start_response_id = %start_response_id,
        last_response_id = %response_id,
        original = turns.len(),
        filtered = filtered.len(),
        "[InterCompaction] filtered turns"
    );
    if filtered.is_empty() {
        return Err(CompactionSampleError::Other(anyhow::anyhow!(
            "No turns remaining after filtering"
        )));
    }

    // Step 2 — split prior `<echo_agent_user_queries>` out of every prior
    // compaction summary item. The LLM never sees them (it would re-emit
    // them verbatim and snowball across rounds); they are reattached to
    // the final summary via `assemble_user_queries_preamble`. Shared with
    // intra-compaction's `History` target.
    let separated = separate_prior_user_queries(&filtered);

    // Step 3 — chunk + flush over the LLM-safe item list.
    let mut compactable: Vec<T> = Vec::new();
    let mut chunk_tokens: u32 = 0;
    let mut chunk_outputs: Vec<LlmCompactionOutput> = Vec::new();
    let mut chunk_idx: usize = 0;

    for turn in &separated.turns_for_llm {
        let turn_tokens = token_counter.count_item_tokens(turn);
        // Flush the current chunk if adding this item would exceed the
        // budget (`UNBOUNDED_CHUNK_LIMIT` disables flushing — Basic).
        if !compactable.is_empty()
            && chunk_token_limit != UNBOUNDED_CHUNK_LIMIT
            && chunk_tokens.saturating_add(turn_tokens) > chunk_token_limit
        {
            let output = flush_chunk(
                &compactable,
                conversation_id,
                response_id,
                chunk_idx,
                config,
                sampler,
                token_counter,
                observer,
            )
            .await?;
            chunk_outputs.push(output);
            chunk_idx += 1;
            compactable.clear();
            chunk_tokens = 0;
        }
        compactable.push(turn.clone());
        chunk_tokens += turn_tokens;
    }

    // Final flush — one chunk for Basic, the trailing chunk for DnC.
    if !compactable.is_empty() {
        let output = flush_chunk(
            &compactable,
            conversation_id,
            response_id,
            chunk_idx,
            config,
            sampler,
            token_counter,
            observer,
        )
        .await?;
        chunk_outputs.push(output);
    }

    if separated.has_prior_compaction {
        observer.on_recompaction(strategy_label);
        info!(
            conversation_id = %conversation_id,
            strategy = strategy_label,
            "[InterCompaction] Re-compaction detected"
        );
    }

    // Step 4a — combine summaries.
    let preamble =
        assemble_user_queries_preamble(separated.prior_user_queries, current_user_queries);
    let mut combined = preamble;
    for (i, output) in chunk_outputs.iter().enumerate() {
        combined.push_str(&format!("<chunk_summary index=\"{}\">\n", i));
        combined.push_str(&output.response);
        combined.push_str("\n</chunk_summary>\n\n");
    }

    // Step 4b — combine thinking-channel output.
    let mut combined_analysis = String::new();
    for (i, output) in chunk_outputs.iter().enumerate() {
        combined_analysis.push_str(&wrap_chunk_analysis(i, &output.thinking));
    }

    // Record chunk count after assembly so dashboards see the same timing
    // they saw pre-unification (where this lived inside DnC).
    observer.on_chunk_count(chunk_outputs.len());

    info!(
        conversation_id = %conversation_id,
        response_id = %response_id,
        strategy = strategy_label,
        num_chunks = chunk_outputs.len(),
        combined_len = combined.len(),
        analysis_len = combined_analysis.len(),
        "[InterCompaction] chunked compaction complete"
    );

    Ok(ChunkedCompactionOutput {
        combined_text: combined,
        analysis_text: combined_analysis,
    })
}

/// Compact a single chunk of items via the LLM.
#[allow(clippy::too_many_arguments)]
async fn flush_chunk<T: CompactionItemBuilder + Send + Sync>(
    turns: &[T],
    conversation_id: &str,
    response_id: &str,
    chunk_idx: usize,
    config: &InterCompactionConfig,
    sampler: &dyn CompactionSampler<Item = T>,
    token_counter: &dyn ItemTokenCounter<T>,
    observer: &dyn InterCompactionObserver,
) -> Result<LlmCompactionOutput, CompactionSampleError> {
    let total_tokens: u32 = turns
        .iter()
        .map(|t| token_counter.count_item_tokens(t))
        .sum();
    info!(
        conversation_id = %conversation_id,
        response_id = %response_id,
        chunk_idx = chunk_idx,
        num_turns = turns.len(),
        total_tokens = total_tokens,
        "[InterCompaction] Compacting chunk"
    );
    let prompt = CompactionPrompt {
        system: format_compaction_developer_prompt().map_err(CompactionSampleError::from)?,
        user: format_compaction_user_prompt().map_err(CompactionSampleError::from)?,
    };
    let timeout = Duration::from_secs(config.sampling_timeout_secs);
    let t0 = Instant::now();
    let result = sampler.sample_compaction(turns, &prompt, timeout).await;
    observer.on_chunk_sampled(result.is_ok(), t0.elapsed());
    info!(
        conversation_id = %conversation_id,
        chunk_idx = chunk_idx,
        elapsed_ms = t0.elapsed().as_millis() as u64,
        success = result.is_ok(),
        "[InterCompaction] Chunk compaction done"
    );
    result
}
