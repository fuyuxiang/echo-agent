//! Per-request transport for server-reported doom-loop signals.
//!
//! The wire shapes and tolerant parsers live in
//! [`echo_agent_sampling_types::doom_loop`]; this module only moves the parsed
//! signals across the layer boundary: the Layer-1 SSE decoder in
//! [`crate::client`] records them as raw payloads arrive, and the Layer-2
//! transform in [`crate::stream::responses`] drains them into the final
//! `ConversationResponse`.

use std::sync::{Arc, Mutex};

use echo_agent_sampling_types::doom_loop::{
    DOOM_LOOP_CHECK_EVENT_TYPE, DoomLoopPeek, DoomLoopRecoveryPolicy, DoomLoopSignal,
    peek_doom_loop,
};

/// Cheap-to-clone accumulator shared between the SSE decode closure and the
/// stream transform of one request attempt. Created fresh per attempt so
/// signals from a failed attempt can never leak into the next one. Carries
/// the policy so the stream transform can judge confidence for the
/// mid-stream abort; the retry loop disarms the abort once the recovery
/// budget is spent so the final attempt completes and can be accepted.
#[derive(Clone, Debug, Default)]
pub struct DoomLoopSignalCollector {
    inner: Arc<Mutex<CollectorState>>,
}

#[derive(Debug, Default)]
struct CollectorState {
    signals: Vec<DoomLoopSignal>,
    malformed_logged: bool,
    policy: DoomLoopRecoveryPolicy,
    // Inverted so `derive(Default)` starts attempts armed.
    abort_disarmed: bool,
}

impl DoomLoopSignalCollector {
    /// A fresh, armed collector judging confidence with `policy`.
    pub(crate) fn new(policy: DoomLoopRecoveryPolicy) -> Self {
        let collector = Self::default();
        if let Ok(mut state) = collector.inner.lock() {
            state.policy = policy;
        }
        collector
    }

    /// Stop the mid-stream abort for this attempt; signals keep recording.
    pub(crate) fn disarm_abort(&self) {
        if let Ok(mut state) = self.inner.lock() {
            state.abort_disarmed = true;
        }
    }

    /// While armed: the raw labels of the confident signals recorded so far
    /// (non-draining), or `None` when there is nothing to act on.
    pub(crate) fn abort_triggers(&self) -> Option<Vec<String>> {
        let state = self.inner.lock().ok()?;
        if state.abort_disarmed {
            return None;
        }
        let confident = state.policy.confident_triggers(&state.signals);
        (!confident.is_empty()).then_some(confident)
    }

    /// Inspect a raw SSE frame. Returns `true` when the frame is the
    /// non-standard `response.doom_loop_check` event — by its SSE `event:`
    /// name or its payload `type` — which the caller must swallow;
    /// forwarding it would fail typed deserialization. Reported triggers
    /// (mid-stream or on the terminal response object) are recorded,
    /// deduplicated by raw label. Never fails.
    pub(crate) fn absorb(&self, event_name: &str, data: &str) -> bool {
        // The name check keeps a check event with an unparseable payload
        // from ever reaching the typed parser.
        let named = event_name == DOOM_LOOP_CHECK_EVENT_TYPE;
        let (signals, swallow) = match peek_doom_loop(data) {
            DoomLoopPeek::CheckEvent(signals) => (signals, true),
            DoomLoopPeek::ResponseField(signals) => (signals, false),
            DoomLoopPeek::None => {
                if named {
                    self.log_malformed_once();
                }
                return named;
            }
        };
        if signals.is_empty() {
            self.log_malformed_once();
        } else {
            self.record(signals);
        }
        swallow || named
    }

    /// Drain the recorded signals; empty when nothing was reported.
    pub(crate) fn take(&self) -> Vec<DoomLoopSignal> {
        match self.inner.lock() {
            Ok(mut state) => std::mem::take(&mut state.signals),
            Err(_) => Vec::new(),
        }
    }

    fn record(&self, signals: Vec<DoomLoopSignal>) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        // Cumulative sets are re-sent as they grow; the raw label is the
        // stable identity. Linear scan is fine for these tiny sets.
        for signal in signals {
            if !state.signals.iter().any(|s| s.raw == signal.raw) {
                state.signals.push(signal);
            }
        }
    }

    /// Debug-log the first malformed payload per attempt (never per event).
    fn log_malformed_once(&self) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        if !state.malformed_logged {
            state.malformed_logged = true;
            tracing::debug!("doom-loop check payload malformed or empty; ignoring");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_sampling_types::doom_loop::{
        DoomLoopSignalKind, SAMPLE_CHECK_EVENT_DATA, SAMPLE_CHECK_EVENT_DATA_CUMULATIVE,
    };

    #[test]
    fn absorb_swallows_check_event_and_records_signals() {
        let collector = DoomLoopSignalCollector::default();
        assert!(collector.absorb(DOOM_LOOP_CHECK_EVENT_TYPE, SAMPLE_CHECK_EVENT_DATA));
        let signals = collector.take();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].kind, DoomLoopSignalKind::TailRepetition(4));
    }

    /// Servers that omit the SSE `event:` name are still handled by the
    /// payload `type` check.
    #[test]
    fn absorb_swallows_check_event_without_sse_name() {
        let collector = DoomLoopSignalCollector::default();
        assert!(collector.absorb("message", SAMPLE_CHECK_EVENT_DATA));
        assert_eq!(collector.take().len(), 1);
    }

    #[test]
    fn absorb_dedupes_cumulative_sets_by_raw_label() {
        let collector = DoomLoopSignalCollector::default();
        assert!(collector.absorb(DOOM_LOOP_CHECK_EVENT_TYPE, SAMPLE_CHECK_EVENT_DATA));
        assert!(collector.absorb(
            DOOM_LOOP_CHECK_EVENT_TYPE,
            SAMPLE_CHECK_EVENT_DATA_CUMULATIVE
        ));
        let signals = collector.take();
        assert_eq!(signals.len(), 2);
        assert_eq!(signals[0].raw, "tail_repetition:4@response");
        assert_eq!(signals[1].raw, "tail_repetition:2@response");
    }

    #[test]
    fn absorb_forwards_ordinary_and_terminal_payloads() {
        let collector = DoomLoopSignalCollector::default();
        let delta = r#"{"type":"response.output_text.delta","delta":"hi"}"#;
        assert!(!collector.absorb("response.output_text.delta", delta));
        assert!(collector.take().is_empty());
        // Terminal response field is recorded but the event is forwarded.
        let terminal = r#"{"type":"response.completed","response":{"id":"r1","doom_loop_check":{"triggers":["low_logprob@response"]}}}"#;
        assert!(!collector.absorb("response.completed", terminal));
        assert_eq!(collector.take().len(), 1);
    }

    #[test]
    fn malformed_check_event_swallowed_without_signals() {
        let collector = DoomLoopSignalCollector::default();
        let wrong_type =
            r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":"nope"}}"#;
        assert!(collector.absorb(DOOM_LOOP_CHECK_EVENT_TYPE, wrong_type));
        assert!(collector.absorb("message", r#"{"type":"response.doom_loop_check"}"#));
        assert!(collector.take().is_empty());
    }

    /// A frame with the check event's SSE name but an unparseable payload
    /// (non-JSON, or JSON without the `type` tag) must still be swallowed —
    /// forwarding it would fail the typed parse and the whole attempt.
    #[test]
    fn named_event_with_garbage_payload_still_swallowed() {
        let collector = DoomLoopSignalCollector::default();
        assert!(collector.absorb(DOOM_LOOP_CHECK_EVENT_TYPE, "not json at all"));
        assert!(collector.absorb(DOOM_LOOP_CHECK_EVENT_TYPE, r#"{"no_type_tag":true}"#));
        assert!(collector.take().is_empty());
    }

    #[test]
    fn take_drains_once() {
        let collector = DoomLoopSignalCollector::default();
        collector.absorb(DOOM_LOOP_CHECK_EVENT_TYPE, SAMPLE_CHECK_EVENT_DATA);
        assert!(!collector.take().is_empty());
        assert!(collector.take().is_empty());
    }

    /// `abort_triggers` fires only on confident signals, does not drain, and
    /// goes quiet once disarmed (the spent-budget attempt must complete).
    #[test]
    fn abort_triggers_requires_confidence_and_honors_disarm() {
        let confident = r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":["tail_repetition:8@thinking"]}}"#;

        let collector = DoomLoopSignalCollector::new(DoomLoopRecoveryPolicy::default());
        // Non-confident channel: recorded but not actionable.
        assert!(collector.absorb(DOOM_LOOP_CHECK_EVENT_TYPE, SAMPLE_CHECK_EVENT_DATA));
        assert!(collector.abort_triggers().is_none());

        assert!(collector.absorb(DOOM_LOOP_CHECK_EVENT_TYPE, confident));
        assert_eq!(
            collector.abort_triggers(),
            Some(vec!["tail_repetition:8@thinking".to_string()])
        );
        // Non-draining: probing twice and taking afterwards both work.
        assert!(collector.abort_triggers().is_some());

        collector.disarm_abort();
        assert!(collector.abort_triggers().is_none());
        assert_eq!(collector.take().len(), 2, "recording survives the disarm");
    }
}
