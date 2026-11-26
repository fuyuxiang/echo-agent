//! Strongly-typed notification metadata.
//!
//! Parses the `_meta` JSON from `SessionNotification` into a struct with
//! typed fields.  All fields are `Option` — gracefully degrades when
//! echo-agent-shell hasn't been updated or meta is absent.

use serde::{Deserialize, Serialize};

/// Parsed fields from `SessionNotification._meta`.
///
/// Extracted once in [`acp_handler`](super::super::app::acp_handler) and passed
/// downstream to the tracker and agent state.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct NotificationMeta {
    /// Accumulated token count across the session (`totalTokens`).
    pub total_tokens: Option<u64>,
    /// UTC ms when this notification was sent (`agentTimestampMs`).
    pub agent_timestamp_ms: Option<i64>,
    /// UTC ms when the current LLM streaming response started (`streamStartMs`).
    /// Resets each tool-use loop iteration.
    pub stream_start_ms: Option<i64>,
    /// UTC ms when the current turn started (`turnStartMs`).
    /// Constant for the entire turn.
    pub turn_start_ms: Option<i64>,
    /// Stable id for the prompt this notification belongs to (`promptId`).
    /// The client passes a UUID in `PromptRequest._meta.promptId`; the agent
    /// echoes it on every notification it emits while processing that
    /// prompt. Used to drop chunks for cancelled / rewound turns.
    pub prompt_id: Option<String>,
    /// Whether this notification is historical replay from `session/load`.
    pub is_replay: bool,
    /// Raw `eventId` string (`"{sessionId}-{counter}"`). Tracked per session
    /// as the reconnect cursor (`_meta.cursor` on `session/load`): the agent
    /// resolves it by exact string match against persisted lines, so the full
    /// id is kept — the numeric suffix alone is ambiguous across the
    /// non-monotonic counter runs of a multi-resume history.
    pub event_id: Option<String>,
    /// Monotonic per-process sequence parsed from `eventId`
    /// (`"{sessionId}-{counter}"`, see `echo-agent-runtime util::event_id`). The
    /// agent stamps the SAME `eventId` on the live emission and on the persisted
    /// line that is later replayed, so a client can dedup an event it receives
    /// twice (replay/live overlap, a re-emit after the reconnect gate, or
    /// duplicate routing). Per-session events arrive in increasing order, so the
    /// pager keeps a highwater and drops anything `<=` it. `None` when the agent
    /// didn't stamp an `eventId` (older shell) — such updates always apply.
    pub event_seq: Option<u64>,
}

/// Serializable counterpart of the replay stamp the agent injects on
/// replayed notifications (`_meta.isReplay`, stamped by echo-agent-runtime's
/// `forward_raw_replay_line` during `session/load`).
///
/// [`NotificationMeta::from_json`] is the parse side; this is the build
/// side, so code that constructs a replay-stamped `_meta` (test fixtures,
/// playgrounds) shares the wire key with the parser instead of hand-writing
/// `json!` literals.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayMetaStamp {
    pub is_replay: bool,
}

impl ReplayMetaStamp {
    /// `_meta` value for a replayed (`session/load`) notification.
    pub fn replayed() -> serde_json::Value {
        serde_json::to_value(Self { is_replay: true }).expect("serialize replay meta stamp")
    }
}

/// User-prompt content-block `_meta` keys (`TextContent.meta`), shared by the
/// producers (`dispatch/queue.rs` drain, `effects.rs` prompt send) and the
/// replay consumer (`acp/tracker.rs` `handle_user_message`) so the wire keys
/// cannot drift. Tests keep raw literals — they pin the wire values.
pub mod user_prompt_meta {
    /// Clean display text shown in scrollback instead of the wire text.
    pub const DISPLAY_TEXT: &str = "displayText";
    /// Render the display text as a skill invocation (teal leading token).
    pub const DISPLAY_AS_SKILL: &str = "displayAsSkill";
    /// Render the display text as a scheduled (cron) prompt.
    pub const DISPLAY_AS_CRON: &str = "displayAsCron";
    /// `[[start, end], …]` byte ranges of recognized slash tokens into the
    /// block's `text`; only meaningful when that text is displayed verbatim
    /// (never stamped alongside `displayText`).
    pub const SKILL_TOKEN_RANGES: &str = "skillTokenRanges";
    /// See [`echo_agent_prompt_queue::COMBINED_DISPLAY_TEXTS_META`].
    pub const COMBINED_DISPLAY_TEXTS: &str = echo_agent_prompt_queue::COMBINED_DISPLAY_TEXTS_META;
}

/// `UserMessageChunk` / `ContentChunk._meta` keys stamped by the shell and
/// read by the pager (live and on replay).
pub mod user_message_chunk_meta {
    /// Prompt index for rewind / attribution.
    pub const PROMPT_INDEX: &str = "promptIndex";
    /// When true, the chunk must not become a scrollback user prompt
    /// ([`echo_agent_runtime::session::PromptOrigin::hide_user_echo_from_scrollback`]).
    pub const HIDE_FROM_SCROLLBACK: &str = "hideFromScrollback";
}

/// Extract the numeric counter from an `eventId` (`"{sessionId}-{counter}"`).
/// The counter is the part after the last `-` (session ids themselves contain `-`).
pub fn event_id_counter(event_id: &str) -> Option<u64> {
    event_id
        .rsplit('-')
        .next()
        .and_then(|c| c.parse::<u64>().ok())
}

impl NotificationMeta {
    /// Parse from the `_meta` JSON map on a `SessionNotification`.
    pub fn from_json(meta: Option<&serde_json::Map<String, serde_json::Value>>) -> Self {
        let Some(m) = meta else {
            return Self::default();
        };
        let event_id = m
            .get("eventId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let event_seq = event_id.as_deref().and_then(event_id_counter);
        Self {
            total_tokens: m.get("totalTokens").and_then(|v| v.as_u64()),
            agent_timestamp_ms: m.get("agentTimestampMs").and_then(|v| v.as_i64()),
            stream_start_ms: m.get("streamStartMs").and_then(|v| v.as_i64()),
            turn_start_ms: m.get("turnStartMs").and_then(|v| v.as_i64()),
            prompt_id: m
                .get("promptId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            is_replay: m.get("isReplay").and_then(|v| v.as_bool()).unwrap_or(false),
            event_id,
            event_seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_full_meta() {
        let meta_json = json!({
            "totalTokens": 5000u64,
            "agentTimestampMs": 1700000000000i64,
            "streamStartMs": 1700000000000i64 - 3200,
            "turnStartMs": 1700000000000i64 - 5000,
            "eventId": "sess-1-7",
        });
        let map = meta_json.as_object().unwrap();
        let meta = NotificationMeta::from_json(Some(map));

        assert_eq!(meta.total_tokens, Some(5000));
        assert_eq!(meta.agent_timestamp_ms, Some(1700000000000));
        assert_eq!(meta.stream_start_ms, Some(1700000000000 - 3200));
        assert_eq!(meta.turn_start_ms, Some(1700000000000 - 5000));
        assert!(!meta.is_replay);
        assert_eq!(meta.event_id.as_deref(), Some("sess-1-7"));
        assert_eq!(meta.event_seq, Some(7));
    }

    #[test]
    fn parse_missing_new_fields() {
        // Simulate old echo-agent-shell that doesn't send streamStartMs/turnStartMs
        let meta_json = json!({
            "totalTokens": 1000u64,
            "agentTimestampMs": 1700000000000i64,
        });
        let map = meta_json.as_object().unwrap();
        let meta = NotificationMeta::from_json(Some(map));

        assert_eq!(meta.total_tokens, Some(1000));
        assert_eq!(meta.agent_timestamp_ms, Some(1700000000000));
        assert_eq!(meta.stream_start_ms, None);
        assert_eq!(meta.turn_start_ms, None);
        assert!(!meta.is_replay);
    }

    #[test]
    fn parse_replay_flag() {
        let meta_json = json!({
            "isReplay": true,
        });
        let map = meta_json.as_object().unwrap();
        let meta = NotificationMeta::from_json(Some(map));

        assert!(meta.is_replay);
    }

    /// The build side ([`ReplayMetaStamp::replayed`]) and the parse side
    /// ([`NotificationMeta::from_json`]) must agree on the wire key — a
    /// rename on either side breaks replay detection silently otherwise.
    #[test]
    fn replay_meta_stamp_round_trips_through_parser() {
        let stamp = ReplayMetaStamp::replayed();
        let map = stamp.as_object().unwrap();
        let meta = NotificationMeta::from_json(Some(map));
        assert!(meta.is_replay, "stamped meta must parse as a replay");
    }

    #[test]
    fn parse_event_id_keeps_raw_string_and_seq() {
        let meta_json = json!({
            "eventId": "sess-ab-12-42",
        });
        let map = meta_json.as_object().unwrap();
        let meta = NotificationMeta::from_json(Some(map));

        assert_eq!(meta.event_id.as_deref(), Some("sess-ab-12-42"));
        assert_eq!(meta.event_seq, Some(42));
    }

    /// A non-numeric suffix yields no seq (dedup disabled) but the raw id is
    /// still kept for the reconnect cursor (string-matched agent-side).
    #[test]
    fn parse_event_id_without_numeric_suffix() {
        let meta_json = json!({
            "eventId": "weird-id-zzz",
        });
        let map = meta_json.as_object().unwrap();
        let meta = NotificationMeta::from_json(Some(map));

        assert_eq!(meta.event_id.as_deref(), Some("weird-id-zzz"));
        assert_eq!(meta.event_seq, None);
    }

    #[test]
    fn parse_none_meta() {
        let meta = NotificationMeta::from_json(None);
        assert_eq!(meta.total_tokens, None);
        assert_eq!(meta.agent_timestamp_ms, None);
        assert_eq!(meta.stream_start_ms, None);
        assert_eq!(meta.turn_start_ms, None);
        assert!(!meta.is_replay);
        assert_eq!(meta.event_id, None);
        assert_eq!(meta.event_seq, None);
    }
}
