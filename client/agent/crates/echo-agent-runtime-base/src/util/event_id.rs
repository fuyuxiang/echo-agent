//! Event ID generation for session notifications.
//!
//! Provides a globally unique event ID format `{session_id}-{counter}` that is
//! used for deduplication in the relay. The counter is monotonically increasing
//! across the entire agent process, ensuring event IDs are always comparable.

use std::sync::atomic::{AtomicU64, Ordering};

/// Global counter for event ID generation.
/// Shared across all sessions to ensure monotonically increasing IDs.
static EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generates a unique event ID for correlation across agent/relay/client.
///
/// Format: `{session_id}-{counter}` where counter is a monotonically increasing
/// global counter. This format allows the relay to compare event IDs numerically
/// by extracting the counter suffix.
///
/// # Arguments
/// * `session_id` - The session ID to include in the event ID
///
/// # Returns
/// A unique event ID string in the format `{session_id}-{counter}`
pub fn generate_event_id(session_id: &str) -> String {
    let count = EVENT_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{}-{}", session_id, count)
}

/// Stamp `_meta.eventId` (+ `agentTimestampMs`) onto a notification's meta
/// unless an `eventId` is already present, preserving any other meta fields.
///
/// Every PERSISTED notification should carry an `eventId`: the reconnect
/// cursor (`session/load` `_meta.cursor`) can only bound the replay tail when
/// each persisted line is identifiable, and the same id must ride the live
/// broadcast so clients advance their cursor to ids that exist on disk.
/// Broadcast-only notifications are deliberately left unstamped — a cursor
/// pointing at an id absent from `updates.jsonl` never resolves and forces a
/// full replay on every reconnect.
///
/// Stamping chokepoints (stamp BEFORE the persist/broadcast fork, so both
/// copies share one id): `SessionActor::emit_notification_direct` (all actor
/// ACP notifications, incl. the buffered pipeline), `send_echo_agent_notification` /
/// `persist_echo_agent_update_only` / `handle_echo_agent_session_notification` (actor EchoAgent),
/// `notification_bridge::stamp_event_id` (bridge), `emit_subagent_notification`
/// (subagent), `GoalNotifySender::send_update` (goal mode), plus the inline
/// `build_notification_meta` user-echo persists. An emitter outside these is
/// not a correctness bug — `prepare_replay_lines` refuses cursors over id-less
/// tails (full replay, safe) — but it silently disables incremental reconnect
/// for affected sessions.
pub fn ensure_event_id_meta(
    session_id: &str,
    meta: &mut Option<serde_json::Map<String, serde_json::Value>>,
) {
    if meta
        .as_ref()
        .and_then(|m| m.get("eventId"))
        .is_some_and(|v| !v.is_null())
    {
        return;
    }
    let event_id = generate_event_id(session_id);
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let obj = meta.get_or_insert_with(serde_json::Map::new);
    obj.insert("eventId".into(), event_id.into());
    obj.entry("agentTimestampMs")
        .or_insert_with(|| timestamp_ms.into());
}

/// Raise the global event counter so the next generated id is at least `next`.
///
/// The counter is process-global and starts at 0 on every launch, but the
/// monotonic-`eventId` invariant the client dedup relies on
/// (`acp::meta::NotificationMeta::event_seq`) spans a *session's whole history*,
/// not a single process. On `--resume` (or any reload into a fresh process) the
/// replayed transcript carries the ORIGINAL process's high counters; without
/// re-seeding, this process would mint LOWER ids for new live events and the
/// client would dedup-drop every one of them (frozen token counter, missing
/// turns). Call this once on session load with `persisted_max + 1`.
///
/// Uses `fetch_max`, so it only ever raises the counter — safe to call from
/// multiple concurrently-loading sessions sharing the process-global counter.
pub fn ensure_event_counter_at_least(next: u64) {
    EVENT_COUNTER.fetch_max(next, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_event_id_format() {
        let id = generate_event_id("test-session-123");
        assert!(id.starts_with("test-session-123-"));
        // Should end with a valid number
        let _counter: u64 = id.rsplit('-').next().unwrap().parse().unwrap();
    }

    #[test]
    fn ensure_event_counter_at_least_only_raises() {
        // Re-seeding to a high floor makes the next id continue past it — this
        // is what keeps `--resume` from minting ids below the replayed maximum.
        // Uses a very high floor so concurrent tests (which only ever raise the
        // shared counter via fetch_add/fetch_max) cannot push it back down.
        ensure_event_counter_at_least(5_000_000);
        let counter1: u64 = generate_event_id("sess")
            .rsplit('-')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            counter1 >= 5_000_000,
            "next id must be at/above the seeded floor, got {counter1}"
        );

        // A lower floor is a no-op (fetch_max never decreases the counter).
        ensure_event_counter_at_least(1);
        let counter2: u64 = generate_event_id("sess")
            .rsplit('-')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            counter2 > counter1,
            "a lower floor must not reset the counter: {counter2} !> {counter1}"
        );
    }

    #[test]
    fn ensure_event_id_meta_stamps_none_and_merges_existing() {
        // None meta: a fresh object with eventId + timestamp is created.
        let mut meta = None;
        ensure_event_id_meta("sess-x", &mut meta);
        let obj = meta.as_ref().unwrap();
        assert!(
            obj["eventId"]
                .as_str()
                .is_some_and(|id| id.starts_with("sess-x-"))
        );
        assert!(obj["agentTimestampMs"].is_i64());

        // Existing meta without eventId: fields are merged, not replaced.
        let mut meta = serde_json::json!({ "custom": true }).as_object().cloned();
        ensure_event_id_meta("sess-x", &mut meta);
        let obj = meta.as_ref().unwrap();
        assert_eq!(obj["custom"], serde_json::json!(true));
        assert!(obj.contains_key("eventId"));
    }

    #[test]
    fn ensure_event_id_meta_keeps_existing_id() {
        // An already-stamped id (e.g. emit site stamped before the persist
        // chokepoint re-checks) must survive so the persisted line matches
        // the live broadcast copy.
        let mut meta = serde_json::json!({ "eventId": "sess-x-42" })
            .as_object()
            .cloned();
        ensure_event_id_meta("sess-x", &mut meta);
        assert_eq!(
            meta.as_ref().and_then(|m| m.get("eventId")),
            Some(&serde_json::json!("sess-x-42"))
        );
    }

    #[test]
    fn test_generate_event_id_incrementing() {
        let id1 = generate_event_id("session-a");
        let id2 = generate_event_id("session-b");
        let id3 = generate_event_id("session-a");

        let counter1: u64 = id1.rsplit('-').next().unwrap().parse().unwrap();
        let counter2: u64 = id2.rsplit('-').next().unwrap().parse().unwrap();
        let counter3: u64 = id3.rsplit('-').next().unwrap().parse().unwrap();

        // Counters should be monotonically increasing
        assert!(counter2 > counter1);
        assert!(counter3 > counter2);
    }
}
