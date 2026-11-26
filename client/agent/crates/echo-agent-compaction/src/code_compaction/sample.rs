//! The shared bounded-retry summary-sampling loop.
//!
//! The canonical `sample → classify → retry` loop, used by **both** echo-agent-build's
//! full-replace pass ([`sample_full_replace_summary`](super::sample_full_replace_summary))
//! and EchoAgent chat's intra `Shared` summarizer
//! ([`apply_intra_compaction`](crate::intra_compaction::apply_intra_compaction)).
//! Centralising it here removes the two near-identical copies that previously
//! lived in `code_compaction::compact` and `intra_compaction::compact`.
//!
//! Classification is uniform:
//! - a usable, non-degenerate response wins immediately;
//! - empty / degenerate responses ([`is_degenerate_summary`]) are **transient**
//!   and retried until `max_attempts` is hit;
//! - a sampler error is **deterministic** (no retry) when
//!   [`CompactionSampleError::is_deterministic`](crate::CompactionSampleError::is_deterministic)
//!   or a context-length overflow ([`is_context_length_error`]); otherwise it is
//!   transient and retried.
//!
//! The loop is *content-neutral*: callers build the prompt, map the structured
//! [`SampleRetryError`] onto their own error type, and decide whether to clean
//! the winning summary (echo-agent-build cleans in its assembler; intra cleans via
//! [`format_compact_summary`](super::format_compact_summary)). Per-attempt
//! telemetry flows through the [`FullReplaceObserver`] seam; callers without
//! per-attempt metrics (intra) pass `&()`.

use std::time::Duration;

use tracing::warn;

use crate::prompt::CompactionPrompt;
use crate::sampler::CompactionSampler;

use super::failure::is_context_length_error;
use super::observer::{FullReplaceAttemptOutcome, FullReplaceObserver};
use super::summary::is_degenerate_summary;

/// A successful retry-bounded sample: the **raw** winning summary (uncleaned)
/// plus the total number of attempts made (first try + retries).
#[derive(Debug)]
pub struct SampledSummary {
    /// Raw model summary text, exactly as emitted. Callers clean it as needed.
    pub summary: String,
    /// Total sample attempts made (1-based).
    pub attempts: u32,
}

/// Terminal failure of [`sample_summary_with_retries`] after all attempts.
///
/// `attempts` is the number of tries made, for the caller's terminal telemetry.
#[derive(Debug)]
pub enum SampleRetryError {
    /// Every attempt produced an empty or degenerate (too-short) summary.
    Empty {
        /// Total attempts made.
        attempts: u32,
    },
    /// The sampler returned an error: either deterministic (re-sending the same
    /// input cannot help — auth / schema / context overflow), or transient but
    /// retries were exhausted.
    Failure {
        /// Rendered upstream error message.
        message: String,
        /// Whether re-sending the same input cannot help.
        deterministic: bool,
        /// Whether the failure was a context-length overflow (a deterministic
        /// signal the echo-agent-build host uses to step down its input size).
        context_overflow: bool,
        /// Total attempts made.
        attempts: u32,
    },
}

/// Call `sampler.sample_compaction` up to `max_attempts` times, retrying
/// transient failures (empty / degenerate responses and non-deterministic
/// sampler errors) with a `retry_delay` sleep between tries.
///
/// Deterministic sampler errors and context-length overflows short-circuit.
/// Every attempt is reported through `observer`; the returned [`SampledSummary`]
/// / [`SampleRetryError`] both carry the total attempt count.
pub async fn sample_summary_with_retries<T, S, O>(
    sampler: &S,
    turns: &[T],
    prompt: &CompactionPrompt,
    max_attempts: u32,
    retry_delay: Duration,
    timeout: Duration,
    observer: &O,
) -> Result<SampledSummary, SampleRetryError>
where
    T: Send + Sync,
    S: CompactionSampler<Item = T> + ?Sized,
    O: FullReplaceObserver + ?Sized,
{
    let max_attempts = max_attempts.max(1);
    for attempt in 1..=max_attempts {
        let will_retry = attempt < max_attempts;
        match sampler.sample_compaction(turns, prompt, timeout).await {
            Ok(output) if !output.response.trim().is_empty() => {
                // Reject summaries whose cleaned seed is too short;
                // retry like a transient failure.
                if is_degenerate_summary(&output.response) {
                    observer.on_attempt(
                        attempt,
                        &FullReplaceAttemptOutcome::Degenerate {
                            summary: &output.response,
                            will_retry,
                        },
                    );
                    if !will_retry {
                        return Err(SampleRetryError::Empty { attempts: attempt });
                    }
                    warn!(
                        attempt,
                        summary_chars = output.response.len(),
                        "[CompactionSample] degenerate summary, retrying"
                    );
                } else {
                    observer.on_attempt(
                        attempt,
                        &FullReplaceAttemptOutcome::Success {
                            summary: &output.response,
                        },
                    );
                    return Ok(SampledSummary {
                        summary: output.response,
                        attempts: attempt,
                    });
                }
            }
            Ok(_) => {
                // Empty response is transient (sampling variance / mid-stream drop).
                observer.on_attempt(
                    attempt,
                    &FullReplaceAttemptOutcome::EmptyResponse { will_retry },
                );
                if !will_retry {
                    return Err(SampleRetryError::Empty { attempts: attempt });
                }
                warn!(attempt, "[CompactionSample] empty summary, retrying");
            }
            Err(e) => {
                let message = e.to_string();
                let context_overflow = is_context_length_error(&message);
                // A context overflow is deterministic for *this* input — retrying
                // the same payload cannot help.
                let deterministic = e.is_deterministic() || context_overflow;
                let retrying = will_retry && !deterministic;
                observer.on_attempt(
                    attempt,
                    &FullReplaceAttemptOutcome::Failure {
                        message: &message,
                        deterministic,
                        context_overflow,
                        will_retry: retrying,
                    },
                );
                if deterministic {
                    return Err(SampleRetryError::Failure {
                        message,
                        deterministic: true,
                        context_overflow,
                        attempts: attempt,
                    });
                }
                if !will_retry {
                    return Err(SampleRetryError::Failure {
                        message,
                        deterministic: false,
                        context_overflow: false,
                        attempts: attempt,
                    });
                }
                warn!(attempt, error = %message, "[CompactionSample] transient sampler error, retrying");
            }
        }
        tokio::time::sleep(retry_delay).await;
    }
    Err(SampleRetryError::Empty {
        attempts: max_attempts,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::sampler::{CompactionSampleError, LlmCompactionOutput};

    /// Mock sampler with scripted responses (consumed in order).
    struct MockSampler {
        responses: Mutex<Vec<Result<String, CompactionSampleError>>>,
        calls: Mutex<usize>,
    }

    impl MockSampler {
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
        type Item = ();

        async fn sample_compaction(
            &self,
            _turns: &[()],
            _prompt: &CompactionPrompt,
            _timeout: Duration,
        ) -> Result<LlmCompactionOutput, CompactionSampleError> {
            *self.calls.lock().unwrap() += 1;
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err(CompactionSampleError::Other(anyhow::anyhow!("no more")));
            }
            responses.remove(0).map(|response| LlmCompactionOutput {
                response,
                thinking: String::new(),
            })
        }
    }

    /// A non-degenerate summary (cleaned seed >= MIN_SUMMARY_SEED_CHARS).
    fn healthy() -> String {
        format!(
            "Summary:\n1. Primary Request: do the thing\n{}",
            "x".repeat(600)
        )
    }

    fn prompt() -> CompactionPrompt {
        CompactionPrompt {
            system: String::new(),
            user: "summarize".into(),
        }
    }

    async fn run(
        sampler: &MockSampler,
        max_attempts: u32,
    ) -> Result<SampledSummary, SampleRetryError> {
        sample_summary_with_retries(
            sampler,
            &[],
            &prompt(),
            max_attempts,
            Duration::ZERO,
            Duration::from_secs(5),
            &(),
        )
        .await
    }

    #[tokio::test]
    async fn success_first_try_reports_one_attempt() {
        let sampler = MockSampler::scripted(vec![Ok(healthy())]);
        let out = run(&sampler, 3).await.expect("should succeed");
        assert_eq!(out.attempts, 1);
        assert_eq!(sampler.call_count(), 1);
    }

    #[tokio::test]
    async fn transient_error_then_success() {
        let sampler = MockSampler::scripted(vec![
            Err(CompactionSampleError::Timeout {
                timeout_secs: 5,
                collected_bytes: 0,
            }),
            Ok(healthy()),
        ]);
        let out = run(&sampler, 3).await.expect("should succeed after retry");
        assert_eq!(out.attempts, 2);
    }

    #[tokio::test]
    async fn deterministic_error_short_circuits() {
        let sampler = MockSampler::scripted(vec![
            Err(CompactionSampleError::Build("bad model".into())),
            Ok(healthy()),
        ]);
        let err = run(&sampler, 3).await.expect_err("should fail");
        assert!(matches!(
            err,
            SampleRetryError::Failure {
                deterministic: true,
                context_overflow: false,
                attempts: 1,
                ..
            }
        ));
        assert_eq!(sampler.call_count(), 1, "deterministic must not retry");
    }

    #[tokio::test]
    async fn context_overflow_is_deterministic_and_flagged() {
        let sampler =
            MockSampler::scripted(vec![Err(CompactionSampleError::Other(anyhow::anyhow!(
                "API error (status 400): prompt is too long for this model's context window"
            )))]);
        let err = run(&sampler, 3).await.expect_err("should fail");
        assert!(matches!(
            err,
            SampleRetryError::Failure {
                deterministic: true,
                context_overflow: true,
                ..
            }
        ));
        assert_eq!(sampler.call_count(), 1, "overflow must not retry");
    }

    #[tokio::test]
    async fn conversation_exceeds_budget_is_context_overflow() {
        let sampler =
            MockSampler::scripted(vec![Err(CompactionSampleError::Other(anyhow::anyhow!(
                "API error (status 400 Bad Request): invalid-argument: \
                 Failed to start sampling: [conversation] Current message \
                 (1000000 tokens) exceeds budget (500000 tokens)"
            )))]);
        let err = run(&sampler, 3).await.expect_err("should fail");
        assert!(matches!(
            err,
            SampleRetryError::Failure {
                deterministic: true,
                context_overflow: true,
                ..
            }
        ));
        assert_eq!(sampler.call_count(), 1, "overflow must not retry");
    }

    #[tokio::test]
    async fn transient_exhausted_is_non_deterministic_failure() {
        let sampler = MockSampler::scripted(vec![
            Err(CompactionSampleError::Timeout {
                timeout_secs: 5,
                collected_bytes: 0,
            }),
            Err(CompactionSampleError::Timeout {
                timeout_secs: 5,
                collected_bytes: 0,
            }),
        ]);
        let err = run(&sampler, 2).await.expect_err("should fail");
        assert!(matches!(
            err,
            SampleRetryError::Failure {
                deterministic: false,
                attempts: 2,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn empty_then_degenerate_exhausts_to_empty() {
        let short = "<summary>\n1. Primary Request: q\n</summary>"; // degenerate
        let sampler = MockSampler::scripted(vec![Ok(String::new()), Ok(short.into())]);
        let err = run(&sampler, 2).await.expect_err("should fail");
        assert!(matches!(err, SampleRetryError::Empty { attempts: 2 }));
    }
}
