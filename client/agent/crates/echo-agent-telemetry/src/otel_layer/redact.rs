use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::LazyLock;

use opentelemetry::trace::{Event, Status};
use opentelemetry::{Array, KeyValue, StringValue, Value};
use opentelemetry_sdk::trace::SpanData;

/// Adding a span attribute (default-deny via `enforce_allowlist`): record
/// numerics as `i64` (`u64` serializes as a string and is dropped); derive
/// label values from an enum `as_str()`; add string keys here and to the
/// round-trip test pin, and only if they carry no user content.
pub(super) static ALLOWED_STRING_KEYS: &[&str] = &[
    // tracing-opentelemetry / framework-injected
    "level",
    "target",
    "code.namespace",
    "code.filepath",
    "thread.name",
    // identifiers
    "session_id",
    "prompt_id",
    "req_id",
    "request_id",
    "child_session_id",
    "parent_session_id",
    "subagent_id",
    "agent_id",
    "task_id",
    "tool_call_id",
    "call_id",
    "event_id",
    "conv_id",
    "turn_id",
    // model / client
    "model_id",
    "model",
    "compact_model",
    "client_type",
    "client_version",
    "subagent_type",
    "persona",
    "role",
    // tool / skill / mcp / method NAMES (identifiers, not arguments)
    "skill_name",
    "server_name",
    "tool_name",
    "tool_names",
    "method",
    "operation",
    "endpoint",
    // paths / urls (additionally home-path- and url-scrubbed by redact_value)
    "path",
    "file_path",
    "repo_path",
    "gcs_path",
    "gcs_url",
    "url",
    "output_path",
    "dir",
    "dir_path",
    "notebook",
    "cwd",
    "original_cwd",
    "chosen_repo_root",
    "worktree",
    "source",
    "bucket_url",
    "object_path",
    "archive_name",
    "artifact",
    // enums / classifications
    "verdict",
    "pattern_class",
    "phase",
    "upload_reason",
    "suppress_reason",
    "error_kind",
    "error_category",
    "error_type",
    "outcome",
    "decision",
    "update_type",
    "kind",
    "step",
    "token_type",
    "stop_reason",
    "compaction_outcome",
    "compaction_stop_reason",
    "compaction_trigger",
    "compaction_prefire_outcome",
    "aspect_ratio",
    "resolution",
    "schedule",
    "interval",
    "mode",
    "detail",
    // span enums + plugin/auth/survey/mcp identifiers (categorical, no user content)
    "status",
    "action",
    "auth_method",
    "to_mode",
    "trigger",
    "survey_type",
    "mention_type",
    "install_kind",
    "transport_type",
    "invocation_trigger",
    "skill_source",
    "plugin_name",
    "plugin_version",
    "plugin_scope",
    "hook_event",
    "hook_name",
    "hook_type",
    "hook_source",
    "server_scope",
    "mcp_server.name",
    "mcp_tool.name",
    "agent.name",
    "skill.name",
    "query_source",
    "effort",
    "start_type",
    "error",
    "location",
    "user_id",
    "parent_agent_id",
    "from_mode",
    "tool_use_id",
    "command_name",
    "command_source",
    "event_type",
    "appearance_id",
    // terminal telemetry
    "terminal.brand",
    "terminal.multiplexer",
    "terminal.tmux_version",
    "terminal.term_var",
    "terminal.term_version",
    "terminal.term_version_source",
    "skip_reason",
    "auto_cadence_reason",
];

/// O(1) lookup view over [`ALLOWED_STRING_KEYS`].
static ALLOWED_STRING_KEY_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ALLOWED_STRING_KEYS.iter().copied().collect());

/// Allowlisted keys holding full URLs: reduced to `scheme://host[:port]` so
/// user-influenced path/query can't export. Storage-*path* keys (`gcs_path`,
/// `object_path`, `output_path`) are excluded — those paths are wanted.
static URL_VALUED_KEYS: &[&str] = &["url", "endpoint", "gcs_url", "bucket_url"];

/// Scrub every text-bearing surface of each span before export.
pub(super) fn redact_batch(batch: &mut [SpanData]) {
    for span in batch.iter_mut() {
        // Exhaustive destructure (no `..`): a new `SpanData` field in a future
        // `opentelemetry_sdk` fails to compile here instead of exporting unscrubbed.
        let SpanData {
            name,
            attributes,
            events,
            links,
            status,
            span_context: _,
            parent_span_id: _,
            parent_span_is_remote: _,
            span_kind: _,
            start_time: _,
            end_time: _,
            dropped_attributes_count: _,
            instrumentation_scope: _,
        } = span;
        redact_in_place(name);
        scrub_attributes(attributes);
        for event in &mut events.events {
            neuter_event_name(event);
            // Re-scrub: synthesized callsite paths can be absolute (home dir).
            redact_in_place(&mut event.name);
            scrub_attributes(&mut event.attributes);
        }
        // Keep the error message (useful telemetry); scrub secrets/paths from it.
        if let Status::Error { description } = status {
            redact_in_place(description);
        }
        for link in &mut links.links {
            scrub_attributes(&mut link.attributes);
        }
    }
}

/// Numeric/bool scalars and their arrays are content-free; everything else —
/// strings and any future `#[non_exhaustive]` variant — is content (fail-closed).
fn is_content_value(value: &Value) -> bool {
    !matches!(
        value,
        Value::Bool(_)
            | Value::I64(_)
            | Value::F64(_)
            | Value::Array(Array::Bool(_) | Array::I64(_) | Array::F64(_))
    )
}

/// Default-deny: drop content-valued attributes whose key isn't allowlisted.
fn enforce_allowlist(attrs: &mut Vec<KeyValue>) {
    attrs.retain(|kv| {
        !is_content_value(&kv.value) || ALLOWED_STRING_KEY_SET.contains(kv.key.as_str())
    });
}

fn scrub_attributes(attrs: &mut Vec<KeyValue>) {
    enforce_allowlist(attrs);
    for kv in attrs.iter_mut() {
        if URL_VALUED_KEYS.contains(&kv.key.as_str()) {
            reduce_url_to_origin(&mut kv.value);
        }
        redact_value(&mut kv.value);
    }
}

/// An event's name is the formatted `tracing` message (`Event.name`) — free
/// text the key allowlist can't gate, so replace it with the static callsite id
/// (fail-closed). Rebuilt from the `code.filepath`/`code.lineno` attrs that
/// `tracing-opentelemetry` attaches to every event (`with_location`, default-on).
fn neuter_event_name(event: &mut Event) {
    let mut file: Option<String> = None;
    let mut line: Option<i64> = None;
    for kv in &event.attributes {
        match kv.key.as_str() {
            "code.filepath" => {
                if let Value::String(s) = &kv.value {
                    file = Some(s.as_str().to_owned());
                }
            }
            "code.lineno" => {
                if let Value::I64(n) = &kv.value {
                    line = Some(*n);
                }
            }
            _ => {}
        }
    }
    event.name = match (file, line) {
        (Some(f), Some(l)) => format!("{f}:{l}").into(),
        (Some(f), None) => f.into(),
        // No location attrs (e.g. a raw-API event): drop the message entirely.
        _ => Cow::Borrowed("event"),
    };
}

/// Reduce a URL to `scheme://host[:port]` — its path/query can carry user
/// content. Unparseable values pass through to the secret scrubber.
fn reduce_url_to_origin(value: &mut Value) {
    if let Value::String(s) = value
        && let Cow::Owned(origin) = crate::redact_common::url_origin(s.as_str())
    {
        *s = StringValue::from(origin);
    }
}

/// Secret-shape then user-path scrub (shared with the external pipeline).
/// Returns `Some` only when the input changed (owned, so callers can
/// overwrite in place).
fn redact_owned(input: &str) -> Option<String> {
    crate::redact_common::redact_owned(input)
}

fn redact_in_place(s: &mut Cow<'static, str>) {
    if let Some(redacted) = redact_owned(s.as_ref()) {
        *s = Cow::Owned(redacted);
    }
}

fn redact_value(value: &mut Value) {
    match value {
        Value::String(s) => {
            if let Some(redacted) = redact_owned(s.as_str()) {
                *s = StringValue::from(redacted);
            }
        }
        Value::Array(Array::String(items)) => {
            for s in items.iter_mut() {
                if let Some(redacted) = redact_owned(s.as_str()) {
                    *s = StringValue::from(redacted);
                }
            }
        }
        // Non-string variants carry no free text; `Value` is `#[non_exhaustive]`.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_value_scrubs_secret_string() {
        let mut v = Value::String(StringValue::from(
            "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.foo.bar.baz".to_string(),
        ));
        redact_value(&mut v);
        let Value::String(s) = &v else {
            panic!("expected string value");
        };
        assert!(
            s.as_str().contains("[REDACTED_SECRET]"),
            "secret not scrubbed: {}",
            s.as_str()
        );
    }

    #[test]
    fn event_name_neutered_to_callsite_drops_message_content() {
        let mut ev = Event::new(
            "received prompt: rm -rf /Users/alice/secret",
            std::time::SystemTime::now(),
            vec![
                KeyValue::new("code.filepath", "src/foo.rs"),
                KeyValue::new("code.lineno", 42_i64),
            ],
            0,
        );
        neuter_event_name(&mut ev);
        assert_eq!(ev.name, "src/foo.rs:42");
        assert!(!ev.name.contains("prompt") && !ev.name.contains("rm -rf"));
    }

    #[test]
    fn event_name_without_location_drops_to_marker() {
        let mut ev = Event::new("SECRET {x:?}", std::time::SystemTime::now(), vec![], 0);
        neuter_event_name(&mut ev);
        assert_eq!(ev.name, "event");
    }

    #[test]
    fn allowlist_drops_nonallowlisted_content_keeps_safe_and_numeric() {
        let mut attrs = vec![
            KeyValue::new("session_id", "sess-abc"), // allowlisted string
            KeyValue::new("path", "/tmp/x.rs"),      // allowlisted string
            KeyValue::new("prompt", "CANARY_PROMPT secret user text"), // not allowlisted → drop
            KeyValue::new("command", "echo CANARY_SECRET"), // not allowlisted → drop
            KeyValue::new("turn_number", 7_i64),     // numeric → keep
            KeyValue::new("is_background", true),    // bool → keep
        ];
        enforce_allowlist(&mut attrs);
        let keys: Vec<&str> = attrs.iter().map(|kv| kv.key.as_str()).collect();
        assert!(keys.contains(&"session_id"));
        assert!(keys.contains(&"path"));
        assert!(keys.contains(&"turn_number"));
        assert!(keys.contains(&"is_background"));
        assert!(
            !keys.contains(&"prompt"),
            "non-allowlisted content must be dropped"
        );
        assert!(
            !keys.contains(&"command"),
            "non-allowlisted content must be dropped"
        );
        // Canary: no dropped content survives anywhere in the attribute set.
        let blob = format!("{attrs:?}");
        assert!(
            !blob.contains("CANARY_PROMPT"),
            "prompt content leaked: {blob}"
        );
        assert!(
            !blob.contains("CANARY_SECRET"),
            "command content leaked: {blob}"
        );
    }

    #[test]
    fn allowlist_contents_are_pinned() {
        // Keep this an independent copy — don't reference ALLOWED_STRING_KEYS, or
        // the assert becomes a tautology and stops gating allowlist changes.
        let expected: &[&str] = &[
            "level",
            "target",
            "code.namespace",
            "code.filepath",
            "thread.name",
            "session_id",
            "prompt_id",
            "req_id",
            "request_id",
            "child_session_id",
            "parent_session_id",
            "subagent_id",
            "agent_id",
            "task_id",
            "tool_call_id",
            "call_id",
            "event_id",
            "conv_id",
            "turn_id",
            "model_id",
            "model",
            "compact_model",
            "client_type",
            "client_version",
            "subagent_type",
            "persona",
            "role",
            "skill_name",
            "server_name",
            "tool_name",
            "tool_names",
            "method",
            "operation",
            "endpoint",
            "path",
            "file_path",
            "repo_path",
            "gcs_path",
            "gcs_url",
            "url",
            "output_path",
            "dir",
            "dir_path",
            "notebook",
            "cwd",
            "original_cwd",
            "chosen_repo_root",
            "worktree",
            "source",
            "bucket_url",
            "object_path",
            "archive_name",
            "artifact",
            "verdict",
            "pattern_class",
            "phase",
            "upload_reason",
            "suppress_reason",
            "error_kind",
            "error_category",
            "error_type",
            "outcome",
            "decision",
            "update_type",
            "kind",
            "step",
            "token_type",
            "stop_reason",
            "compaction_outcome",
            "compaction_stop_reason",
            "compaction_trigger",
            "compaction_prefire_outcome",
            "aspect_ratio",
            "resolution",
            "schedule",
            "interval",
            "mode",
            "detail",
            "status",
            "action",
            "auth_method",
            "to_mode",
            "trigger",
            "survey_type",
            "mention_type",
            "install_kind",
            "transport_type",
            "invocation_trigger",
            "skill_source",
            "plugin_name",
            "plugin_version",
            "plugin_scope",
            "hook_event",
            "hook_name",
            "hook_type",
            "hook_source",
            "server_scope",
            "mcp_server.name",
            "mcp_tool.name",
            "agent.name",
            "skill.name",
            "query_source",
            "effort",
            "start_type",
            "error",
            "location",
            "user_id",
            "parent_agent_id",
            "from_mode",
            "tool_use_id",
            "command_name",
            "command_source",
            "event_type",
            "appearance_id",
            "terminal.brand",
            "terminal.multiplexer",
            "terminal.tmux_version",
            "terminal.term_var",
            "terminal.term_version",
            "terminal.term_version_source",
            "skip_reason",
            "auto_cadence_reason",
        ];
        assert_eq!(
            ALLOWED_STRING_KEYS, expected,
            "ALLOWED_STRING_KEYS changed: adding a key exports a new field — confirm it carries no \
             user content and get telemetry-owner review, then update this pin."
        );
    }

    #[test]
    fn error_status_message_retained_but_secret_scrubbed() {
        // Error messages are useful telemetry and must survive; only secret
        // shapes (and home/username paths) are scrubbed out of them.
        let mut status = Status::error("upstream auth failed: sk-CANARYabcdefghij1234567890");
        if let Status::Error { description } = &mut status {
            redact_in_place(description);
        }
        let Status::Error { description } = status else {
            panic!("status code must stay Error");
        };
        assert!(
            description.contains("upstream auth failed"),
            "useful message lost: {description}"
        );
        assert!(
            !description.contains("CANARY"),
            "secret survived: {description}"
        );
    }

    #[test]
    fn url_value_reduced_to_origin_dropping_path_and_query() {
        let mut attrs = vec![KeyValue::new(
            "url",
            "https://example.com:8443/search?q=CANARY+secret+terms&u=bob#frag",
        )];
        scrub_attributes(&mut attrs);
        let blob = format!("{attrs:?}");
        assert!(
            blob.contains("https://example.com:8443"),
            "origin lost: {blob}"
        );
        assert!(!blob.contains("CANARY"), "query content survived: {blob}");
        assert!(!blob.contains("search"), "path survived: {blob}");
    }

    #[test]
    fn url_valued_keys_reduced_to_origin_but_storage_paths_kept() {
        // Origin-reduction applies to every URL-valued key, not just `url`...
        let mut attrs = vec![
            KeyValue::new(
                "bucket_url",
                "https://store.example.com/b/CANARY/o?sig=CANARYSIG",
            ),
            KeyValue::new(
                "endpoint",
                "https://api.example.com:8443/v1/chat?u=CANARYUSER",
            ),
            // ...but storage *paths* are deliberately exported in full.
            KeyValue::new("gcs_path", "sessions/abc123/artifact-kept.tar"),
        ];
        scrub_attributes(&mut attrs);
        let blob = format!("{attrs:?}");
        assert!(
            blob.contains("https://store.example.com"),
            "bucket_url origin lost: {blob}"
        );
        assert!(
            blob.contains("https://api.example.com:8443"),
            "endpoint origin lost: {blob}"
        );
        assert!(
            !blob.contains("CANARY"),
            "url path/query content survived: {blob}"
        );
        assert!(
            blob.contains("sessions/abc123/artifact-kept.tar"),
            "storage path was wrongly reduced: {blob}"
        );
    }

    #[test]
    fn allowlisted_value_is_still_secret_scrubbed() {
        // Allowlisting a key permits the field; it does not exempt the value
        // from the shape scrub.
        let mut attrs = vec![KeyValue::new("source", "sk-CANARYabcdefghij1234567890")];
        scrub_attributes(&mut attrs);
        let blob = format!("{attrs:?}");
        assert!(
            !blob.contains("CANARY"),
            "secret in allowlisted value not scrubbed: {blob}"
        );
    }

    #[test]
    fn allowlisted_path_values_are_still_home_scrubbed() {
        // Path keys are allowlisted so the field exports, but home/username
        // segments must still collapse — allowlist is not a scrub bypass.
        let home = dirs::home_dir().expect("home dir for path-scrub test");
        let home_str = home.to_string_lossy();
        // Skip if the home path is too short/generic for the scrubber to match.
        if home_str.len() < 4 {
            return;
        }
        let full = format!("{home_str}/secret-project/src/main.rs");
        let mut attrs = vec![
            KeyValue::new("path", full.clone()),
            KeyValue::new("file_path", full.clone()),
            KeyValue::new("cwd", full.clone()),
        ];
        scrub_attributes(&mut attrs);
        let blob = format!("{attrs:?}");
        assert!(
            !blob.contains(home_str.as_ref()),
            "home path survived allowlisted scrub: {blob}"
        );
        assert!(
            blob.contains("main.rs") || blob.contains("[HOME]") || blob.contains("~"),
            "expected redacted path to retain a filename or home marker: {blob}"
        );
    }

    #[test]
    fn error_key_value_is_secret_and_path_scrubbed() {
        // Free-form `error` strings are allowlisted for classification labels;
        // any secret/path content that sneaks in must still be scrubbed.
        let home = dirs::home_dir().expect("home dir");
        let home_str = home.to_string_lossy();
        let msg =
            format!("failed reading {home_str}/.config/creds with sk-CANARYabcdefghij1234567890");
        let mut attrs = vec![KeyValue::new("error", msg)];
        scrub_attributes(&mut attrs);
        let blob = format!("{attrs:?}");
        assert!(!blob.contains("CANARY"), "secret survived in error: {blob}");
        if home_str.len() >= 4 {
            assert!(
                !blob.contains(home_str.as_ref()),
                "home path survived in error: {blob}"
            );
        }
    }
}
