//! ACP extension handler for bulk session updates (`echo.agent/session/updates`).
//!
//! Returns session updates in a single response with rewind dead branches
//! filtered out. Supports optional pagination (`offset`/`limit`) for large
//! sessions.
//!
//! ## Usage
//!
//! ```json
//! // Full session (default)
//! { "sessionId": "...", "cwd": "/path" }
//! → { "updates": [...], "totalCount": 3000, "hasMore": false }
//!
//! // Paginated
//! { "sessionId": "...", "cwd": "/path", "offset": 0, "limit": 500 }
//! → { "updates": [...], "totalCount": 3000, "hasMore": true }
//!
//! // Tail (most recent N updates)
//! { "sessionId": "...", "cwd": "/path", "offset": -100 }
//! → { "updates": [...], "totalCount": 3000, "hasMore": false }
//!
//! // Tail by user-message turns
//! { "sessionId": "...", "cwd": "/path", "turnIndex": 2 }
//! → { "updates": [...], "totalCount": 3000, "hasMore": true, "promptStarts": [...] }
//! ```
//!
//! Negative `offset` counts from the end: `-100` means "last 100 updates".
//! `turnIndex` slices by user-message turn boundaries instead of raw count.
//!
//! Each element in the `updates` array is the full JSONL storage envelope
//! (with `timestamp`, `method`, and `params` wrapper), not just the inner
//! notification params. Clients should parse the `method` field to determine
//! the update type (`"session/update"` for ACP, `"_echo.agent/session/update"` for
//! EchoAgent extensions) and extract the notification payload from `params`.
//!
//! Metadata columns and cross-host import live in [`crate::extensions::session_state`].

use std::io::{self, BufRead, BufReader};
use std::path::Path;

use agent_client_protocol as acp;

use super::ExtResult;
use crate::session::wire_tags::{REWIND_MARKER, USER_MESSAGE_CHUNK_PREFIX};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    session_id: String,
    cwd: String,
    /// Negative offset counts from end (e.g. `-100` = last 100).
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    limit: Option<usize>,
    /// Deliver updates as chunked notifications instead of a JSON array.
    #[serde(default)]
    stream: bool,
    /// Updates per chunk notification (default 64). Only used with `stream`.
    #[serde(default)]
    chunk_size: Option<usize>,
    /// Tail by user-message-turn count instead of raw update count.
    #[serde(default)]
    turn_index: Option<usize>,
    /// Routing metadata — forwarded to chunk notifications for leader/relay.
    #[serde(default, rename = "_meta")]
    meta: Option<crate::extensions::routing::RequestMeta>,
}

const DEFAULT_CHUNK_SIZE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PageBounds {
    start: usize,
    end: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct TailPage {
    lines: Vec<String>,
    total_count: usize,
    has_more: bool,
    prompt_starts: Vec<usize>,
}

fn page_bounds(request: &Request, total_count: usize) -> PageBounds {
    let start = match request.offset {
        Some(off) if off < 0 => (total_count as i64 + off).max(0) as usize,
        Some(off) => (off as usize).min(total_count),
        None => 0,
    };
    let end = match request.limit {
        Some(lim) => (start + lim).min(total_count),
        None => total_count,
    };
    PageBounds { start, end }
}

/// Check if a raw JSONL line is a `user_message_chunk` ACP update.
/// Uses a fast substring match on the serialized JSON instead of a full
/// deserialization.  The pattern `"sessionUpdate":"user_message_chunk"` is
/// emitted deterministically by serde and cannot appear in user content
/// without being escaped, so false positives are not possible.
fn is_user_message_chunk(line: &str) -> bool {
    line.contains(&*USER_MESSAGE_CHUNK_PREFIX)
}

/// Scan lines and return the index of the first update in each user-message turn.
fn compute_prompt_starts<T: AsRef<str>>(lines: &[T]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut in_user = false;
    for (i, line) in lines.iter().enumerate() {
        let line = line.as_ref();
        let is_user = is_user_message_chunk(line);
        if is_user && !in_user {
            starts.push(i);
        }
        in_user = is_user;
    }
    starts
}

fn try_stream_tail_page(request: &Request, updates_path: &Path) -> io::Result<Option<TailPage>> {
    let is_negative_offset = request.offset.is_some_and(|o| o < 0);
    let is_turn_index = request.turn_index.filter(|&n| n > 0).is_some() && request.offset.is_none();

    if !is_negative_offset && !is_turn_index {
        return Ok(None);
    }

    let file = std::fs::File::open(updates_path)?;
    let reader = BufReader::new(file);

    if is_turn_index {
        // Single-pass scan: read lines, detect rewinds, and compute prompt
        // boundaries in one pass to avoid a second traversal.
        let mut has_rewinds = false;
        let mut all_lines = Vec::new();
        let mut prompt_starts = Vec::new();
        let mut in_user = false;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            has_rewinds |= line.contains(&*REWIND_MARKER);
            let is_user = is_user_message_chunk(&line);
            if is_user && !in_user {
                prompt_starts.push(all_lines.len());
            }
            in_user = is_user;
            all_lines.push(line);
        }

        // If rewinds are present, filter dead branches and recompute
        // prompt boundaries over the live lines.
        let (all_lines, prompt_starts) = if has_rewinds {
            let refs: Vec<&str> = all_lines.iter().map(|s| s.as_str()).collect();
            let live = crate::session::storage::filter_rewind_lines(refs);
            let owned: Vec<String> = live.into_iter().map(|s| s.to_owned()).collect();
            let ps = compute_prompt_starts(&owned);
            (owned, ps)
        } else {
            (all_lines, prompt_starts)
        };
        let total_count = all_lines.len();

        let tail_n = request.turn_index.unwrap();
        let start = if tail_n >= prompt_starts.len() {
            0
        } else {
            prompt_starts[prompt_starts.len() - tail_n]
        };
        let end = match request.limit {
            Some(lim) => (start + lim).min(total_count),
            None => total_count,
        };
        let has_more = start > 0;
        let lines = all_lines[start..end].to_vec();
        Ok(Some(TailPage {
            lines,
            total_count,
            has_more,
            prompt_starts,
        }))
    } else {
        // Negative-offset path: ring buffer, only keep last N lines in memory.
        let tail_n = (-request.offset.unwrap()) as usize;
        let mut total_count = 0usize;
        let mut has_rewinds = false;
        let mut tail = std::collections::VecDeque::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            has_rewinds |= line.contains(&*REWIND_MARKER);
            total_count += 1;
            tail.push_back(line);
            if tail.len() > tail_n {
                tail.pop_front();
            }
        }

        if has_rewinds {
            return Ok(None);
        }

        let bounds = page_bounds(request, total_count);
        let retained_start = total_count.saturating_sub(tail.len());
        let local_start = bounds.start.saturating_sub(retained_start);
        let page_len = bounds.end.saturating_sub(bounds.start);
        let lines: Vec<String> = tail.into_iter().skip(local_start).take(page_len).collect();
        Ok(Some(TailPage {
            lines,
            total_count,
            has_more: bounds.end < total_count,
            prompt_starts: vec![],
        }))
    }
}

fn response_from_page<T: AsRef<str>>(
    page: &[T],
    total_count: usize,
    has_more: bool,
    prompt_starts: &[usize],
) -> ExtResult {
    let last_event_id = extract_last_event_id(page);

    let page_len = page.len();
    let data_len: usize = page.iter().map(|l| l.as_ref().len()).sum();
    let commas = page_len.saturating_sub(1);
    let mut buf = String::with_capacity(data_len + commas + 120);

    buf.push_str(r#"{"updates":["#);
    for (i, line) in page.iter().enumerate() {
        if i > 0 {
            buf.push(',');
        }
        buf.push_str(line.as_ref());
    }
    buf.push_str(r#"],"totalCount":"#);
    buf.push_str(&total_count.to_string());
    buf.push_str(r#","hasMore":"#);
    buf.push_str(if has_more { "true" } else { "false" });
    if let Some(ref eid) = last_event_id {
        buf.push_str(r#","lastEventId":"#);
        buf.push_str(&serde_json::to_string(eid).unwrap_or_default());
    }
    append_prompt_starts(&mut buf, prompt_starts);
    buf.push('}');

    serde_json::value::RawValue::from_string(buf)
        .map(|raw| acp::ExtResponse::new(std::sync::Arc::from(raw)))
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

/// Extract `_meta.eventId` from the last line that has one (reverse scan).
fn extract_last_event_id<T: AsRef<str>>(lines: &[T]) -> Option<String> {
    use crate::session::storage::{RawLinePeek, RawParamsPeek};

    for line in lines.iter().rev() {
        let line = line.as_ref();
        if !line.contains("eventId") {
            continue;
        }
        let Ok(env) = serde_json::from_str::<RawLinePeek<'_>>(line) else {
            continue;
        };
        let Some(raw_params) = env.params.map(|p| p.get()) else {
            continue;
        };
        let Ok(pp) = serde_json::from_str::<RawParamsPeek<'_>>(raw_params) else {
            continue;
        };
        let Some(meta_raw) = pp.meta else { continue };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_raw.get()) else {
            continue;
        };
        if let Some(eid) = meta.get("eventId").and_then(|v| v.as_str()) {
            return Some(eid.to_string());
        }
    }
    None
}

/// Send updates as chunked `_echo.agent/session/updates/chunk` notifications.
/// Injects routing metadata when `target_client_id` is set.
fn send_streamed_chunks<T: AsRef<str>>(
    gateway: &echo_agent_acp::AcpAgentGatewaySender,
    session_id: &str,
    lines: &[T],
    chunk_size: usize,
    target_client_id: &crate::extensions::routing::TargetClientId,
) {
    use crate::extensions::routing::inject_routing_meta;

    let total_chunks = lines.len().div_ceil(chunk_size);

    for (chunk_idx, chunk) in lines.chunks(chunk_size).enumerate() {
        let done = chunk_idx + 1 == total_chunks;

        let updates: Vec<&serde_json::value::RawValue> = chunk
            .iter()
            .filter_map(|l| serde_json::from_str::<&serde_json::value::RawValue>(l.as_ref()).ok())
            .collect();

        let mut params = serde_json::json!({
            "sessionId": session_id,
            "index": chunk_idx,
            "updates": updates,
            "done": done,
        });

        inject_routing_meta(&mut params, target_client_id);

        if let Ok(raw) = serde_json::value::to_raw_value(&params) {
            gateway.forward_fire_and_forget(acp::ExtNotification::new(
                "echo.agent/session/updates/chunk",
                std::sync::Arc::from(raw),
            ));
        }
    }
}

/// Inline sync I/O (`spawn_blocking` is slow on `current_thread` + `LocalSet`).
pub async fn handle(
    args: &acp::ExtRequest,
    gateway: &echo_agent_acp::AcpAgentGatewaySender,
) -> ExtResult {
    let _timer = crate::instrumentation_timer!("session.ext.bulk_updates");

    let request: Request = serde_json::from_str(args.params.get())
        .map_err(|e| acp::Error::invalid_params().data(e.to_string()))?;

    let target_client_id = request
        .meta
        .as_ref()
        .map(|m| m.client_id.clone())
        .unwrap_or_default();

    let session_info = crate::session::info::Info {
        id: acp::SessionId::new(request.session_id.clone()),
        cwd: request.cwd.clone(),
    };
    let mut updates_path = crate::session::persistence::session_dir(&session_info)
        .join(crate::session::storage::UPDATES_FILE);

    // Subagents persist under their own cwd (may differ from the parent cwd
    // passed here), so fall back to an id scan when the (id, cwd) path misses.
    if !updates_path.exists()
        && let Some(found_dir) =
            crate::session::persistence::find_persisted_session_dir_by_id_result(
                &request.session_id,
            )
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?
    {
        let candidate = found_dir.join(crate::session::storage::UPDATES_FILE);
        if candidate.exists() {
            updates_path = candidate;
        }
    }

    if !updates_path.exists() {
        if request.stream {
            return streamed_metadata_response(0, None, 0, &[]);
        }
        return empty_response(0);
    }

    if let Some(tail_page) = try_stream_tail_page(&request, &updates_path)
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))?
    {
        if request.stream {
            let chunk_size = request.chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE).max(1);
            let last_event_id = extract_last_event_id(&tail_page.lines);
            let chunk_count = tail_page.lines.len().div_ceil(chunk_size);
            send_streamed_chunks(
                gateway,
                &request.session_id,
                &tail_page.lines,
                chunk_size,
                &target_client_id,
            );
            return streamed_metadata_response(
                tail_page.total_count,
                last_event_id.as_deref(),
                chunk_count,
                &tail_page.prompt_starts,
            );
        }
        return response_from_page(
            &tail_page.lines,
            tail_page.total_count,
            tail_page.has_more,
            &tail_page.prompt_starts,
        );
    }

    let raw_contents = std::fs::read_to_string(&updates_path)
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;

    let lines: Vec<&str> = raw_contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let live_lines = crate::session::storage::filter_rewind_lines(lines);
    let total_count = live_lines.len();
    let prompt_starts = compute_prompt_starts(&live_lines);

    // turn_index with rewinds: resolve start from prompt boundaries
    let (bounds, has_more) = if let Some(tail_n) = request
        .turn_index
        .filter(|&n| n > 0)
        .filter(|_| request.offset.is_none())
    {
        let start = if tail_n >= prompt_starts.len() {
            0
        } else {
            prompt_starts[prompt_starts.len() - tail_n]
        };
        let end = match request.limit {
            Some(lim) => (start + lim).min(total_count),
            None => total_count,
        };
        (PageBounds { start, end }, start > 0)
    } else {
        let b = page_bounds(&request, total_count);
        (b, b.end < total_count)
    };
    let page = &live_lines[bounds.start..bounds.end];

    if request.stream {
        let chunk_size = request.chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE).max(1);
        let last_event_id = extract_last_event_id(page);
        let chunk_count = page.len().div_ceil(chunk_size);
        send_streamed_chunks(
            gateway,
            &request.session_id,
            page,
            chunk_size,
            &target_client_id,
        );
        return streamed_metadata_response(
            total_count,
            last_event_id.as_deref(),
            chunk_count,
            &prompt_starts,
        );
    }

    response_from_page(page, total_count, has_more, &prompt_starts)
}

fn append_prompt_starts(buf: &mut String, prompt_starts: &[usize]) {
    buf.push_str(r#","promptStarts":["#);
    for (i, idx) in prompt_starts.iter().enumerate() {
        if i > 0 {
            buf.push(',');
        }
        buf.push_str(&idx.to_string());
    }
    buf.push(']');
}

fn streamed_metadata_response(
    total_count: usize,
    last_event_id: Option<&str>,
    chunk_count: usize,
    prompt_starts: &[usize],
) -> ExtResult {
    let mut json = format!(r#"{{"totalCount":{total_count},"chunkCount":{chunk_count}"#,);
    if let Some(eid) = last_event_id {
        json.push_str(r#","lastEventId":"#);
        json.push_str(&serde_json::to_string(eid).unwrap_or_default());
    }
    append_prompt_starts(&mut json, prompt_starts);
    json.push('}');
    serde_json::value::RawValue::from_string(json)
        .map(|raw| acp::ExtResponse::new(std::sync::Arc::from(raw)))
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

fn empty_response(total_count: usize) -> ExtResult {
    let json =
        format!(r#"{{"updates":[],"totalCount":{total_count},"hasMore":false,"promptStarts":[]}}"#);
    let raw = serde_json::value::RawValue::from_string(json)
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
    Ok(acp::ExtResponse::new(std::sync::Arc::from(raw)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_response(response: acp::ExtResponse) -> serde_json::Value {
        serde_json::from_str(response.0.get()).unwrap()
    }

    fn dummy_gateway() -> echo_agent_acp::AcpAgentGatewaySender {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        echo_agent_acp::AcpAgentGatewaySender::new(tx)
    }

    fn make_request(
        session_id: &str,
        cwd: &str,
        offset: Option<i64>,
        limit: Option<usize>,
    ) -> acp::ExtRequest {
        let mut map = serde_json::Map::new();
        map.insert(
            "sessionId".to_string(),
            serde_json::Value::String(session_id.to_string()),
        );
        map.insert(
            "cwd".to_string(),
            serde_json::Value::String(cwd.to_string()),
        );
        if let Some(offset) = offset {
            map.insert("offset".to_string(), serde_json::json!(offset));
        }
        if let Some(limit) = limit {
            map.insert("limit".to_string(), serde_json::json!(limit));
        }

        let raw = serde_json::value::to_raw_value(&serde_json::Value::Object(map)).unwrap();
        acp::ExtRequest::new("echo.agent/session/updates", std::sync::Arc::from(raw))
    }

    #[tokio::test]
    async fn handle_tail_request_matches_expected_tail_window() {
        let cwd_tmp = tempfile::TempDir::new().unwrap();
        let cwd = cwd_tmp.path().to_string_lossy().to_string();
        let session_id = "tail-equivalence";
        let session_info = crate::session::info::Info {
            id: acp::SessionId::new(session_id),
            cwd: cwd.clone(),
        };
        let session_dir = crate::session::persistence::session_dir(&session_info);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("updates.jsonl"),
            [
                r#"{"timestamp":1,"method":"session/update","params":{"seq":1}}"#,
                r#"{"timestamp":2,"method":"session/update","params":{"seq":2}}"#,
                r#"{"timestamp":3,"method":"session/update","params":{"seq":3}}"#,
                r#"{"timestamp":4,"method":"session/update","params":{"seq":4}}"#,
                r#"{"timestamp":5,"method":"session/update","params":{"seq":5}}"#,
                r#"{"timestamp":6,"method":"session/update","params":{"seq":6}}"#,
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();

        let gw = dummy_gateway();
        let response = handle(&make_request(session_id, &cwd, Some(-3), Some(2)), &gw)
            .await
            .unwrap();
        let json = parse_response(response);

        assert_eq!(json["totalCount"], 6);
        assert_eq!(json["hasMore"], true);
        assert_eq!(json["updates"].as_array().unwrap().len(), 2);
        assert_eq!(json["updates"][0]["params"]["seq"], 4);
        assert_eq!(json["updates"][1]["params"]["seq"], 5);
    }

    #[tokio::test]
    async fn handle_tail_request_with_rewind_returns_only_live_timeline() {
        let cwd_tmp = tempfile::TempDir::new().unwrap();
        let cwd = cwd_tmp.path().to_string_lossy().to_string();
        let session_id = "tail-rewind-filter";
        let session_info = crate::session::info::Info {
            id: acp::SessionId::new(session_id),
            cwd: cwd.clone(),
        };
        let session_dir = crate::session::persistence::session_dir(&session_info);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("updates.jsonl"),
            [
                r#"{"timestamp":1,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"first"}}}}"#,
                r#"{"timestamp":2,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"resp1"}}}}"#,
                r#"{"timestamp":3,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"dead-branch"}}}}"#,
                r#"{"timestamp":4,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"dead-resp"}}}}"#,
                r#"{"timestamp":5,"method":"_echo.agent/session/update","params":{"sessionId":"s","update":{"sessionUpdate":"rewind_marker","target_prompt_index":1}}}"#,
                r#"{"timestamp":6,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"replacement"}}}}"#,
                r#"{"timestamp":7,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"replacement-resp"}}}}"#,
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();

        let gw = dummy_gateway();
        let response = handle(&make_request(session_id, &cwd, Some(-3), None), &gw)
            .await
            .unwrap();
        let json = parse_response(response);
        let updates = json["updates"].as_array().unwrap();
        let rendered = serde_json::to_string(updates).unwrap();

        assert_eq!(json["totalCount"], 4);
        assert_eq!(updates.len(), 3);
        assert!(!rendered.contains("dead-branch"));
        assert!(!rendered.contains("dead-resp"));
        assert!(rendered.contains("replacement"));
        assert!(rendered.contains("replacement-resp"));
    }

    /// Divergent-cwd subagents must resolve by id, not the caller's parent cwd.
    #[tokio::test]
    async fn handle_falls_back_to_id_lookup_for_divergent_cwd() {
        // Transcript lives under the subagent's own cwd …
        let child_cwd_tmp = tempfile::TempDir::new().unwrap();
        let child_cwd = child_cwd_tmp.path().to_string_lossy().to_string();
        // … but the caller only knows the parent cwd.
        let parent_cwd_tmp = tempfile::TempDir::new().unwrap();
        let parent_cwd = parent_cwd_tmp.path().to_string_lossy().to_string();

        let session_id = "divergent-cwd-fallback-019f19fe07ea";
        let child_info = crate::session::info::Info {
            id: acp::SessionId::new(session_id),
            cwd: child_cwd.clone(),
        };
        let child_dir = crate::session::persistence::session_dir(&child_info);
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(child_dir.join("summary.json"), "{}").unwrap();
        std::fs::write(
            child_dir.join("updates.jsonl"),
            [
                r#"{"timestamp":1,"method":"session/update","params":{"seq":1}}"#,
                r#"{"timestamp":2,"method":"session/update","params":{"seq":2}}"#,
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();

        // Sanity: the parent-cwd path must not exist (else the test is moot).
        let parent_info = crate::session::info::Info {
            id: acp::SessionId::new(session_id),
            cwd: parent_cwd.clone(),
        };
        assert!(
            !crate::session::persistence::session_dir(&parent_info)
                .join("updates.jsonl")
                .exists()
        );

        let gw = dummy_gateway();
        let response = handle(&make_request(session_id, &parent_cwd, None, None), &gw)
            .await
            .unwrap();
        let json = parse_response(response);

        assert_eq!(
            json["totalCount"], 2,
            "id fallback should resolve the divergent-cwd transcript"
        );
        assert_eq!(json["updates"].as_array().unwrap().len(), 2);

        // Clean up the dir written under the real echoagent home.
        let _ = std::fs::remove_dir_all(&child_dir);
    }

    fn capturing_gateway() -> (
        echo_agent_acp::AcpAgentGatewaySender,
        tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (echo_agent_acp::AcpAgentGatewaySender::new(tx), rx)
    }

    fn make_stream_request(
        session_id: &str,
        cwd: &str,
        stream: bool,
        chunk_size: Option<usize>,
        offset: Option<i64>,
    ) -> acp::ExtRequest {
        let mut map = serde_json::Map::new();
        map.insert("sessionId".into(), serde_json::json!(session_id));
        map.insert("cwd".into(), serde_json::json!(cwd));
        map.insert("stream".into(), serde_json::json!(stream));
        if let Some(cs) = chunk_size {
            map.insert("chunkSize".into(), serde_json::json!(cs));
        }
        if let Some(off) = offset {
            map.insert("offset".into(), serde_json::json!(off));
        }
        let raw = serde_json::value::to_raw_value(&serde_json::Value::Object(map)).unwrap();
        acp::ExtRequest::new("echo.agent/session/updates", std::sync::Arc::from(raw))
    }

    fn extract_chunk_params(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
    ) -> Vec<serde_json::Value> {
        let mut chunks = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg {
                let params: serde_json::Value =
                    serde_json::from_str(args.request.params.get()).unwrap();
                chunks.push(params);
                let _ = args.response_tx.send(Ok(()));
            }
        }
        chunks
    }

    #[tokio::test]
    async fn stream_sends_correct_chunks() {
        let cwd_tmp = tempfile::TempDir::new().unwrap();
        let cwd = cwd_tmp.path().to_string_lossy().to_string();
        let session_id = "stream-chunks";
        let session_dir = crate::session::persistence::session_dir(&crate::session::info::Info {
            id: acp::SessionId::new(session_id),
            cwd: cwd.clone(),
        });
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("updates.jsonl"),
            [
                r#"{"timestamp":1,"method":"session/update","params":{"seq":1}}"#,
                r#"{"timestamp":2,"method":"session/update","params":{"seq":2}}"#,
                r#"{"timestamp":3,"method":"session/update","params":{"seq":3}}"#,
                r#"{"timestamp":4,"method":"session/update","params":{"seq":4}}"#,
                r#"{"timestamp":5,"method":"session/update","params":{"seq":5}}"#,
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();

        let (gw, mut rx) = capturing_gateway();
        let response = handle(
            &make_stream_request(session_id, &cwd, true, Some(2), None),
            &gw,
        )
        .await
        .unwrap();
        let json = parse_response(response);

        assert_eq!(json["totalCount"], 5);
        assert_eq!(json["chunkCount"], 3);
        assert!(json.get("updates").is_none());

        let chunks = extract_chunk_params(&mut rx);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0]["index"], 0);
        assert_eq!(chunks[0]["updates"].as_array().unwrap().len(), 2);
        assert_eq!(chunks[0]["done"], false);
        assert_eq!(chunks[1]["index"], 1);
        assert_eq!(chunks[1]["updates"].as_array().unwrap().len(), 2);
        assert_eq!(chunks[1]["done"], false);
        assert_eq!(chunks[2]["index"], 2);
        assert_eq!(chunks[2]["updates"].as_array().unwrap().len(), 1);
        assert_eq!(chunks[2]["done"], true);
    }

    #[tokio::test]
    async fn stream_empty_session_returns_metadata_shape() {
        let cwd_tmp = tempfile::TempDir::new().unwrap();
        let cwd = cwd_tmp.path().to_string_lossy().to_string();
        let session_id = "stream-empty";

        let (gw, mut rx) = capturing_gateway();
        let response = handle(
            &make_stream_request(session_id, &cwd, true, None, None),
            &gw,
        )
        .await
        .unwrap();
        let json = parse_response(response);

        assert_eq!(json["totalCount"], 0);
        assert_eq!(json["chunkCount"], 0);
        assert!(json.get("updates").is_none());

        let chunks = extract_chunk_params(&mut rx);
        assert_eq!(chunks.len(), 0);
    }

    fn make_turn_index_request(
        session_id: &str,
        cwd: &str,
        turn_index: usize,
        offset: Option<i64>,
        limit: Option<usize>,
    ) -> acp::ExtRequest {
        let mut map = serde_json::Map::new();
        map.insert("sessionId".into(), serde_json::json!(session_id));
        map.insert("cwd".into(), serde_json::json!(cwd));
        map.insert("turnIndex".into(), serde_json::json!(turn_index));
        if let Some(off) = offset {
            map.insert("offset".into(), serde_json::json!(off));
        }
        if let Some(lim) = limit {
            map.insert("limit".into(), serde_json::json!(lim));
        }
        let raw = serde_json::value::to_raw_value(&serde_json::Value::Object(map)).unwrap();
        acp::ExtRequest::new("echo.agent/session/updates", std::sync::Arc::from(raw))
    }

    fn user_chunk(text: &str) -> String {
        format!(
            r#"{{"timestamp":0,"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"user_message_chunk","content":{{"type":"text","text":"{text}"}}}}}}}}"#
        )
    }

    fn agent_chunk(text: &str) -> String {
        format!(
            r#"{{"timestamp":0,"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"{text}"}}}}}}}}"#
        )
    }

    fn echo_agent_rewind(target: usize) -> String {
        format!(
            r#"{{"timestamp":0,"method":"_echo.agent/session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"rewind_marker","target_prompt_index":{target}}}}}}}"#
        )
    }

    fn write_session(session_id: &str, cwd: &str, lines: &[String]) {
        let session_info = crate::session::info::Info {
            id: acp::SessionId::new(session_id),
            cwd: cwd.to_string(),
        };
        let session_dir = crate::session::persistence::session_dir(&session_info);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join("updates.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    #[tokio::test]
    async fn turn_index_returns_last_n_turns() {
        let cwd_tmp = tempfile::TempDir::new().unwrap();
        let cwd = cwd_tmp.path().to_string_lossy().to_string();
        let session_id = "tail-prompts-basic";

        // 3 turns: user+agent, user+agent, user+agent
        let lines = vec![
            user_chunk("p1"),
            agent_chunk("r1"),
            user_chunk("p2"),
            agent_chunk("r2"),
            user_chunk("p3"),
            agent_chunk("r3"),
        ];
        write_session(session_id, &cwd, &lines);

        let gw = dummy_gateway();
        let response = handle(
            &make_turn_index_request(session_id, &cwd, 2, None, None),
            &gw,
        )
        .await
        .unwrap();
        let json = parse_response(response);

        assert_eq!(json["totalCount"], 6);
        assert_eq!(json["hasMore"], true);
        let updates = json["updates"].as_array().unwrap();
        // Last 2 turns = 4 updates (p2, r2, p3, r3)
        assert_eq!(updates.len(), 4);
        let rendered = serde_json::to_string(updates).unwrap();
        assert!(rendered.contains("p2"));
        assert!(rendered.contains("r3"));
        assert!(!rendered.contains("p1"));
        // promptStarts: indices in full array
        let ps = json["promptStarts"].as_array().unwrap();
        assert_eq!(ps, &[0, 2, 4]);
    }

    #[tokio::test]
    async fn turn_index_exceeding_turns_returns_all() {
        let cwd_tmp = tempfile::TempDir::new().unwrap();
        let cwd = cwd_tmp.path().to_string_lossy().to_string();
        let session_id = "tail-prompts-exceed";

        let lines = vec![
            user_chunk("p1"),
            agent_chunk("r1"),
            user_chunk("p2"),
            agent_chunk("r2"),
        ];
        write_session(session_id, &cwd, &lines);

        let gw = dummy_gateway();
        let response = handle(
            &make_turn_index_request(session_id, &cwd, 100, None, None),
            &gw,
        )
        .await
        .unwrap();
        let json = parse_response(response);

        assert_eq!(json["totalCount"], 4);
        assert_eq!(json["hasMore"], false);
        assert_eq!(json["updates"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn turn_index_with_rewinds() {
        let cwd_tmp = tempfile::TempDir::new().unwrap();
        let cwd = cwd_tmp.path().to_string_lossy().to_string();
        let session_id = "tail-prompts-rewind";

        // Turn 1, Turn 2 (dead), rewind, Turn 2 (replacement), Turn 3
        let lines = vec![
            user_chunk("p1"),
            agent_chunk("r1"),
            user_chunk("dead"),
            agent_chunk("dead-r"),
            echo_agent_rewind(1),
            user_chunk("p2-new"),
            agent_chunk("r2-new"),
            user_chunk("p3"),
            agent_chunk("r3"),
        ];
        write_session(session_id, &cwd, &lines);

        let gw = dummy_gateway();
        let response = handle(
            &make_turn_index_request(session_id, &cwd, 2, None, None),
            &gw,
        )
        .await
        .unwrap();
        let json = parse_response(response);

        // Live lines: p1, r1, p2-new, r2-new, p3, r3 = 6
        assert_eq!(json["totalCount"], 6);
        assert_eq!(json["hasMore"], true);
        let updates = json["updates"].as_array().unwrap();
        // Last 2 turns: p2-new, r2-new, p3, r3
        assert_eq!(updates.len(), 4);
        let rendered = serde_json::to_string(updates).unwrap();
        assert!(!rendered.contains("dead"));
        assert!(rendered.contains("p2-new"));
        assert!(rendered.contains("p3"));
        // promptStarts over live lines
        let ps = json["promptStarts"].as_array().unwrap();
        assert_eq!(ps, &[0, 2, 4]);
    }

    #[tokio::test]
    async fn turn_index_ignored_when_offset_set() {
        let cwd_tmp = tempfile::TempDir::new().unwrap();
        let cwd = cwd_tmp.path().to_string_lossy().to_string();
        let session_id = "tail-prompts-offset-priority";

        let lines = vec![
            user_chunk("p1"),
            agent_chunk("r1"),
            user_chunk("p2"),
            agent_chunk("r2"),
            user_chunk("p3"),
            agent_chunk("r3"),
        ];
        write_session(session_id, &cwd, &lines);

        let gw = dummy_gateway();
        // offset set → turnIndex should be ignored, offset takes priority
        let response = handle(
            &make_turn_index_request(session_id, &cwd, 1, Some(-2), None),
            &gw,
        )
        .await
        .unwrap();
        let json = parse_response(response);

        assert_eq!(json["totalCount"], 6);
        // offset -2 → last 2 updates
        let updates = json["updates"].as_array().unwrap();
        assert_eq!(updates.len(), 2);
    }

    #[tokio::test]
    async fn prompt_starts_included_in_regular_response() {
        let cwd_tmp = tempfile::TempDir::new().unwrap();
        let cwd = cwd_tmp.path().to_string_lossy().to_string();
        let session_id = "prompt-starts-regular";

        let lines = vec![
            user_chunk("p1"),
            agent_chunk("r1"),
            user_chunk("p2"),
            agent_chunk("r2"),
        ];
        write_session(session_id, &cwd, &lines);

        let gw = dummy_gateway();
        // Regular request (no turnIndex, no offset)
        let response = handle(&make_request(session_id, &cwd, None, None), &gw)
            .await
            .unwrap();
        let json = parse_response(response);

        let ps = json["promptStarts"].as_array().unwrap();
        assert_eq!(ps, &[0, 2]);
    }

    #[tokio::test]
    async fn prompt_starts_in_streamed_metadata() {
        let cwd_tmp = tempfile::TempDir::new().unwrap();
        let cwd = cwd_tmp.path().to_string_lossy().to_string();
        let session_id = "prompt-starts-stream";

        let lines = vec![
            user_chunk("p1"),
            agent_chunk("r1"),
            user_chunk("p2"),
            agent_chunk("r2"),
        ];
        write_session(session_id, &cwd, &lines);

        let (gw, mut rx) = capturing_gateway();
        let response = handle(
            &make_stream_request(session_id, &cwd, true, Some(64), None),
            &gw,
        )
        .await
        .unwrap();
        let json = parse_response(response);

        let ps = json["promptStarts"].as_array().unwrap();
        assert_eq!(ps, &[0, 2]);
        // Drain the chunks
        let _ = extract_chunk_params(&mut rx);
    }
}
