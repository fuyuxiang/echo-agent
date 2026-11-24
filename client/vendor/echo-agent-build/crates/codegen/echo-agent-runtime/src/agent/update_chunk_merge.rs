use agent_client_protocol::{self as acp};
use serde::{Deserialize, Serialize};
use serde_json;

use crate::session::replay_events::SessionNotification;

/// Controls how sampling/output chunks are buffered before being delivered to the client.
/// Parsed from `InitializeRequest.meta.bufferingSettings` and preserved for later use.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BufferingSettings {
    /// Maximum number of items to accumulate before flushing
    #[serde(default = "default_max_items")]
    pub max_items: u64,
    /// Maximum total bytes to accumulate before flushing
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
    /// Maximum time in milliseconds to wait before flushing buffered items
    #[serde(default = "default_max_duration_ms")]
    pub max_duration_ms: u64,
}

fn default_max_items() -> u64 {
    100
}

fn default_max_bytes() -> u64 {
    1024 * 2 // 2 KB
}

fn default_max_duration_ms() -> u64 {
    10 // 10 ms
}

/// Low-level buffer for ACP text chunks (agent message/thought chunks).
///
/// API:
/// - `consume_chunk(...) -> Option<SessionNotification>` returns a notification that should be sent now
///   (typically the previously buffered one), or `None` if we keep buffering.
/// - `flush() -> Option<SessionNotification>` returns any pending buffered notification to send.
///
/// Example of session notification:
/// ```json
/// {
///   "sessionId":"019e0000-0000-7000-8000-000000000001",
///   "update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":".g"}},
///   "_meta":{
///     "totalTokens":100,
///     "eventId":"019e0000-0000-7000-8000-000000000001-0001",
///     "agentTimestampMs":1700000000000,
///     "updateType":"AgentMessageChunk",
///     "updateParams":{"textPreview":".g"}
///   }
/// }
/// ```
/// ```json
/// {
///   "sessionId":"019e0000-0000-7000-8000-000000000001",
///   "update":{
///       "sessionUpdate":"agent_message_chunk",
///       "content":{"type":"text","text":".,"}
///   },
///   "_meta":{
///       "totalTokens":100,
///       "eventId":"019e0000-0000-7000-8000-000000000001-0002",
///       "agentTimestampMs":1700000000000,
///       "updateType":"AgentMessageChunk",
///       "updateParams":{"textPreview":".,"}
///   }
/// }
/// ```
pub(crate) struct ReplayBuffer {
    settings: Option<BufferingSettings>,
    pending: Option<SessionNotification>,
    pending_count: u64,
    pending_bytes: u64,
}

impl ReplayBuffer {
    pub(crate) fn new(settings: Option<BufferingSettings>) -> Self {
        tracing::info!("ReplayBuffer::new: settings = {:?}", settings);
        Self {
            settings,
            pending: None,
            pending_count: 0,
            pending_bytes: 0,
        }
    }

    pub(crate) fn max_wait_duration_ms(&self) -> Option<u64> {
        self.settings.as_ref().map(|s| s.max_duration_ms)
    }

    pub(crate) fn consume_chunk(
        &mut self,
        incoming: impl Into<SessionNotification>,
    ) -> Option<(SessionNotification, Option<SessionNotification>)> {
        let incoming = incoming.into();
        match self.settings.as_ref() {
            Some(settings) => self.consume_chunk_with_settings(
                incoming,
                settings.max_bytes,
                settings.max_items,
                settings.max_duration_ms,
            ),
            None => {
                // Buffering disabled: always send immediately.
                Some((incoming, None))
            }
        }
    }

    fn consume_chunk_with_settings(
        &mut self,
        incoming: SessionNotification,
        max_bytes: u64,
        max_items: u64,
        max_duration_ms: u64,
    ) -> Option<(SessionNotification, Option<SessionNotification>)> {
        let session_id_matches = self
            .pending
            .as_ref()
            .map(|n| n.session_id() == incoming.session_id())
            .unwrap_or(true);

        let incoming_notification_timestamp_in_range = self
            .pending
            .as_ref()
            .map(|prev| incoming.is_in_timestamp_window(prev, max_duration_ms))
            .unwrap_or(true);

        if !session_id_matches {
            // can't merge, we need to send both chunks immediately to preserve current chunk order.
            match self.pending.take() {
                Some(pending) => {
                    // No buffered item after this call.
                    self.pending_count = 0;
                    self.pending_bytes = 0;
                    return Some((pending, Some(incoming)));
                }
                None => {
                    return Some((incoming, None));
                }
            }
        }

        if !incoming_notification_timestamp_in_range {
            // need to pop previously pending notification and send it immediately
            let prev = self.pending.replace(incoming);
            if let Some(prev) = prev {
                return Some((prev, None));
            } else {
                return None;
            }
        }

        let pending = self.pending.take();
        let had_prev = pending.is_some();
        let prev_count = self.pending_count;
        // pending is now empty; we'll either refill it (and set counts) or send immediately.
        self.pending_count = 0;
        self.pending_bytes = 0;

        let (force_send, first, second) = self.merge(pending, incoming);

        match (force_send, first, second) {
            (_, pending, Some(next)) => {
                // Merge wasn't allowed; send both immediately to preserve current chunk order.
                // (Nothing remains buffered.)
                self.pending_count = 0;
                self.pending_bytes = 0;
                Some((pending, Some(next)))
            }
            (force_send, pending, None) => {
                // Decide whether to keep buffering this (possibly merged) pending update.
                let next_count = if had_prev {
                    prev_count.saturating_add(1)
                } else {
                    1
                };
                let next_bytes = estimate_payload_bytes(&pending);

                if force_send || next_count >= max_items || next_bytes >= max_bytes {
                    // Threshold exceeded: send now, leaving buffer empty.
                    self.pending_count = 0;
                    self.pending_bytes = 0;
                    Some((pending, None))
                } else {
                    self.pending = Some(pending);
                    self.pending_count = next_count;
                    self.pending_bytes = next_bytes;
                    None
                }
            }
        }
    }

    /// Two-by-two dispatch on protocol kind:
    /// - Same kind on both sides → delegate to per-kind merge function.
    /// - Different kinds → can't merge, force-flush prev and pass incoming through.
    /// - No prev → buffer the incoming chunk if it's of a bufferable kind,
    ///   force-send otherwise.
    fn merge(
        &mut self,
        prev: Option<SessionNotification>,
        new: SessionNotification,
    ) -> (bool, SessionNotification, Option<SessionNotification>) {
        match (prev, new) {
            (Some(SessionNotification::Acp(prev)), SessionNotification::Acp(new)) => {
                let (force, first, second) = merge_acp_chunks(*new, *prev);
                (
                    force,
                    SessionNotification::Acp(Box::new(first)),
                    second.map(|s| SessionNotification::Acp(Box::new(s))),
                )
            }
            (Some(SessionNotification::EchoAgent(prev)), SessionNotification::EchoAgent(new)) => {
                let (force, first, second) = merge_echo_agent_chunks(*prev, *new);
                (
                    force,
                    SessionNotification::EchoAgent(Box::new(first)),
                    second.map(|s| SessionNotification::EchoAgent(Box::new(s))),
                )
            }
            // Different kinds: can't merge. Force-flush prev, return incoming as second.
            (Some(prev), incoming) => (true, prev, Some(incoming)),
            // No pending: buffer if the new chunk is a streaming kind,
            // force-send otherwise (e.g. ToolCall, Plan, etc.).
            (None, incoming) => {
                let bufferable = incoming.is_streaming_chunk();
                (!bufferable, incoming, None)
            }
        }
    }

    pub(crate) fn flush(&mut self) -> Option<SessionNotification> {
        let out = self.pending.take();
        self.pending_count = 0;
        self.pending_bytes = 0;
        out
    }
}

macro_rules! merge_text_chunks {
    (
        $variant:ident,
        $prev_session_id:expr,
        $prev_meta:expr,
        $prev_text:ident,
        $prev_chunk_meta:ident,
        $new_session_id:expr,
        $new_meta:expr,
        $new_text:ident,
        $new_chunk_meta:ident $(,)?
    ) => {{
        let no_annotations = $prev_text.annotations.is_none() && $new_text.annotations.is_none();
        if no_annotations {
            (
                false,
                acp::SessionNotification::new(
                    $prev_session_id,
                    acp::SessionUpdate::$variant(
                        acp::ContentChunk::new(acp::ContentBlock::Text(
                            acp::TextContent::new(format!("{}{}", $prev_text.text, $new_text.text))
                                .meta($prev_text.meta),
                        ))
                        .meta($prev_chunk_meta),
                    ),
                )
                .meta(merge_meta($prev_meta, $new_meta)),
                None,
            )
        } else {
            (
                true,
                acp::SessionNotification::new(
                    $prev_session_id,
                    acp::SessionUpdate::$variant(
                        acp::ContentChunk::new(acp::ContentBlock::Text(
                            acp::TextContent::new($prev_text.text)
                                .annotations($prev_text.annotations)
                                .meta($prev_text.meta),
                        ))
                        .meta($prev_chunk_meta),
                    ),
                )
                .meta($prev_meta),
                Some(
                    acp::SessionNotification::new(
                        $new_session_id,
                        acp::SessionUpdate::$variant(
                            acp::ContentChunk::new(acp::ContentBlock::Text(
                                acp::TextContent::new($new_text.text)
                                    .annotations($new_text.annotations)
                                    .meta($new_text.meta),
                            ))
                            .meta($new_chunk_meta),
                        ),
                    )
                    .meta($new_meta),
                ),
            )
        }
    }};
}

fn chunk_id_as_u64(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| {
            value
                .as_i64()
                .and_then(|v| if v >= 0 { Some(v as u64) } else { None })
        })
        .or_else(|| value.as_str().and_then(|s| s.parse::<u64>().ok()))
}

fn append_chunk_id_range(range_arr: &mut Vec<serde_json::Value>, new_chunk_id: u64) {
    let Some(last_value) = range_arr.last_mut() else {
        range_arr.push(serde_json::json!(new_chunk_id));
        return;
    };

    if let Some(last_id) = chunk_id_as_u64(last_value) {
        if new_chunk_id == last_id + 1 {
            *last_value = serde_json::json!([last_id, new_chunk_id]);
        } else {
            range_arr.push(serde_json::json!(new_chunk_id));
        }
        return;
    }

    if let Some(range_values) = last_value.as_array_mut() {
        if range_values.len() == 2 {
            if let Some(range_end) = chunk_id_as_u64(&range_values[1])
                && new_chunk_id == range_end + 1
            {
                range_values[1] = serde_json::json!(new_chunk_id);
                return;
            }
        } else if range_values.len() == 1
            && let Some(range_end) = chunk_id_as_u64(&range_values[0])
            && new_chunk_id == range_end + 1
        {
            *last_value = serde_json::json!([range_end, new_chunk_id]);
            return;
        }
    }

    range_arr.push(serde_json::json!(new_chunk_id));
}

fn merge_meta(prev: Option<acp::Meta>, new: Option<acp::Meta>) -> Option<acp::Meta> {
    match (prev, new) {
        (Some(mut prev_val), Some(new_val)) => {
            let (prev_obj, new_obj) = (&mut prev_val, &new_val);
            if let Some(new_chunk_id_value) = new_obj.get("chunkId")
                && let Some(new_chunk_id) = chunk_id_as_u64(new_chunk_id_value)
            {
                if let Some(chunk_id_range) = prev_obj.get_mut("chunkIdRange") {
                    if let Some(range_arr) = chunk_id_range.as_array_mut() {
                        append_chunk_id_range(range_arr, new_chunk_id);
                    }
                } else if let Some(prev_chunk_id_value) = prev_obj.get("chunkId") {
                    if let Some(prev_chunk_id) = chunk_id_as_u64(prev_chunk_id_value) {
                        let mut range_arr = Vec::new();
                        if new_chunk_id == prev_chunk_id + 1 {
                            range_arr.push(serde_json::json!([prev_chunk_id, new_chunk_id]));
                        } else {
                            range_arr.push(serde_json::json!(prev_chunk_id));
                            range_arr.push(serde_json::json!(new_chunk_id));
                        }
                        prev_obj.insert("chunkIdRange".to_string(), range_arr.into());
                        prev_obj.remove("chunkId");
                    } else {
                        prev_obj.insert("chunkId".to_string(), new_chunk_id_value.clone());
                    }
                } else {
                    prev_obj.insert("chunkId".to_string(), new_chunk_id_value.clone());
                }
            }
            Some(prev_val)
        }
        (Some(prev), None) => Some(prev),
        (None, Some(new)) => Some(new),
        (None, None) => None,
    }
}

/// Merge two consecutive ACP notifications.
fn merge_acp_chunks(
    new: acp::SessionNotification,
    prev: acp::SessionNotification,
) -> (
    bool,
    acp::SessionNotification,
    Option<acp::SessionNotification>,
) {
    let private = |notification: &acp::SessionNotification| {
        notification.meta.as_ref()
            .and_then(|meta| meta.get(crate::session::PRIVATE_MEMORY_LOOKUP_META))
            .and_then(serde_json::Value::as_bool) == Some(true)
    };
    if private(&prev) != private(&new) {
        return (true, prev, Some(new));
    }
    let (new_session_id, new_update, new_meta) = (new.session_id, new.update, new.meta);
    let (prev_session_id, prev_update, prev_meta) = (prev.session_id, prev.update, prev.meta);
    match (prev_update, new_update) {
        (
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk {
                content: acp::ContentBlock::Text(prev_text),
                meta: prev_chunk_meta,
                ..
            }),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk {
                content: acp::ContentBlock::Text(incoming_text),
                meta: n_chunk_meta,
                ..
            }),
        ) => merge_text_chunks!(
            AgentMessageChunk,
            prev_session_id,
            prev_meta,
            prev_text,
            prev_chunk_meta,
            new_session_id,
            new_meta,
            incoming_text,
            n_chunk_meta,
        ),
        (
            acp::SessionUpdate::AgentThoughtChunk(acp::ContentChunk {
                content: acp::ContentBlock::Text(prev_text),
                meta: prev_chunk_meta,
                ..
            }),
            acp::SessionUpdate::AgentThoughtChunk(acp::ContentChunk {
                content: acp::ContentBlock::Text(new_text),
                meta: new_chunk_meta,
                ..
            }),
        ) => merge_text_chunks!(
            AgentThoughtChunk,
            prev_session_id,
            prev_meta,
            prev_text,
            prev_chunk_meta,
            new_session_id,
            new_meta,
            new_text,
            new_chunk_meta,
        ),
        (prev, new) => (
            true,
            acp::SessionNotification::new(prev_session_id, prev).meta(prev_meta),
            Some(acp::SessionNotification::new(new_session_id, new).meta(new_meta)),
        ),
    }
}

/// Merge two consecutive EchoAgent notifications.
fn merge_echo_agent_chunks(
    prev: crate::extensions::notification::SessionNotification,
    new: crate::extensions::notification::SessionNotification,
) -> (
    bool,
    crate::extensions::notification::SessionNotification,
    Option<crate::extensions::notification::SessionNotification>,
) {
    use crate::extensions::notification::SessionUpdate as XUpdate;

    if prev.session_id != new.session_id {
        return (true, prev, Some(new));
    }

    let prev_session_id = prev.session_id.clone();
    match (prev.update, new.update) {
        (
            XUpdate::ToolCallDeltaChunk {
                tool_call_id: prev_id,
                tool_index: prev_idx,
                name: prev_name,
                arguments_delta: prev_args,
            },
            XUpdate::ToolCallDeltaChunk {
                tool_call_id: new_id,
                tool_index: new_idx,
                name: new_name,
                arguments_delta: new_args,
            },
        ) if same_tool_call(&prev_id, &new_id, prev_idx, new_idx) => {
            // Merge: concat arguments_delta, prefer earlier id+name.
            let merged_args = match (prev_args, new_args) {
                (Some(mut a), Some(b)) => {
                    a.push_str(&b);
                    Some(a)
                }
                (Some(a), None) => Some(a),
                (None, b) => b,
            };
            let merged = crate::extensions::notification::SessionNotification {
                session_id: prev_session_id,
                update: XUpdate::ToolCallDeltaChunk {
                    tool_call_id: prev_id.or(new_id),
                    tool_index: prev_idx,
                    name: prev_name.or(new_name),
                    arguments_delta: merged_args,
                },
                meta: None,
            };
            (false, merged, None)
        }
        (prev_update, new_update) => {
            let prev_notif = crate::extensions::notification::SessionNotification {
                session_id: prev_session_id.clone(),
                update: prev_update,
                meta: prev.meta,
            };
            let new_notif = crate::extensions::notification::SessionNotification {
                session_id: prev_session_id,
                update: new_update,
                meta: new.meta,
            };
            (true, prev_notif, Some(new_notif))
        }
    }
}

/// Two `ToolCallDeltaChunk`s belong to the same tool call if their ids
/// match (when both present), otherwise if their `tool_index`es match.
/// Continuation chunks omit the id, so the index fallback is what
/// stitches them to the initial id+name chunk.
fn same_tool_call(
    prev_id: &Option<String>,
    new_id: &Option<String>,
    prev_idx: u32,
    new_idx: u32,
) -> bool {
    match (prev_id, new_id) {
        (Some(a), Some(b)) => a == b,
        _ => prev_idx == new_idx,
    }
}

fn estimate_payload_bytes(n: &SessionNotification) -> u64 {
    match n {
        SessionNotification::Acp(n) => match &n.update {
            acp::SessionUpdate::AgentMessageChunk(chunk)
            | acp::SessionUpdate::AgentThoughtChunk(chunk) => match &chunk.content {
                acp::ContentBlock::Text(t) => t.text.len() as u64,
                _ => 0,
            },
            _ => 0,
        },
        SessionNotification::EchoAgent(n) => match &n.update {
            crate::extensions::notification::SessionUpdate::ToolCallDeltaChunk {
                arguments_delta,
                name,
                ..
            } => {
                arguments_delta.as_ref().map(|s| s.len()).unwrap_or(0) as u64
                    + name.as_ref().map(|s| s.len()).unwrap_or(0) as u64
            }
            _ => 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol as acp;
    use serde_json::json;

    fn settings(max_items: u64, max_bytes: u64) -> BufferingSettings {
        BufferingSettings {
            max_items,
            max_bytes,
            max_duration_ms: 50,
        }
    }

    fn msg_chunk(session: &str, agent_ts_ms: u64, text: &str) -> acp::SessionNotification {
        acp::SessionNotification::new(
            acp::SessionId::new(session),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new(text.to_string()),
            ))),
        )
        .meta(
            json!({
                "agentTimestampMs": agent_ts_ms,
            })
            .as_object()
            .cloned(),
        )
    }

    fn pending_text(buf: &ReplayBuffer) -> Option<String> {
        let pending = buf.pending.as_ref()?;
        match pending {
            SessionNotification::Acp(n) => match &n.update {
                acp::SessionUpdate::AgentMessageChunk(chunk)
                | acp::SessionUpdate::AgentThoughtChunk(chunk) => match &chunk.content {
                    acp::ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                },
                _ => None,
            },
            SessionNotification::EchoAgent(_) => None,
        }
    }

    #[test]
    fn buffers_first_chunk_and_updates_count_and_bytes() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));
        let n = msg_chunk("s", 1, "hello");

        let out = buf.consume_chunk(n);
        assert!(out.is_none(), "first chunk should be buffered");
        assert_eq!(buf.pending_count, 1);
        assert_eq!(buf.pending_bytes, 5);
        assert_eq!(pending_text(&buf).as_deref(), Some("hello"));
    }

    #[test]
    fn merges_second_chunk_and_increments_count_and_updates_bytes() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));

        assert!(buf.consume_chunk(msg_chunk("s", 1, "he")).is_none());
        assert_eq!(buf.pending_count, 1);
        assert_eq!(buf.pending_bytes, 2);

        assert!(buf.consume_chunk(msg_chunk("s", 1, "llo")).is_none());
        assert_eq!(buf.pending_count, 2);
        assert_eq!(buf.pending_bytes, 5);
        assert_eq!(pending_text(&buf).as_deref(), Some("hello"));
    }

    #[test]
    fn hits_max_items_threshold_and_flushes_instead_of_buffering() {
        let mut buf = ReplayBuffer::new(Some(settings(2, 1_000_000)));

        assert!(buf.consume_chunk(msg_chunk("s", 1, "he")).is_none());
        let out = buf.consume_chunk(msg_chunk("s", 1, "llo"));

        let Some((first, second)) = out else {
            panic!("expected flush when reaching max_items");
        };
        assert!(second.is_none(), "should flush single merged notification");
        match &first.expect_acp().update {
            acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                acp::ContentBlock::Text(t) => assert_eq!(t.text, "hello"),
                _ => panic!("expected text content"),
            },
            _ => panic!("expected AgentMessageChunk"),
        }
        assert!(buf.pending.is_none());
        assert_eq!(buf.pending_count, 0);
        assert_eq!(buf.pending_bytes, 0);
    }

    #[test]
    fn timestamp_mismatch_flushes_pending_and_does_not_buffer() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));

        assert!(buf.consume_chunk(msg_chunk("s", 1, "hello")).is_none());
        assert_eq!(buf.pending_count, 1);

        let out = buf.consume_chunk(msg_chunk("s", 52, "world"));
        let Some((first, second)) = out else {
            panic!("expected immediate send on timestamp mismatch");
        };
        assert_eq!(
            match &first.expect_acp().update {
                acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                    acp::ContentBlock::Text(t) => t.text.as_str(),
                    _ => "<non-text>",
                },
                _ => "<non-msg>",
            },
            "hello"
        );
        assert!(second.is_none());

        assert!(buf.pending.is_some());
        assert_eq!(buf.pending_count, 1);
        assert_eq!(buf.pending_bytes, 5);

        let rest = buf.flush();
        assert!(rest.is_some());
        assert_eq!(
            rest.as_ref().map(|n| &n.expect_acp().update),
            Some(&acp::SessionUpdate::AgentMessageChunk(
                acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
                    "world".to_string()
                )))
            ))
        );
        assert!(buf.pending.is_none());
        assert_eq!(buf.pending_count, 0);
        assert_eq!(buf.pending_bytes, 0);
    }

    #[test]
    fn merges_chunk_ids_into_range() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));

        let mut n1 = msg_chunk("s", 1, "hello");
        n1.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(1));

        let mut n2 = msg_chunk("s", 1, " world");
        n2.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(2));

        assert!(buf.consume_chunk(n1).is_none());
        assert!(buf.consume_chunk(n2).is_none());

        let out = buf.flush().expect("should have pending");
        let meta = out.into_acp().meta.expect("should have meta");
        let chunk_id_range = meta.get("chunkIdRange").expect("should have chunkIdRange");
        assert_eq!(chunk_id_range, &json!([[1, 2]]));
        assert!(meta.get("chunkId").is_none());
    }

    #[test]
    fn appends_to_existing_chunk_id_range() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));

        let mut n1 = msg_chunk("s", 1, "a");
        n1.meta
            .as_mut()
            .unwrap()
            .insert("chunkIdRange".to_string(), json!([[1, 2]]));

        let mut n2 = msg_chunk("s", 1, "b");
        n2.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(3));

        assert!(buf.consume_chunk(n1).is_none());
        assert!(buf.consume_chunk(n2).is_none());

        let out = buf.flush().expect("should have pending");
        let meta = out.into_acp().meta.expect("should have meta");
        let chunk_id_range = meta.get("chunkIdRange").expect("should have chunkIdRange");
        assert_eq!(chunk_id_range, &json!([[1, 3]]));
    }

    #[test]
    fn squashes_consecutive_chunk_ids_into_single_range() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));

        let mut n1 = msg_chunk("s", 1, "a");
        n1.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(1));

        let mut n2 = msg_chunk("s", 1, "b");
        n2.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(2));

        let mut n3 = msg_chunk("s", 1, "c");
        n3.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(3));

        let mut n4 = msg_chunk("s", 1, "d");
        n4.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(4));

        assert!(buf.consume_chunk(n1).is_none());
        assert!(buf.consume_chunk(n2).is_none());
        assert!(buf.consume_chunk(n3).is_none());
        assert!(buf.consume_chunk(n4).is_none());

        let out = buf.flush().expect("should have pending");
        let meta = out.into_acp().meta.expect("should have meta");
        let chunk_id_range = meta.get("chunkIdRange").expect("should have chunkIdRange");
        assert_eq!(chunk_id_range, &json!([[1, 4]]));
        assert!(meta.get("chunkId").is_none());
    }

    #[test]
    fn keeps_gapped_chunk_ids_as_separate_segments() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));

        let mut n1 = msg_chunk("s", 1, "a");
        n1.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(1));

        let mut n2 = msg_chunk("s", 1, "b");
        n2.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(2));

        let mut n3 = msg_chunk("s", 1, "c");
        n3.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(4));

        assert!(buf.consume_chunk(n1).is_none());
        assert!(buf.consume_chunk(n2).is_none());
        assert!(buf.consume_chunk(n3).is_none());

        let out = buf.flush().expect("should have pending");
        let meta = out.into_acp().meta.expect("should have meta");
        let chunk_id_range = meta.get("chunkIdRange").expect("should have chunkIdRange");
        assert_eq!(chunk_id_range, &json!([[1, 2], 4]));
        assert!(meta.get("chunkId").is_none());
    }

    #[test]
    fn extends_trailing_singleton_when_next_is_consecutive() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));

        let mut n1 = msg_chunk("s", 1, "a");
        n1.meta
            .as_mut()
            .unwrap()
            .insert("chunkIdRange".to_string(), json!([[1, 2], 4]));

        let mut n2 = msg_chunk("s", 1, "b");
        n2.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(5));

        assert!(buf.consume_chunk(n1).is_none());
        assert!(buf.consume_chunk(n2).is_none());

        let out = buf.flush().expect("should have pending");
        let meta = out.into_acp().meta.expect("should have meta");
        let chunk_id_range = meta.get("chunkIdRange").expect("should have chunkIdRange");
        assert_eq!(chunk_id_range, &json!([[1, 2], [4, 5]]));
        assert!(meta.get("chunkId").is_none());
    }

    #[test]
    fn appends_non_consecutive_after_trailing_singleton() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));

        let mut n1 = msg_chunk("s", 1, "a");
        n1.meta
            .as_mut()
            .unwrap()
            .insert("chunkIdRange".to_string(), json!([[1, 2], 4]));

        let mut n2 = msg_chunk("s", 1, "b");
        n2.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(6));

        assert!(buf.consume_chunk(n1).is_none());
        assert!(buf.consume_chunk(n2).is_none());

        let out = buf.flush().expect("should have pending");
        let meta = out.into_acp().meta.expect("should have meta");
        let chunk_id_range = meta.get("chunkIdRange").expect("should have chunkIdRange");
        assert_eq!(chunk_id_range, &json!([[1, 2], 4, 6]));
        assert!(meta.get("chunkId").is_none());
    }

    #[test]
    fn keeps_out_of_order_chunk_ids_in_arrival_order() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));

        let mut n1 = msg_chunk("s", 1, "a");
        n1.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(1));

        let mut n2 = msg_chunk("s", 1, "b");
        n2.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(2));

        let mut n3 = msg_chunk("s", 1, "c");
        n3.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(4));

        let mut n4 = msg_chunk("s", 1, "d");
        n4.meta
            .as_mut()
            .unwrap()
            .insert("chunkId".to_string(), json!(3));

        assert!(buf.consume_chunk(n1).is_none());
        assert!(buf.consume_chunk(n2).is_none());
        assert!(buf.consume_chunk(n3).is_none());
        assert!(buf.consume_chunk(n4).is_none());

        let out = buf.flush().expect("should have pending");
        let meta = out.into_acp().meta.expect("should have meta");
        let chunk_id_range = meta.get("chunkIdRange").expect("should have chunkIdRange");
        assert_eq!(chunk_id_range, &json!([[1, 2], 4, 3]));
        assert!(meta.get("chunkId").is_none());
    }

    #[test]
    fn flush_clears_pending_and_resets_counters() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));

        assert!(buf.consume_chunk(msg_chunk("s", 1, "hello")).is_none());
        assert_eq!(buf.pending_count, 1);
        assert_eq!(buf.pending_bytes, 5);

        let out = buf.flush();
        assert!(out.is_some());
        assert!(buf.pending.is_none());
        assert_eq!(buf.pending_count, 0);
        assert_eq!(buf.pending_bytes, 0);
    }

    #[test]
    fn flush_merges_streaming_chunks() {
        use agent_client_protocol as acp;

        let session_id = acp::SessionId::new("test-session");
        let mut replay_buffer = ReplayBuffer::new(Some(BufferingSettings {
            max_items: 100,
            max_bytes: 1_000_000,
            max_duration_ms: 50,
        }));

        let first = acp::SessionNotification::new(
            session_id.clone(),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("he".to_string()),
            ))),
        )
        .meta(
            serde_json::json!({ "agentTimestampMs": 1000 })
                .as_object()
                .cloned(),
        );
        let second = acp::SessionNotification::new(
            session_id,
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("llo".to_string()),
            ))),
        )
        .meta(
            serde_json::json!({ "agentTimestampMs": 1000 })
                .as_object()
                .cloned(),
        );

        assert!(replay_buffer.consume_chunk(first).is_none());
        assert!(replay_buffer.consume_chunk(second).is_none());

        let flushed = replay_buffer.flush().expect("expected merged notification");
        let text = match &flushed.expect_acp().update {
            acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                acp::ContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            },
            _ => None,
        };
        assert_eq!(text.as_deref(), Some("hello"));
        assert!(replay_buffer.flush().is_none());
    }

    // ── EchoAgent / ToolCallDeltaChunk tests ──────────────────────────────

    fn delta_chunk(
        session: &str,
        tool_call_id: Option<&str>,
        tool_index: u32,
        name: Option<&str>,
        arguments_delta: Option<&str>,
    ) -> crate::extensions::notification::SessionNotification {
        crate::extensions::notification::SessionNotification {
            session_id: acp::SessionId::new(session),
            update: crate::extensions::notification::SessionUpdate::ToolCallDeltaChunk {
                tool_call_id: tool_call_id.map(Into::into),
                tool_index,
                name: name.map(Into::into),
                arguments_delta: arguments_delta.map(Into::into),
            },
            meta: None,
        }
    }

    #[test]
    fn echo_agent_same_id_deltas_merge_args_and_preserve_name() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));
        let init = delta_chunk("s", Some("call_1"), 0, Some("read_file"), None);
        let d1 = delta_chunk("s", None, 0, None, Some("{\"path\":"));
        let d2 = delta_chunk("s", None, 0, None, Some("\"src\"}"));

        assert!(buf.consume_chunk(init).is_none());
        assert!(buf.consume_chunk(d1).is_none());
        assert!(buf.consume_chunk(d2).is_none());

        let flushed = buf.flush().expect("should have pending");
        let n = match flushed {
            SessionNotification::EchoAgent(n) => *n,
            _ => panic!("expected EchoAgent"),
        };
        match n.update {
            crate::extensions::notification::SessionUpdate::ToolCallDeltaChunk {
                tool_call_id,
                name,
                arguments_delta,
                ..
            } => {
                assert_eq!(tool_call_id.as_deref(), Some("call_1"));
                assert_eq!(name.as_deref(), Some("read_file"));
                assert_eq!(arguments_delta.as_deref(), Some("{\"path\":\"src\"}"));
            }
            other => panic!("expected ToolCallDeltaChunk, got {other:?}"),
        }
    }

    #[test]
    fn echo_agent_different_tool_call_id_forces_flush() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));
        let first = delta_chunk("s", Some("call_a"), 0, Some("grep"), Some("a-args"));
        let second = delta_chunk("s", Some("call_b"), 1, Some("read_file"), Some("b-args"));

        assert!(buf.consume_chunk(first).is_none());
        let (flushed, rest) = buf.consume_chunk(second).expect("should force-flush");

        // Both emitted immediately to preserve ordering.
        assert!(matches!(flushed, SessionNotification::EchoAgent(_)));
        assert!(rest.is_some());
        assert!(buf.pending.is_none());
    }

    #[test]
    fn echo_agent_tool_call_delta_is_bufferable() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));
        let chunk = delta_chunk("s", Some("call_1"), 0, Some("bash"), Some("{\"cmd\":"));

        let out = buf.consume_chunk(chunk);
        assert!(out.is_none(), "streaming chunk should be buffered");
        assert!(buf.pending.is_some());
        assert_eq!(buf.pending_count, 1);
    }

    #[test]
    fn echo_agent_after_acp_forces_flush() {
        let mut buf = ReplayBuffer::new(Some(settings(100, 1_000_000)));
        let acp_chunk = msg_chunk("s", 1, "thinking...");
        let echo_agent_chunk = delta_chunk("s", Some("call_1"), 0, Some("bash"), Some("args"));

        assert!(buf.consume_chunk(acp_chunk).is_none());
        let (flushed, rest) = buf
            .consume_chunk(echo_agent_chunk)
            .expect("should force-flush");

        // Both emitted immediately — different kinds can't merge.
        assert!(matches!(flushed, SessionNotification::Acp(_)));
        assert!(matches!(rest, Some(SessionNotification::EchoAgent(_))));
        assert!(buf.pending.is_none());
    }
}
