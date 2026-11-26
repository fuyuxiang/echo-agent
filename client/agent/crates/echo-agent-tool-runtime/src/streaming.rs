//! Canonical partial-result streaming contract shared by every streaming tool.
//!
//! A tool declares a [`StreamingSpec`] in its [`ToolCapabilities`] and emits
//! deltas from `execute` via [`stream_chunk`], which materializes the spec into
//! a [`PartialResultPayload`] carried by [`ToolProgress::Custom`]. Downstream
//! layers dispatch on the envelope's `subkind` rather than on the tool's
//! identity.

use echo_agent_tool_protocol::StreamingSpec;
use serde::{Deserialize, Serialize};

use crate::tool::ToolProgress;

/// Per-frame `delta` byte cap used when [`StreamingSpec::max_delta_bytes`] is
/// unset. Guards against a single oversized tick flooding the harness in one
/// frame. Deliberately independent of `ToolCapabilities::max_frame_bytes`,
/// which caps whole frames (16 MiB ceiling), not deltas.
const DEFAULT_MAX_DELTA_BYTES: usize = 16 * 1024;

/// Canonical payload carried by a streaming tool's [`ToolProgress::Custom`].
///
/// Downstream layers dispatch on the envelope's `subkind`. Deltas are
/// append-only and lossless (see [`stream_chunk`]).
///
/// Parsed strictly (`deny_unknown_fields`): an unexpected field is a hard
/// deserialize error rather than being silently ignored, so producer/consumer
/// schema drift (e.g. a stale field from an un-updated producer) is caught and
/// the frame is dropped with a warning instead of misinterpreted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialResultPayload {
    /// Content produced since the previous tick (the delta).
    pub delta: String,

    /// Monotonic total bytes produced so far (NOT the current buffer length).
    pub total_bytes: u64,

    /// Cumulative content was lost upstream and will never be delivered
    /// (distinct from a single-tick `gap`).
    #[serde(default)]
    pub truncated: bool,

    /// This delta has a gap: a single oversized tick overflowed the tail
    /// buffer and its middle was dropped.
    #[serde(default)]
    pub gap: bool,
}

/// Byte count of an incomplete (still-arriving) UTF-8 sequence at the very end
/// of `bytes`, or 0 when the slice ends on a complete sequence or in invalid
/// bytes that can never become valid (those are surfaced lossily instead of
/// held forever).
fn incomplete_utf8_suffix_len(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => 0,
        // `error_len() == None` means the error is an incomplete sequence at
        // the end of the input — the only case worth holding back.
        Err(e) if e.error_len().is_none() => bytes.len() - e.valid_up_to(),
        Err(_) => 0,
    }
}

/// Build at most one [`ToolProgress::Custom`] delta from a monotonic byte
/// source, with UTF-8-safe slicing at both the tick boundary and the per-frame
/// cap.
///
/// `tail` is the source's (possibly truncated) tail buffer — the newest bytes
/// are always at its end. `total` is the monotonic count of bytes produced so
/// far; `last_total` records how much has already been surfaced and is advanced
/// in place. Returns `None` when `total` has not advanced (no new bytes).
///
/// Deltas are **append-only and lossless**: when a delta would end mid-way
/// through a multi-byte UTF-8 sequence, or exceeds the per-frame cap
/// ([`StreamingSpec::max_delta_bytes`], default 16 KiB), the excess bytes are
/// *held back* — `last_total` advances only past the emitted bytes, so the next
/// call re-slices the remainder from the (still-growing) tail. Concatenated
/// deltas are therefore always valid UTF-8 and lossless.
///
/// `truncated` is the caller's cumulative upstream-truncation flag (e.g. a
/// source that hit a hard output cap and will never deliver the elided bytes).
/// It is copied into the payload verbatim and is intentionally distinct from
/// the per-tick `gap` (a single oversized tick overflowed the tail buffer and
/// its middle was dropped upstream). Sources with no cumulative-truncation
/// notion pass `false`.
pub fn stream_chunk(
    spec: &StreamingSpec,
    tail: &[u8],
    total: u64,
    last_total: &mut u64,
    truncated: bool,
) -> Option<ToolProgress> {
    if total <= *last_total {
        return None;
    }
    let new = total - *last_total;
    let tail_len = tail.len() as u64;
    // Deltas are keyed off the monotonic `total`, not the buffer length: when
    // all genuinely-new bytes still fit in the tail we slice its suffix; when a
    // single tick's burst exceeded the buffer the middle was dropped upstream,
    // so we emit what survived plus a `gap` marker.
    let (delta_bytes, gap) = if new <= tail_len {
        (&tail[(tail_len - new) as usize..], false)
    } else {
        (tail, true)
    };

    let cap = spec
        .max_delta_bytes
        .map_or(DEFAULT_MAX_DELTA_BYTES, |c| c as usize);

    // Defer: emit the longest prefix that fits the cap AND ends on a complete
    // UTF-8 sequence; hold the rest back for the next call (the tail still
    // contains it, since `last_total` only advances past the emitted bytes).
    // Nothing is dropped.
    let mut cut = delta_bytes.len().min(cap);
    while cut > 0 && incomplete_utf8_suffix_len(&delta_bytes[..cut]) > 0 {
        cut -= 1;
    }
    // A cap smaller than one multi-byte char would deadlock at cut == 0 while
    // bytes remain; emit the full first char in that pathological case rather
    // than stalling forever.
    if cut == 0 && !delta_bytes.is_empty() {
        cut = delta_bytes.len().min(4);
        while cut < delta_bytes.len() && incomplete_utf8_suffix_len(&delta_bytes[..cut]) > 0 {
            cut += 1;
        }
    }
    if cut == 0 {
        return None;
    }
    let delta = String::from_utf8_lossy(&delta_bytes[..cut]).into_owned();
    let consumed = cut as u64;

    // Advance only past what was emitted (gap case: the upstream-dropped
    // middle counts as consumed — those bytes can never be re-sliced).
    *last_total = if gap {
        total - (delta_bytes.len() as u64 - consumed.min(delta_bytes.len() as u64))
    } else {
        *last_total + consumed
    };

    let payload = PartialResultPayload {
        delta,
        total_bytes: total,
        // The caller's cumulative upstream-loss flag passes through verbatim;
        // the per-tick `gap` is reported separately.
        truncated,
        gap,
    };
    Some(ToolProgress::Custom {
        subkind: spec.subkind.clone(),
        // Infallible: a struct of String/u64/bool/Copy-enums has no map keys
        // or floats that could make `to_value` fail.
        payload: serde_json::to_value(&payload).expect("PartialResultPayload always serializes"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with(max_delta_bytes: Option<u32>) -> StreamingSpec {
        StreamingSpec {
            subkind: "test_chunk".to_owned(),
            max_delta_bytes,
        }
    }

    /// Run `stream_chunk`, assert it produced a frame, and decode the payload.
    fn run(
        spec: &StreamingSpec,
        tail: &[u8],
        total: u64,
        last_total: &mut u64,
        truncated: bool,
    ) -> PartialResultPayload {
        let progress = stream_chunk(spec, tail, total, last_total, truncated)
            .expect("expected a progress frame");
        let ToolProgress::Custom { subkind, payload } = progress else {
            panic!("expected ToolProgress::Custom");
        };
        assert_eq!(subkind, "test_chunk");
        serde_json::from_value(payload).expect("payload decodes")
    }

    #[test]
    fn no_new_bytes_returns_none_and_leaves_last_total() {
        let spec = spec_with(None);
        let mut last = 10;
        assert!(stream_chunk(&spec, b"abc", 10, &mut last, false).is_none());
        assert!(stream_chunk(&spec, b"abc", 5, &mut last, false).is_none());
        assert_eq!(
            last, 10,
            "last_total is untouched when total does not advance"
        );
    }

    #[test]
    fn emits_suffix_delta_and_advances_last_total() {
        let spec = spec_with(None);
        let mut last = 2;
        // total 2 -> 5: 3 genuinely-new bytes, all present in the tail suffix.
        let p = run(&spec, b"abcde", 5, &mut last, false);
        assert_eq!(p.delta, "cde");
        assert_eq!(p.total_bytes, 5);
        assert!(!p.gap);
        assert!(!p.truncated);
        assert_eq!(last, 5, "last_total advanced in place");
    }

    #[test]
    fn gap_set_when_new_exceeds_surviving_tail() {
        // 100 new bytes but only a 4-byte tail survived upstream: the middle
        // was dropped, so the whole tail is emitted with gap = true.
        let spec = spec_with(None);
        let mut last = 0;
        let p = run(&spec, b"tail", 100, &mut last, false);
        assert_eq!(p.delta, "tail");
        assert_eq!(p.total_bytes, 100);
        assert!(p.gap);
        assert!(
            !p.truncated,
            "a per-tick gap must not set cumulative truncated"
        );
        assert_eq!(last, 100, "fully-emitted gap delta consumes the total");
    }

    #[test]
    fn truncated_is_caller_supplied_and_distinct_from_gap() {
        let spec = spec_with(None);
        let mut last = 0;
        // Caller reports cumulative upstream truncation; no per-tick gap here.
        let p = run(&spec, b"abc", 3, &mut last, true);
        assert!(
            p.truncated,
            "caller's cumulative flag passes through verbatim"
        );
        assert!(!p.gap, "no tail overflow this tick");
    }

    #[test]
    fn append_multibyte_split_across_ticks_is_held_back_and_reassembled() {
        // Tick 1 delivers "aé" cut mid-'é' (0xC3 without 0xA9). The lone lead
        // byte is held back, NOT emitted as U+FFFD.
        let spec = spec_with(None);
        let mut last = 0;
        let p = run(&spec, b"a\xC3", 2, &mut last, false);
        assert_eq!(p.delta, "a", "incomplete UTF-8 suffix held back");
        assert_eq!(last, 1, "last_total advances only past emitted bytes");

        // Tick 2: the continuation byte arrives; the held bytes re-slice from
        // the tail and the char comes out whole.
        let p = run(&spec, "aé".as_bytes(), 3, &mut last, false);
        assert_eq!(p.delta, "é", "held bytes reassemble into a whole char");
        assert_eq!(last, 3);
    }

    #[test]
    fn append_over_cap_defers_remainder_to_next_call_without_loss() {
        // Cap 4: a 9-byte burst is paced out over capped frames; nothing is
        // dropped and the concatenation is lossless.
        let spec = spec_with(Some(4));
        let mut last = 0;
        let mut out = String::new();
        while last < 9 {
            let p = run(&spec, b"abcdefghi", 9, &mut last, false);
            assert!(p.delta.len() <= 4, "every frame respects the cap");
            out.push_str(&p.delta);
        }
        assert_eq!(out, "abcdefghi", "deferred remainders are all emitted");
        assert_eq!(last, 9);
    }

    #[test]
    fn append_cap_cut_respects_utf8_boundaries() {
        // 7 ASCII bytes + 'é' (2 bytes) = 9 bytes. A cap of 8 would split the
        // 'é'; the cut backs off and the 'é' is deferred whole.
        let tail = "aaaaaaaé".as_bytes();
        let spec = spec_with(Some(8));
        let mut last = 0;
        let p = run(&spec, tail, tail.len() as u64, &mut last, false);
        assert_eq!(p.delta, "aaaaaaa", "backed off the split multibyte char");
        let p = run(&spec, tail, tail.len() as u64, &mut last, false);
        assert_eq!(p.delta, "é");
    }

    #[test]
    fn payload_decodes_with_optional_flags_absent() {
        // Wire tolerance: a payload missing the bool/count fields still
        // decodes (serde defaults), matching the ToolCapabilities convention.
        let p: PartialResultPayload = serde_json::from_value(serde_json::json!({
            "delta": "x",
            "total_bytes": 1,
        }))
        .expect("payload with absent flags decodes");
        assert!(!p.truncated);
        assert!(!p.gap);
    }

    #[test]
    fn payload_rejects_unknown_field() {
        // Strict (`deny_unknown_fields`): a stale/typo'd field — e.g. a removed
        // `accumulation` from an un-updated producer — is a hard error, not
        // silently ignored, so schema drift never decodes into a partial frame.
        let decoded = serde_json::from_value::<PartialResultPayload>(serde_json::json!({
            "delta": "x",
            "total_bytes": 1,
            "truncated": false,
            "gap": false,
            "accumulation": "append",
        }));
        assert!(
            decoded.is_err(),
            "an unknown field must be rejected under deny_unknown_fields"
        );
    }

    // ── Limit / latency invariants ──────────────────────────────────────────

    /// A backlog drains in exactly `ceil(new / cap)` calls — no extra round-trips.
    #[test]
    fn drains_backlog_in_minimum_ticks() {
        let cap = 4usize;
        let spec = spec_with(Some(cap as u32));
        let data = b"abcdefghij";
        let total = data.len() as u64;
        let mut last = 0;
        let mut ticks = 0usize;
        let mut out = String::new();
        while last < total {
            out.push_str(&run(&spec, data, total, &mut last, false).delta);
            ticks += 1;
            assert!(ticks <= 100, "must terminate");
        }
        assert_eq!(out, "abcdefghij", "lossless");
        assert_eq!(
            ticks,
            data.len().div_ceil(cap),
            "no extra ticks beyond ceil(new / cap)"
        );
    }

    /// ASCII frames fill to the cap — no under-fill, no empty trailing frame.
    #[test]
    fn ascii_frames_fill_to_cap() {
        let spec = spec_with(Some(4));
        let data = b"abcdefgh";
        let mut last = 0;
        assert_eq!(run(&spec, data, 8, &mut last, false).delta, "abcd");
        assert_eq!(run(&spec, data, 8, &mut last, false).delta, "efgh");
        assert!(
            stream_chunk(&spec, data, 8, &mut last, false).is_none(),
            "no spurious empty trailing frame"
        );
    }

    /// A delta exactly at the cap is one frame, no gap, nothing deferred.
    #[test]
    fn exact_cap_emits_single_frame() {
        let cap = 8u64;
        let spec = spec_with(Some(cap as u32));
        let mut last = 0;
        let p = run(&spec, b"abcdefgh", cap, &mut last, false);
        assert_eq!(p.delta, "abcdefgh");
        assert!(!p.gap);
        assert_eq!(last, cap);
        assert!(stream_chunk(&spec, b"abcdefgh", cap, &mut last, false).is_none());
    }

    /// A cap smaller than one char still emits a whole char — never stalls.
    #[test]
    fn tiny_cap_below_char_still_makes_progress() {
        let spec = spec_with(Some(1));
        let mut last = 0;
        let p = run(&spec, "é".as_bytes(), 2, &mut last, false);
        assert_eq!(
            p.delta, "é",
            "emits the whole first char despite cap < charlen"
        );
        assert_eq!(last, 2, "and makes forward progress");
    }

    /// UTF-8 backoff loses at most 3 bytes, so frames stay within 3 of the cap.
    #[test]
    fn utf8_backoff_stays_within_three_bytes_of_cap() {
        let cap = 7usize; // splits a 4-byte char -> backs off to 4 (cap - 3)
        let spec = spec_with(Some(cap as u32));
        let data = "😀😀😀😀".as_bytes();
        let total = data.len() as u64;
        let mut last = 0;
        while last < total {
            let n = run(&spec, data, total, &mut last, false).delta.len();
            assert!(
                n.is_multiple_of(4) && n >= 4,
                "emits whole 4-byte chars, got {n}"
            );
            assert!(
                n >= cap - 3,
                "frame stays within 3 bytes of the cap, got {n}"
            );
            assert!(n <= cap, "frame respects the cap, got {n}");
        }
        assert_eq!(last, total, "drains losslessly");
    }

    /// A gap drains only the surviving tail; never re-scans the dropped middle.
    #[test]
    fn gap_with_cap_paces_surviving_tail_and_terminates() {
        let spec = spec_with(Some(4));
        let tail = b"abcdefgh";
        let total = 1000u64; // only 8 of 1000 bytes survived in the tail
        let mut last = 0;
        let mut ticks = 0usize;
        let mut emitted = 0usize;
        let mut saw_gap = false;
        while last < total {
            let Some(progress) = stream_chunk(&spec, tail, total, &mut last, false) else {
                break;
            };
            let ToolProgress::Custom { payload, .. } = progress else {
                panic!("expected ToolProgress::Custom");
            };
            let p: PartialResultPayload = serde_json::from_value(payload).unwrap();
            saw_gap |= p.gap;
            emitted += p.delta.len();
            ticks += 1;
            assert!(
                ticks <= 4,
                "gap pacing must drain only the surviving tail, not re-scan the dropped middle"
            );
        }
        assert!(saw_gap, "first frame reports the gap");
        assert_eq!(
            emitted,
            tail.len(),
            "emits exactly the surviving tail bytes — no replay of dropped middle"
        );
    }
}
