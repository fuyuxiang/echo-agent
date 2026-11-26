//! echo-agent-build's full-replace compaction pass.
//!
//! echo-agent-build does not select a tail to keep; it summarizes the whole
//! conversation and rebuilds a fresh history from scratch. This module is the
//! transport-agnostic orchestration of that pass:
//!
//! ```text
//! build prompt → sample (retry + classify) → clean → assemble
//! ```
//!
//! Per-harness concerns stay in the product host (for example `echo-agent-runtime`): the triggers, the
//! conversation *gathering / sanitization* that produces `llm_turns`, the
//! verbatim→fitted→lossy input ladder, the live LLM transport (the
//! [`CompactionSampler`] impl), persistence/replay, and the rendering of
//! `system_reminder`. This function takes those as inputs and returns the
//! rebuilt history; it never commits or persists.

use std::time::{Duration, Instant};

use tracing::info;

use crate::item::CompactionItemFactory;
use crate::prompt::CompactionPrompt;
use crate::sampler::CompactionSampler;

use super::assemble::{CompactedHistoryParts, assemble_compacted_history};
use super::config::FullReplaceConfig;
use super::observer::FullReplaceObserver;
use super::prompt::build_summary_prompt;
use super::sample::{SampleRetryError, SampledSummary, sample_summary_with_retries};

/// Everything the assembler needs that the harness extracts from its own
/// state (separate from the conversation that gets summarized).
pub struct FullReplaceContext<T> {
    /// The original system message, carried over verbatim.
    pub system_message: T,
    /// The user-info / project-layout prefix (no `<user_query>` tags).
    pub user_message_prefix: String,
    /// Pre-rendered AGENTS.md block to re-inject, if any.
    pub agents_md_reminder: Option<String>,
    /// The last real user query (raw), kept verbatim post-compaction.
    pub last_user_query: Option<String>,
    /// Working tail retained verbatim (tool/subagent results from the current
    /// turn). echo-agent-build keeps this; pass empty to drop it.
    pub recent_messages: Vec<T>,
    /// Pre-rendered `<system-reminder>` (edited files, running tasks,
    /// subagents, MCP, …). The harness builds this; we only carry it.
    pub system_reminder: Option<String>,
    /// Optional transcript-pointer block appended to the summary.
    pub transcript_hint: Option<String>,
}

/// Outcome of a failed full-replace pass.
#[derive(Debug)]
pub enum FullReplaceError {
    /// No turns were supplied to summarize.
    NothingToCompact,
    /// The model returned no usable summary text after all attempts.
    EmptyResponse,
    /// The sampler failed deterministically (re-sending can't help), or all
    /// transient retries were exhausted.
    Sampler {
        /// The rendered upstream error.
        message: String,
        /// Whether re-sending the *same* input cannot help. The product host
        /// uses this to decide whether to suppress auto-compaction.
        deterministic: bool,
        /// Whether the failure was a context-length overflow. The product host
        /// uses this to step its input ladder (rebuild a smaller input and
        /// call this pass again) instead of suppressing.
        context_overflow: bool,
    },
}

impl std::fmt::Display for FullReplaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingToCompact => write!(f, "nothing to compact"),
            Self::EmptyResponse => write!(f, "compaction model returned an empty summary"),
            Self::Sampler { message, .. } => write!(f, "compaction sampling failed: {message}"),
        }
    }
}

impl std::error::Error for FullReplaceError {}

/// A successful full-replace pass.
pub struct FullReplaceOutput<T> {
    /// The rebuilt, compacted history (`[SP, UP', AGENTS_MD?, UQ_last?,
    /// recent…, summary, reminder?]`).
    pub history: Vec<T>,
    /// The **raw** model summary (pre-clean), so the product host can persist
    /// it (request artifact, compaction segment) exactly as the model emitted
    /// it. The cleaned form is already embedded in `history` by the assembler.
    pub summary: String,
    /// Total sample attempts made (first try + retries).
    pub attempts: u32,
}

/// A successful full-replace **sampling** pass (summary only, no assembly).
///
/// Returned by [`sample_full_replace_summary`] for harnesses (echo-agent-build's
/// shell) that drive the input ladder and assemble the history themselves —
/// they build the assembly inputs (state-context system-reminder, AGENTS.md,
/// plan-mode) *after* the LLM call, so they cannot use the bundled
/// [`apply_full_replace_compaction`].
pub struct FullReplaceSummary {
    /// The **raw** model summary (pre-clean).
    pub summary: String,
    /// Total sample attempts made (first try + retries).
    pub attempts: u32,
}

/// Run echo-agent-build's full-replace compaction pass and return the rebuilt
/// history. Pure orchestration: no triggers, no persistence, no commit.
///
/// - `llm_turns` — the (harness-prepared/sanitized) conversation the model
///   summarizes. Empty ⇒ [`FullReplaceError::NothingToCompact`].
/// - `user_context` — optional `/compact <text>` context spliced into the prompt.
/// - `ctx` — the assembly inputs the harness extracted from its state.
/// - `observer` — per-attempt + terminal telemetry seam (pass `&()` for none).
///
/// The **input ladder** (verbatim → fitted → lossy) stays in the product host: on a
/// context-length overflow this returns
/// [`FullReplaceError::Sampler`] with `context_overflow = true`, and the
/// harness rebuilds a smaller input and calls this pass again.
pub async fn apply_full_replace_compaction<T, S, O>(
    sampler: &S,
    llm_turns: &[T],
    user_context: Option<&str>,
    ctx: FullReplaceContext<T>,
    config: &FullReplaceConfig,
    observer: &O,
) -> Result<FullReplaceOutput<T>, FullReplaceError>
where
    T: CompactionItemFactory + Send + Sync,
    S: CompactionSampler<Item = T> + ?Sized,
    O: FullReplaceObserver + ?Sized,
{
    let FullReplaceSummary { summary, attempts } =
        sample_full_replace_summary(sampler, llm_turns, user_context, config, observer).await?;

    info!(
        turns = llm_turns.len(),
        summary_chars = summary.len(),
        attempts,
        "[FullReplaceCompaction] sampled summary; assembling history"
    );

    // Clean (inside the assembler via `format_compact_summary_content`) and
    // rebuild the compacted history. `compaction_summary` is the raw model
    // output; the assembler strips scratchpad / control tokens.
    let parts = CompactedHistoryParts {
        system_message: ctx.system_message,
        user_message_prefix: ctx.user_message_prefix,
        agents_md_reminder: ctx.agents_md_reminder,
        last_user_query: ctx.last_user_query,
        recent_messages: ctx.recent_messages,
        compaction_summary: summary.clone(),
        system_reminder: ctx.system_reminder,
        transcript_hint: ctx.transcript_hint,
    };
    Ok(FullReplaceOutput {
        history: assemble_compacted_history(parts),
        summary,
        attempts,
    })
}

/// Run only the **sampling** half of the full-replace pass: build the prompt,
/// sample with bounded retries (transient + degenerate), classify failures,
/// and report every attempt through `observer`. Returns the raw summary; the
/// caller assembles the history (and owns the input ladder).
///
/// This is the seam echo-agent-build's shell uses: it drives the verbatim → fitted →
/// lossy input ladder around this call (stepping on a
/// [`FullReplaceError::Sampler`] with `context_overflow = true`) and assembles
/// the compacted history afterward from inputs it gathers post-sampling.
pub async fn sample_full_replace_summary<T, S, O>(
    sampler: &S,
    llm_turns: &[T],
    user_context: Option<&str>,
    config: &FullReplaceConfig,
    observer: &O,
) -> Result<FullReplaceSummary, FullReplaceError>
where
    T: Send + Sync,
    S: CompactionSampler<Item = T> + ?Sized,
    O: FullReplaceObserver + ?Sized,
{
    if llm_turns.is_empty() {
        return Err(FullReplaceError::NothingToCompact);
    }

    let prompt = CompactionPrompt {
        // echo-agent-build appends the summarization prompt as the final user
        // message; there is no separate system prompt for the compaction call.
        system: String::new(),
        user: build_summary_prompt(user_context),
    };
    let timeout = Duration::from_secs(config.sampling_timeout_secs);
    let started = Instant::now();

    match sample_summary_with_retries(
        sampler,
        llm_turns,
        &prompt,
        config.max_attempts,
        Duration::from_secs(config.retry_delay_secs),
        timeout,
        observer,
    )
    .await
    {
        Ok(SampledSummary { summary, attempts }) => {
            observer.on_success(attempts, summary.chars().count(), started.elapsed());
            Ok(FullReplaceSummary { summary, attempts })
        }
        Err(SampleRetryError::Empty { attempts }) => {
            observer.on_error(attempts);
            Err(FullReplaceError::EmptyResponse)
        }
        Err(SampleRetryError::Failure {
            message,
            deterministic,
            context_overflow,
            attempts,
        }) => {
            observer.on_error(attempts);
            Err(FullReplaceError::Sampler {
                message,
                deterministic,
                context_overflow,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::code_compaction::observer::FullReplaceAttemptOutcome;
    use crate::sampler::{CompactionSampleError, LlmCompactionOutput};

    /// Mock item recording which factory constructor produced it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum MockItem {
        System(String),
        User(String),
        UserMeta(String),
        ProjectInstructions(String),
        SystemReminder(String),
        Tail(String),
    }

    impl CompactionItemFactory for MockItem {
        fn new_user(text: String) -> Self {
            Self::User(text)
        }
        fn new_user_meta(text: String) -> Self {
            Self::UserMeta(text)
        }
        fn new_project_instructions(text: String) -> Self {
            Self::ProjectInstructions(text)
        }
        fn new_system_reminder(text: String) -> Self {
            Self::SystemReminder(text)
        }
    }

    /// Mock sampler with scripted responses (consumed in order).
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
        fn scripted(responses: Vec<Result<String, CompactionSampleError>>) -> Self {
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
                    "no more scripted responses"
                )));
            }
            responses.remove(0).map(|response| LlmCompactionOutput {
                response,
                thinking: String::new(),
            })
        }
    }

    fn ctx(recent: Vec<MockItem>) -> FullReplaceContext<MockItem> {
        FullReplaceContext {
            system_message: MockItem::System("you are a helpful assistant".into()),
            user_message_prefix: "<user_info>OS: macos</user_info>".into(),
            agents_md_reminder: Some("# AGENTS.md\nbe nice".into()),
            last_user_query: Some("fix the login bug".into()),
            recent_messages: recent,
            system_reminder: Some(
                "<system-reminder>\n## Running Subagents\n- sub-1\n</system-reminder>".into(),
            ),
            transcript_hint: None,
        }
    }

    fn cfg() -> FullReplaceConfig {
        FullReplaceConfig {
            max_attempts: 3,
            retry_delay_secs: 0,
            sampling_timeout_secs: 5,
        }
    }

    /// A non-degenerate mock summary (cleaned seed >=
    /// [`crate::code_compaction::config::MIN_SUMMARY_SEED_CHARS`]).
    fn healthy_summary(primary: &str) -> String {
        let body = format!(
            "1. Primary Request: {primary}\n\
             2. Key Technical Concepts: Rust, auth, session tokens\n\
             3. Files and Code Sections: crates/foo/src/auth.rs — login handler\n\
             4. Errors and Fixes: None\n\
             5. Problem Solving: traced token validation failure\n\
             6. All User Messages: fix the login bug\n\
             7. Pending Tasks: run integration tests\n\
             8. Current Work: editing auth.rs login handler\n\
             9. Optional Next Step: run tests"
        );
        let padding = "x".repeat(
            crate::code_compaction::config::MIN_SUMMARY_SEED_CHARS.saturating_sub(body.len()),
        );
        format!(
            "<analysis>\nthinking about it\n</analysis>\n\n\
             <summary>\n{body}\n{padding}\n</summary>"
        )
    }

    /// Golden end-to-end test: a realistic conversation + a mock sampler that
    /// returns a structured summary must produce echo-agent-build's exact compacted
    /// history shape, with the LLM output cleaned and the agent-state reminder
    /// carried through as the final item.
    #[tokio::test]
    async fn full_replace_produces_echo_agent_build_history_shape() {
        let llm_turns = vec![
            MockItem::System("you are a helpful assistant".into()),
            MockItem::User("fix the login bug".into()),
            MockItem::Tail("assistant: looked at auth.rs".into()),
        ];
        let recent = vec![MockItem::Tail("tool: read_file(auth.rs) -> ...".into())];
        let sampler = MockSampler::returns(&healthy_summary("fix login bug"));

        let out =
            apply_full_replace_compaction(&sampler, &llm_turns, None, ctx(recent), &cfg(), &())
                .await
                .expect("compaction should succeed")
                .history;

        // [system, prefix, agents_md, last_query, recent_tail, summary, reminder]
        assert_eq!(out.len(), 7, "got: {out:#?}");
        assert_eq!(
            out[0],
            MockItem::System("you are a helpful assistant".into())
        );
        assert_eq!(
            out[1],
            MockItem::UserMeta("<user_info>OS: macos</user_info>".into())
        );
        assert_eq!(
            out[2],
            MockItem::ProjectInstructions("# AGENTS.md\nbe nice".into())
        );
        assert_eq!(
            out[3],
            MockItem::User("<user_query>\nfix the login bug\n</user_query>".into())
        );
        assert_eq!(
            out[4],
            MockItem::Tail("tool: read_file(auth.rs) -> ...".into())
        );

        // Summary carrier: cleaned (no <analysis>/<summary> tags), with preamble.
        let MockItem::UserMeta(summary) = &out[5] else {
            panic!("expected UserMeta summary at [5], got {:?}", out[5]);
        };
        assert!(summary.starts_with("This session is being continued"));
        assert!(summary.contains("Summary:\n1. Primary Request: fix login bug"));
        assert!(
            !summary.contains("<analysis>"),
            "scratchpad leaked: {summary}"
        );
        assert!(!summary.contains("<summary>"), "live tag leaked: {summary}");
        assert!(!summary.contains("thinking about it"));

        // Agent-state reminder carried through verbatim as the final item.
        assert_eq!(
            out[6],
            MockItem::SystemReminder(
                "<system-reminder>\n## Running Subagents\n- sub-1\n</system-reminder>".into()
            )
        );
    }

    #[tokio::test]
    async fn empty_turns_is_nothing_to_compact() {
        let sampler = MockSampler::returns("unused");
        let result =
            apply_full_replace_compaction(&sampler, &[], None, ctx(vec![]), &cfg(), &()).await;
        assert!(matches!(result, Err(FullReplaceError::NothingToCompact)));
        assert_eq!(sampler.call_count(), 0, "must not call the LLM");
    }

    #[tokio::test]
    async fn retries_transient_then_succeeds() {
        let llm_turns = vec![MockItem::User("q".into())];
        let sampler = MockSampler::scripted(vec![
            Err(CompactionSampleError::Timeout {
                timeout_secs: 5,
                collected_bytes: 0,
            }),
            Ok(healthy_summary("q")),
        ]);
        let out =
            apply_full_replace_compaction(&sampler, &llm_turns, None, ctx(vec![]), &cfg(), &())
                .await
                .expect("should succeed after one retry")
                .history;
        assert_eq!(sampler.call_count(), 2);
        assert!(matches!(out.last(), Some(MockItem::SystemReminder(_))));
    }

    #[tokio::test]
    async fn deterministic_failure_does_not_retry() {
        let llm_turns = vec![MockItem::User("q".into())];
        let sampler = MockSampler::scripted(vec![
            Err(CompactionSampleError::Build("bad model".into())),
            Ok("never reached".into()),
        ]);
        let result =
            apply_full_replace_compaction(&sampler, &llm_turns, None, ctx(vec![]), &cfg(), &())
                .await;
        assert!(matches!(
            result,
            Err(FullReplaceError::Sampler {
                deterministic: true,
                context_overflow: false,
                ..
            })
        ));
        assert_eq!(
            sampler.call_count(),
            1,
            "deterministic error must not retry"
        );
    }

    #[tokio::test]
    async fn empty_response_after_retries_errors() {
        let llm_turns = vec![MockItem::User("q".into())];
        let sampler = MockSampler::scripted(vec![Ok("   ".into()), Ok("".into()), Ok("".into())]);
        let result =
            apply_full_replace_compaction(&sampler, &llm_turns, None, ctx(vec![]), &cfg(), &())
                .await;
        assert!(matches!(result, Err(FullReplaceError::EmptyResponse)));
        assert_eq!(sampler.call_count(), 3);
    }

    #[tokio::test]
    async fn degenerate_summary_retries_then_succeeds() {
        let llm_turns = vec![MockItem::User("q".into())];
        let short = "<summary>\n1. Primary Request: q\n</summary>";
        let long = format!(
            "<summary>\n1. Primary Request: fix the login bug\n{}\n</summary>",
            "x".repeat(600)
        );
        let sampler = MockSampler::scripted(vec![Ok(short.into()), Ok(long.clone())]);
        let out =
            apply_full_replace_compaction(&sampler, &llm_turns, None, ctx(vec![]), &cfg(), &())
                .await
                .expect("should succeed after degenerate retry")
                .history;
        assert_eq!(sampler.call_count(), 2);
        let MockItem::UserMeta(summary) = &out[out.len() - 2] else {
            panic!("expected summary carrier");
        };
        assert!(summary.contains("fix the login bug"));
    }

    #[tokio::test]
    async fn degenerate_summary_after_retries_errors() {
        let llm_turns = vec![MockItem::User("q".into())];
        let short = "<summary>\n1. Primary Request: q\n</summary>";
        let sampler =
            MockSampler::scripted(vec![Ok(short.into()), Ok(short.into()), Ok(short.into())]);
        let result =
            apply_full_replace_compaction(&sampler, &llm_turns, None, ctx(vec![]), &cfg(), &())
                .await;
        assert!(matches!(result, Err(FullReplaceError::EmptyResponse)));
        assert_eq!(sampler.call_count(), 3);
    }

    /// A context-length overflow must short-circuit (no retry) and surface
    /// `context_overflow = true` so the product host steps its input ladder.
    #[tokio::test]
    async fn context_overflow_is_terminal_and_flagged() {
        let llm_turns = vec![MockItem::User("q".into())];
        let sampler = MockSampler::scripted(vec![
            Err(CompactionSampleError::Other(anyhow::anyhow!(
                "API error (status 400): The prompt is too long for this model's context window."
            ))),
            Ok(healthy_summary("never reached")),
        ]);
        let result =
            apply_full_replace_compaction(&sampler, &llm_turns, None, ctx(vec![]), &cfg(), &())
                .await;
        assert!(matches!(
            result,
            Err(FullReplaceError::Sampler {
                context_overflow: true,
                deterministic: true,
                ..
            })
        ));
        assert_eq!(sampler.call_count(), 1, "overflow must not retry");
    }

    /// The observer sees one terminal `on_success` and the right per-attempt
    /// outcomes (a degenerate retry then a success).
    #[tokio::test]
    async fn observer_receives_attempt_and_success_callbacks() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct RecordingObserver {
            attempts: Mutex<Vec<String>>,
            successes: Mutex<u32>,
            errors: Mutex<u32>,
        }
        impl FullReplaceObserver for RecordingObserver {
            fn on_attempt(&self, _attempt: u32, outcome: &FullReplaceAttemptOutcome<'_>) {
                let tag = match outcome {
                    FullReplaceAttemptOutcome::Success { .. } => "success",
                    FullReplaceAttemptOutcome::EmptyResponse { .. } => "empty",
                    FullReplaceAttemptOutcome::Degenerate { .. } => "degenerate",
                    FullReplaceAttemptOutcome::Failure { .. } => "failure",
                };
                self.attempts.lock().unwrap().push(tag.to_string());
            }
            fn on_success(&self, _attempts: u32, _summary_chars: usize, _elapsed: Duration) {
                *self.successes.lock().unwrap() += 1;
            }
            fn on_error(&self, _attempts: u32) {
                *self.errors.lock().unwrap() += 1;
            }
        }

        let llm_turns = vec![MockItem::User("q".into())];
        let short = "<summary>\n1. Primary Request: q\n</summary>";
        let sampler = MockSampler::scripted(vec![Ok(short.into()), Ok(healthy_summary("q"))]);
        let observer = RecordingObserver::default();
        let out = apply_full_replace_compaction(
            &sampler,
            &llm_turns,
            None,
            ctx(vec![]),
            &cfg(),
            &observer,
        )
        .await
        .expect("should succeed");
        assert_eq!(out.attempts, 2);
        assert_eq!(
            *observer.attempts.lock().unwrap(),
            vec!["degenerate", "success"]
        );
        assert_eq!(*observer.successes.lock().unwrap(), 1);
        assert_eq!(*observer.errors.lock().unwrap(), 0);
    }
}
