use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::Serialize;

use crate::types::Event;

#[derive(Serialize)]
struct EventEntry {
    ts: String,
    #[serde(flatten)]
    event: Event,
}

const EVENTS_FILE: &str = "events.jsonl";

/// Shared event writer for `events.jsonl`. `Clone + Send + Sync`.
#[derive(Clone)]
pub struct EventWriter {
    inner: Arc<EventWriterInner>,
}

struct EventWriterInner {
    file: Mutex<Option<File>>,
    error_logged: AtomicBool,
}

impl EventWriter {
    pub fn open(session_dir: &Path) -> Self {
        let path = session_dir.join(EVENTS_FILE);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| {
                tracing::warn!(path = %path.display(), error = %e, "failed to open {EVENTS_FILE}");
                e
            })
            .ok();
        Self {
            inner: Arc::new(EventWriterInner {
                file: Mutex::new(file),
                error_logged: AtomicBool::new(false),
            }),
        }
    }

    /// No-op writer that discards all events.
    pub fn noop() -> Self {
        Self {
            inner: Arc::new(EventWriterInner {
                file: Mutex::new(None),
                error_logged: AtomicBool::new(true), // suppress error logging
            }),
        }
    }

    pub fn emit(&self, event: Event) {
        let entry = EventEntry {
            ts: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            event,
        };
        let Ok(mut line) = serde_json::to_vec(&entry) else {
            return;
        };
        line.push(b'\n');

        let Ok(mut guard) = self.inner.file.lock() else {
            return;
        };
        if let Some(ref mut f) = *guard
            && let Err(e) = f.write_all(&line)
            && !self.inner.error_logged.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(error = %e, "{EVENTS_FILE} write failed");
        }
    }
}

impl std::fmt::Debug for EventWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventWriter").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        EVENT_SCHEMA_VERSION, Event, SessionRelationship, ToolOutcome, TurnOutcomeLabel,
    };

    fn _assert_event_writer_is_send_sync_clone()
    where
        EventWriter: Send + Sync + Clone,
    {
    }

    #[test]
    fn test_emit_writes_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let writer = EventWriter::open(dir.path());

        writer.emit(Event::TurnStarted {
            session_id: "test-session".into(),
            turn_number: 1,
            model_id: "echo-agent-3".into(),
            yolo_mode: false,
            conversation_message_count: 0,
            session_relationship: SessionRelationship::Primary,
            schema_version: EVENT_SCHEMA_VERSION.into(),
            redirect_kind: None,
        });
        writer.emit(Event::FirstToken);
        writer.emit(Event::ToolCompleted {
            tool_name: "bash".into(),
            duration_ms: 1500,
            outcome: ToolOutcome::Success,
            tool_call_id: "call_xyz".into(),
            source: crate::types::ToolCompletedSource::Shell,
        });
        writer.emit(Event::TurnEnded {
            outcome: TurnOutcomeLabel::Completed,
            cancellation_category: None,
            cancellation_context: None,
        });

        let text = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let lines: Vec<&str> = text.trim().split('\n').collect();
        assert_eq!(lines.len(), 4);

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["type"], "turn_started");
        assert_eq!(first["session_id"], "test-session");
        assert!(first["ts"].as_str().is_some());

        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["type"], "first_token");

        let third: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(third["type"], "tool_completed");
        assert_eq!(third["tool_name"], "bash");
        assert_eq!(third["duration_ms"], 1500);
        assert_eq!(third["tool_call_id"], "call_xyz");
        assert!(
            third.get("source").is_none(),
            "shell ToolCompleted must omit source"
        );

        let fourth: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
        assert_eq!(fourth["type"], "turn_ended");
        assert_eq!(fourth["outcome"], "completed");
        assert!(fourth.get("cancellation_category").is_none());
    }

    #[test]
    fn cloned_writer_shares_file() {
        let dir = tempfile::tempdir().unwrap();
        let w1 = EventWriter::open(dir.path());
        let w2 = w1.clone();

        w1.emit(Event::FirstToken);
        w2.emit(Event::FirstToken);

        let text = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let lines: Vec<&str> = text.trim().split('\n').collect();
        assert_eq!(lines.len(), 2, "both writes should go to the same file");
    }

    #[test]
    fn mcp_server_failed_serializes_enum_error_type() {
        let dir = tempfile::tempdir().unwrap();
        let w = EventWriter::open(dir.path());

        w.emit(Event::McpServerFailed {
            server_name: "confluence".into(),
            transport: Some("http".into()),
            target: Some("https://mcp.confluence.example.com".into()),
            error_type: crate::types::McpErrorCategory::Timeout,
            error_message: "timed out after 10s".into(),
            duration_ms: Some(10002),
            timeout_sec: Some(10),
        });

        let text = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let val: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(val["type"], "mcp_server_failed");
        assert_eq!(val["error_type"], "timeout");
        assert_eq!(val["server_name"], "confluence");
        assert_eq!(val["duration_ms"], 10002);
    }
}
