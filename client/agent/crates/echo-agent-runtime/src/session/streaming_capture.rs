//! Out-of-band per-turn capture of the model's streamed reasoning + text.
//!
//! Lives apart from `chat_state`: the model never sees this. It exists purely
//! for trace export (`{session_id}/turn_N/streaming_partial.json`) when the
//! canonical assistant turn never reached `record_assistant_response` — a user
//! cancel mid-stream, a sampler terminal error (e.g. `MaxTokensTruncation`), or
//! a doomloop where every generation returns reasoning-only and the turn errors
//! `reasoning_only`.

use std::fmt::Write;

use crate::session::acp_session::CapturePhase;

/// Hard cap on total bytes (reasoning + text) accumulated across all of a
/// turn's stream segments in a single `StreamingTurnCapture`. Past this we
/// mark `truncated = true` and stop appending so a runaway extended-thinking
/// turn (or a long doomloop) cannot blow memory.
pub(crate) const STREAMING_CAPTURE_MAX_BYTES: usize = 8_000_000;

/// Doom-loop recovery stamp on one generation: what the server reported and
/// what the recovery did about it. Raw trigger labels only — never content.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DoomLoopSegmentStamp {
    /// Raw trigger labels the abort/accept acted on.
    pub(crate) doom_loop_triggers: Vec<String>,
    /// 1-based doom-resample attempt number within the turn.
    pub(crate) attempt: u32,
    /// Chunk index the mid-stream abort fired at; `None` for
    /// terminal-response detections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) aborted_at_chunk: Option<u64>,
    /// `"resampled"` (generation discarded, retried) or
    /// `"accepted_after_budget"` (generation committed with signals).
    pub(crate) action: String,
}

/// One generation within a turn: the reasoning + text the model streamed for a
/// single inference call. A doomloop turn produces several of these (one
/// reasoning-only generation per retry); a normal turn produces one.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct StreamSegment {
    /// Stream-start timestamp (ms epoch) for this generation.
    pub(crate) started_at_ms: Option<i64>,
    /// Reasoning channel text for this generation.
    pub(crate) reasoning_text: String,
    /// Text channel content for this generation.
    pub(crate) response_text: String,
    /// Count of reasoning chunks received.
    pub(crate) reasoning_chunks: u32,
    /// Count of text chunks received.
    pub(crate) text_chunks: u32,
    /// Lifecycle phase this generation was last in.
    pub(crate) phase: CapturePhase,
    /// Doom-loop recovery stamp, when recovery acted on this generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) doom_loop: Option<DoomLoopSegmentStamp>,
}

/// Per-turn snapshot of the model's streamed generations, retained out-of-band
/// from `chat_state`. The in-progress generation lives in the flat fields
/// (`reasoning_text` / `response_text` / `reasoning_chunks` / `text_chunks` /
/// `started_at_ms` / `phase`); finalized prior generations live in `segments`.
///
/// At upload-finalize the flat fields are rebuilt as a joined view of the
/// retained `segments`. This duplicates each retained generation's reasoning
/// (once under `segments[i]`, once joined in the flat `reasoning_text`) — a
/// deliberate back-compat tradeoff bounded by the byte cap: the currently
/// deployed trace viewer reads only the flat fields, so the joined view makes
/// the full doomloop visible without a frontend deploy, while `segments`
/// carries the structured per-attempt breakdown for newer readers.
///
/// Uploaded as `{session_id}/turn_N/streaming_partial.json` by
/// `upload_streaming_partial` whenever a non-completed turn end produces a
/// non-empty capture.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct StreamingTurnCapture {
    /// `promptId` of the turn that produced this capture.
    pub(crate) prompt_id: Option<String>,
    /// Turn number for the trace artifact path.
    pub(crate) turn_number: u64,
    /// Resolved model id reported by the sampler, if available.
    pub(crate) model_id: Option<String>,
    /// Stream-start timestamp (ms epoch); the first retained generation's after
    /// finalize.
    pub(crate) started_at_ms: Option<i64>,
    /// Reasoning channel text. The in-progress generation's during streaming;
    /// the retained generations' joined view after finalize.
    pub(crate) reasoning_text: String,
    /// Text channel content. The in-progress generation's during streaming; the
    /// retained generations' joined view after finalize.
    pub(crate) response_text: String,
    /// Count of reasoning chunks (summed across retained generations after
    /// finalize).
    pub(crate) reasoning_chunks: u32,
    /// Count of text chunks (summed across retained generations after finalize).
    pub(crate) text_chunks: u32,
    /// `true` if the retained reasoning was clipped at
    /// `STREAMING_CAPTURE_MAX_BYTES` (recomputed at finalize from the retained
    /// segments; committed generations are cleared on `Completed` and never
    /// counted, so a clip inside one is not carried).
    pub(crate) truncated: bool,
    /// Why the capture was taken — set when the consumer takes it.
    /// e.g. `"user_cancel"`, `"sampler_error:max_tokens_truncation"`.
    pub(crate) reason: Option<String>,
    /// Which streaming lifecycle phase the model was last in (the last retained
    /// generation's after finalize). See [`CapturePhase`].
    pub(crate) phase: CapturePhase,
    /// Finalized prior generations of this turn (one per inference call). The
    /// in-progress generation lives in the flat fields above until
    /// `start_stream` or `finalize_for_upload` folds it in here. A doomloop turn
    /// ends with several reasoning-only segments; a normal turn keeps none.
    #[serde(default)]
    pub(crate) segments: Vec<StreamSegment>,
    /// Number of model generations (inference calls) observed for this turn —
    /// one per `StreamStarted`, counted independently of how many segments are
    /// retained for upload, so a byte-capped doomloop still reports its true
    /// attempt count. A large value is the doomloop signature.
    #[serde(default)]
    pub(crate) attempt_count: u32,
    /// Reasoning-token count the sampler reported for the TERMINAL empty
    /// response (the last attempt), not the doomloop sum — recorded even when
    /// the reasoning text hit the byte cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_tokens: Option<u32>,
    /// Completion-token count from the terminal empty response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completion_tokens: Option<u32>,
    /// `finish_reason` from the terminal empty response, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) finish_reason: Option<String>,
    /// Sampler empty-response classification, e.g. `reasoning_only`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) empty_reason: Option<String>,
    /// Doom-loop stamp for the in-progress generation; folded into its
    /// [`StreamSegment`] by `push_current_segment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) doom_loop: Option<DoomLoopSegmentStamp>,
}

impl StreamingTurnCapture {
    /// Empty means nothing worth uploading. A terminal reasoning-only empty
    /// response stamps the token magnitude (`reasoning_tokens` / `empty_reason`
    /// / ...) even when no reasoning text was streamed to the shell, so those
    /// fields keep the capture non-empty — otherwise the take gate would drop
    /// the only record of the doomloop's size.
    pub(crate) fn is_empty(&self) -> bool {
        self.reasoning_text.is_empty()
            && self.response_text.is_empty()
            && self.segments.is_empty()
            && self.reasoning_tokens.is_none()
            && self.completion_tokens.is_none()
            && self.finish_reason.is_none()
            && self.empty_reason.is_none()
    }

    /// Reset the capture in-place and stamp the new turn's identifiers. Called
    /// from `StreamStarted` only when the prompt id changes (or from the first
    /// chunk if `StreamStarted` was dropped), so a same-turn restart — a
    /// doomloop's next reasoning-only generation — never wipes the segments
    /// already accumulated for this turn.
    pub(crate) fn begin_turn(&mut self, prompt_id: Option<String>, turn_number: u64) {
        *self = Self {
            prompt_id,
            turn_number,
            ..Self::default()
        };
    }

    /// Open a fresh in-progress generation within the current turn: fold the
    /// previous in-progress slot into `segments` (if it streamed anything),
    /// stamp the new generation's start time, and count the attempt. Same-turn
    /// restarts call this without `begin_turn`, so each generation is retained
    /// and counted separately.
    pub(crate) fn start_stream(&mut self, started_at_ms: i64) {
        self.push_current_segment();
        self.started_at_ms = Some(started_at_ms);
        self.attempt_count += 1;
    }

    /// Fold the in-progress slot (the flat fields) into `segments` and clear it
    /// for the next generation. A slot that streamed nothing is skipped —
    /// unless it carries a doom-loop stamp, which folds text-free (the same
    /// rule as `clear_current_segment`) so a stamp can never linger and
    /// mislabel a later generation. Only uncommitted generations reach this —
    /// a generation that emits `Completed` is discarded via
    /// `clear_current_segment` instead.
    pub(crate) fn push_current_segment(&mut self) {
        if self.reasoning_text.is_empty()
            && self.response_text.is_empty()
            && self.phase != CapturePhase::ToolCall
            && self.doom_loop.is_none()
        {
            return;
        }
        self.segments.push(StreamSegment {
            started_at_ms: self.started_at_ms,
            reasoning_text: std::mem::take(&mut self.reasoning_text),
            response_text: std::mem::take(&mut self.response_text),
            reasoning_chunks: self.reasoning_chunks,
            text_chunks: self.text_chunks,
            phase: self.phase,
            doom_loop: self.doom_loop.take(),
        });
        self.reasoning_chunks = 0;
        self.text_chunks = 0;
        self.phase = CapturePhase::default();
    }

    /// Stamp the in-progress generation with a doom-loop recovery action.
    /// Telemetry only — the stamp rides into this generation's
    /// [`StreamSegment`] when it is folded.
    pub(crate) fn stamp_doom_loop(&mut self, stamp: DoomLoopSegmentStamp) {
        self.doom_loop = Some(stamp);
    }

    /// Whether any retained generation carries a doom-loop stamp.
    pub(crate) fn has_doom_loop_segments(&self) -> bool {
        self.doom_loop.is_some() || self.segments.iter().any(|s| s.doom_loop.is_some())
    }

    /// Discard the in-progress generation without folding it into `segments`.
    /// Called on `Completed`: that generation committed to `afterStateHistory`,
    /// so its reasoning must neither be uploaded nor count against the byte cap
    /// of later generations. A doom-stamped committed generation (a
    /// budget-spent accept) keeps a TEXT-FREE segment so the stamp survives
    /// for traces — the text itself lives in the committed history.
    pub(crate) fn clear_current_segment(&mut self) {
        if let Some(stamp) = self.doom_loop.take() {
            self.segments.push(StreamSegment {
                started_at_ms: self.started_at_ms,
                reasoning_text: String::new(),
                response_text: String::new(),
                reasoning_chunks: self.reasoning_chunks,
                text_chunks: self.text_chunks,
                phase: self.phase,
                doom_loop: Some(stamp),
            });
        }
        self.reasoning_text.clear();
        self.response_text.clear();
        self.reasoning_chunks = 0;
        self.text_chunks = 0;
        self.phase = CapturePhase::default();
        // Recompute `truncated` from what remains: dropping the slot's bytes may
        // bring the turn back under the cap, so `append`'s sticky early-return
        // must not keep suppressing later uncommitted generations because a
        // since-discarded committed generation tripped the cap.
        self.truncated = self.total_bytes() >= STREAMING_CAPTURE_MAX_BYTES;
    }

    /// Total reasoning + text bytes accumulated for the turn so far, across
    /// every finalized segment plus the in-progress slot.
    fn total_bytes(&self) -> usize {
        let segment_bytes: usize = self
            .segments
            .iter()
            .map(|s| s.reasoning_text.len() + s.response_text.len())
            .sum();
        self.reasoning_text.len() + self.response_text.len() + segment_bytes
    }

    /// Append text to the in-progress generation, respecting the total byte cap
    /// across all of the turn's segments; clipped portions set `truncated`.
    pub(crate) fn append(&mut self, channel_is_reasoning: bool, text: &str) {
        // Record the phase before the cap check so it stays accurate even when
        // the bytes themselves are clipped — the model is still in this phase
        // regardless of whether we retained the text.
        self.phase = if channel_is_reasoning {
            CapturePhase::Reasoning
        } else {
            CapturePhase::ResponseText
        };
        // Already capped: skip the O(segments) byte recount on the rest of a
        // runaway turn's chunks.
        if self.truncated {
            return;
        }
        let total = self.total_bytes();
        if total >= STREAMING_CAPTURE_MAX_BYTES {
            self.truncated = true;
            return;
        }
        let remaining = STREAMING_CAPTURE_MAX_BYTES - total;
        let to_append = if text.len() <= remaining {
            text
        } else {
            self.truncated = true;
            // Slice on a char boundary to keep the JSON serialization valid
            // even when the cap lands mid-codepoint.
            let mut cut = remaining;
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            &text[..cut]
        };
        if channel_is_reasoning {
            self.reasoning_text.push_str(to_append);
            self.reasoning_chunks += 1;
        } else {
            self.response_text.push_str(to_append);
            self.text_chunks += 1;
        }
    }

    /// Consolidate the turn for upload by folding the in-progress slot into
    /// `segments`. `segments` only ever holds uncommitted generations (a
    /// committed one is discarded on `Completed`), so every retained generation
    /// — a doomloop retry or a cancel / error mid-stream — is uploaded
    /// regardless of whether it carried reasoning, response text, or a tool
    /// call. The flat back-compat fields are rebuilt from the segments; when
    /// there are none the capture is left empty (no upload).
    pub(crate) fn finalize_for_upload(&mut self) {
        self.push_current_segment();
        // `truncated` reflects only the retained reasoning: committed
        // generations were cleared on `Completed` (never counted), and the
        // in-progress slot was just folded in, so `total_bytes()` is exactly
        // the kept bytes.
        self.truncated = self.total_bytes() >= STREAMING_CAPTURE_MAX_BYTES;
        let mut reasoning = String::new();
        let mut response = String::new();
        let mut reasoning_chunks = 0u32;
        let mut text_chunks = 0u32;
        for (i, seg) in self.segments.iter().enumerate() {
            append_attempt(&mut reasoning, i, &seg.reasoning_text);
            append_attempt(&mut response, i, &seg.response_text);
            reasoning_chunks += seg.reasoning_chunks;
            text_chunks += seg.text_chunks;
        }
        self.reasoning_text = reasoning;
        self.response_text = response;
        self.reasoning_chunks = reasoning_chunks;
        self.text_chunks = text_chunks;
        self.started_at_ms = self.segments.first().and_then(|s| s.started_at_ms);
        self.phase = self.segments.last().map(|s| s.phase).unwrap_or_default();
    }
}

/// Append `text` to `buf` as the `index`-th attempt's slice, inserting a
/// human-readable attempt separator before every non-empty slice after the
/// first. Empty slices contribute nothing (and no separator).
fn append_attempt(buf: &mut String, index: usize, text: &str) {
    if text.is_empty() {
        return;
    }
    if !buf.is_empty() {
        let _ = write!(buf, "\n\n--- attempt {} ---\n\n", index + 1);
    }
    buf.push_str(text);
}

#[cfg(test)]
mod streaming_turn_capture_tests {
    use super::{
        CapturePhase, DoomLoopSegmentStamp, STREAMING_CAPTURE_MAX_BYTES, StreamingTurnCapture,
    };

    /// A doom stamp on a TEXTLESS slot (mid-stream abort before any delta
    /// reached the shell) must still fold on the resample's `start_stream` —
    /// the textless early-return must not let it linger and mislabel or be
    /// overwritten by a later generation.
    #[test]
    fn textless_doom_stamp_folds_instead_of_leaking_to_next_generation() {
        let mut cap = StreamingTurnCapture::default();
        cap.begin_turn(Some("p1".to_owned()), 1);
        cap.start_stream(1);
        cap.stamp_doom_loop(DoomLoopSegmentStamp {
            doom_loop_triggers: vec!["tail_repetition:8@thinking".to_string()],
            attempt: 1,
            aborted_at_chunk: Some(0),
            action: "resampled".to_string(),
        });
        // Resample begins with no text streamed for the doomed attempt.
        cap.start_stream(2);
        cap.append(true, "fresh reasoning");
        cap.push_current_segment();

        assert_eq!(cap.segments.len(), 2, "stamp-only slot folded");
        let stamped = cap.segments[0].doom_loop.as_ref().expect("stamp preserved");
        assert_eq!(stamped.action, "resampled");
        assert_eq!(stamped.attempt, 1);
        assert!(cap.segments[0].reasoning_text.is_empty(), "text-free");
        assert!(
            cap.segments[1].doom_loop.is_none(),
            "the fresh generation is not mislabeled by the lingering stamp"
        );
        assert_eq!(cap.segments[1].reasoning_text, "fresh reasoning");
    }

    /// Doom stamps ride the fold into segments, survive JSON round-trip, and
    /// a stamped committed slot keeps a text-free segment on clear.
    #[test]
    fn doom_loop_stamp_folds_and_round_trips() {
        let mut cap = StreamingTurnCapture::default();
        cap.begin_turn(Some("p1".to_owned()), 1);
        cap.start_stream(1);
        cap.append(true, "loop loop");
        cap.stamp_doom_loop(DoomLoopSegmentStamp {
            doom_loop_triggers: vec!["tail_repetition:8@thinking".to_string()],
            attempt: 1,
            aborted_at_chunk: Some(42),
            action: "resampled".to_string(),
        });
        // Resample folds the doomed slot with its stamp.
        cap.start_stream(2);
        cap.append(true, "fresh");
        cap.stamp_doom_loop(DoomLoopSegmentStamp {
            doom_loop_triggers: vec!["tail_repetition:4@thinking".to_string()],
            attempt: 2,
            aborted_at_chunk: None,
            action: "accepted_after_budget".to_string(),
        });
        // Committed generation: text discarded, stamp retained text-free.
        cap.clear_current_segment();

        assert_eq!(cap.segments.len(), 2);
        assert_eq!(
            cap.segments[0]
                .doom_loop
                .as_ref()
                .map(|s| s.action.as_str()),
            Some("resampled")
        );
        assert_eq!(cap.segments[0].reasoning_text, "loop loop");
        assert_eq!(
            cap.segments[1]
                .doom_loop
                .as_ref()
                .map(|s| s.action.as_str()),
            Some("accepted_after_budget")
        );
        assert!(cap.segments[1].reasoning_text.is_empty());
        assert!(cap.has_doom_loop_segments());

        let json = serde_json::to_string(&cap).unwrap();
        let back: StreamingTurnCapture = serde_json::from_str(&json).unwrap();
        assert_eq!(back.segments[0].doom_loop, cap.segments[0].doom_loop);
        // Unstamped captures serialize without the field at all.
        let plain = serde_json::to_string(&StreamingTurnCapture::default()).unwrap();
        assert!(!plain.contains("doom_loop"));
    }

    #[test]
    fn same_turn_stream_starts_accumulate_segments() {
        // Two `StreamStarted`s for the SAME prompt (a doomloop's two
        // reasoning-only generations) must accumulate as two segments, not wipe
        // each other — the original bug left only the last generation.
        let mut cap = StreamingTurnCapture::default();
        cap.begin_turn(Some("p1".to_owned()), 1);
        cap.start_stream(10);
        cap.append(true, "reasoning attempt one");
        cap.start_stream(20);
        cap.append(true, "reasoning attempt two");
        cap.finalize_for_upload();

        assert_eq!(cap.segments.len(), 2, "both same-turn generations kept");
        assert_eq!(cap.attempt_count, 2);
        assert_eq!(cap.segments[0].reasoning_text, "reasoning attempt one");
        assert_eq!(cap.segments[1].reasoning_text, "reasoning attempt two");
        assert!(cap.reasoning_text.contains("reasoning attempt one"));
        assert!(cap.reasoning_text.contains("reasoning attempt two"));
        assert!(cap.reasoning_text.contains("--- attempt 2 ---"));
        assert_eq!(cap.started_at_ms, Some(10));
    }

    #[test]
    fn finalize_keeps_only_uncommitted_generations() {
        // A committed generation (Completed → slot cleared) never enters
        // segments. Every uncommitted generation is kept regardless of content:
        // reasoning (doomloop), response text (cancel/error mid-answer), or a
        // tool call.
        let mut cap = StreamingTurnCapture::default();
        cap.begin_turn(Some("p1".to_owned()), 1);
        cap.start_stream(1);
        cap.append(true, "committed reasoning");
        cap.append(false, "committed answer");
        cap.clear_current_segment();
        cap.start_stream(2);
        cap.append(true, "doomloop reasoning");
        cap.start_stream(3);
        cap.append(false, "answer cut off");
        cap.start_stream(4);
        cap.append(true, "reasoning then a tool");
        cap.phase = CapturePhase::ToolCall;
        cap.finalize_for_upload();

        assert_eq!(cap.segments.len(), 3, "committed cleared, uncommitted kept");
        assert_eq!(cap.segments[0].reasoning_text, "doomloop reasoning");
        assert_eq!(cap.segments[1].response_text, "answer cut off");
        assert_eq!(cap.segments[2].phase, CapturePhase::ToolCall);
        assert_eq!(cap.attempt_count, 4, "all four generations counted");
        assert!(cap.reasoning_text.contains("doomloop reasoning"));
        assert!(cap.response_text.contains("answer cut off"));
        assert!(!cap.is_empty());
    }

    #[test]
    fn finalize_with_only_committed_generations_is_empty() {
        // A normal turn whose generation committed (Completed → slot cleared)
        // leaves nothing to upload.
        let mut cap = StreamingTurnCapture::default();
        cap.begin_turn(Some("p1".to_owned()), 1);
        cap.start_stream(1);
        cap.append(true, "reasoning");
        cap.append(false, "the answer");
        cap.clear_current_segment();
        cap.finalize_for_upload();

        assert!(cap.segments.is_empty());
        assert!(cap.is_empty());
    }

    #[test]
    fn token_metadata_capture_is_not_empty() {
        // A terminal reasoning-only empty response stamps the token magnitude
        // even when no reasoning text reached the shell. That capture must
        // still upload, so the take gate's `is_empty` check must not discard it.
        let mut cap = StreamingTurnCapture::default();
        cap.begin_turn(Some("p1".to_owned()), 1);
        cap.empty_reason = Some("reasoning_only".to_owned());
        cap.reasoning_tokens = Some(4096);
        cap.finalize_for_upload();

        assert!(cap.segments.is_empty());
        assert!(
            !cap.is_empty(),
            "stamped token magnitude must survive upload"
        );
    }

    #[test]
    fn byte_cap_counts_across_segments() {
        // The cap is enforced over the whole turn, not per generation: a
        // finalized segment's bytes count against a later generation's room.
        let mut cap = StreamingTurnCapture::default();
        cap.begin_turn(Some("p1".to_owned()), 1);
        cap.start_stream(1);
        let big = "a".repeat(STREAMING_CAPTURE_MAX_BYTES * 3 / 4);
        cap.append(true, &big);
        assert!(!cap.truncated, "first generation fits under the cap");
        cap.start_stream(2);
        cap.append(true, &big);
        assert!(
            cap.truncated,
            "the prior segment's bytes must count against the cap"
        );
    }

    #[test]
    fn clear_on_commit_resets_cap_so_later_generation_appends() {
        // A committed generation that trips the cap is discarded on the commit
        // path; clearing it must reset `truncated` so a following uncommitted
        // (doomloop) generation's reasoning is still retained, not suppressed by
        // the sticky early-return in `append`.
        let mut cap = StreamingTurnCapture::default();
        cap.begin_turn(Some("p1".to_owned()), 1);
        cap.start_stream(1);
        cap.append(true, &"a".repeat(STREAMING_CAPTURE_MAX_BYTES + 1));
        assert!(cap.truncated, "the committed generation tripped the cap");
        cap.clear_current_segment();
        assert!(
            !cap.truncated,
            "clearing the committed slot resets the cap flag"
        );

        cap.start_stream(2);
        cap.append(true, "doomloop reasoning after a capped commit");
        cap.finalize_for_upload();

        assert_eq!(cap.segments.len(), 1);
        assert_eq!(
            cap.segments[0].reasoning_text,
            "doomloop reasoning after a capped commit"
        );
        assert!(!cap.truncated);
        assert!(!cap.is_empty());
    }

    #[test]
    fn new_prompt_id_resets_to_single_turn() {
        // A genuinely new turn (different prompt id) wipes the prior turn's
        // segments rather than appending to them.
        let mut cap = StreamingTurnCapture::default();
        cap.begin_turn(Some("p1".to_owned()), 1);
        cap.start_stream(1);
        cap.append(true, "first turn reasoning");
        cap.start_stream(2);
        cap.append(true, "first turn second gen");

        cap.begin_turn(Some("p2".to_owned()), 2);
        cap.start_stream(3);
        cap.append(true, "second turn reasoning");
        cap.finalize_for_upload();

        assert_eq!(cap.prompt_id.as_deref(), Some("p2"));
        assert_eq!(cap.turn_number, 2);
        assert_eq!(cap.segments.len(), 1);
        assert_eq!(cap.attempt_count, 1);
        assert!(cap.reasoning_text.contains("second turn reasoning"));
        assert!(!cap.reasoning_text.contains("first turn"));
    }

    #[test]
    fn token_stamps_survive_finalize() {
        // The terminal-attempt token magnitude stamped by `handle_sampling_failure`
        // must ride through `finalize_for_upload`.
        let mut cap = StreamingTurnCapture::default();
        cap.begin_turn(Some("p1".to_owned()), 1);
        cap.start_stream(1);
        cap.append(true, "doomloop reasoning");
        cap.reasoning_tokens = Some(4096);
        cap.completion_tokens = Some(12);
        cap.finish_reason = Some("stop".to_owned());
        cap.empty_reason = Some("reasoning_only".to_owned());
        cap.finalize_for_upload();

        assert!(!cap.is_empty(), "the uncommitted reasoning segment is kept");
        assert_eq!(cap.reasoning_tokens, Some(4096));
        assert_eq!(cap.completion_tokens, Some(12));
        assert_eq!(cap.finish_reason.as_deref(), Some("stop"));
        assert_eq!(cap.empty_reason.as_deref(), Some("reasoning_only"));
    }
}
