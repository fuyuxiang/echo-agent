//! Mirror types for the leader "session roster" wire format.
//!
//! The leader process hosts session actors and exposes a roster API the
//! pager consumes in leader mode (FleetView dashboard):
//!
//! - Request/response `echo.agent/sessions/list` → [`RosterListResponse`].
//! - Broadcast notification `echo.agent/sessions/changed` → [`RosterChanged`].
//!
//! These structs mirror the producer-side wire format (camelCase JSON,
//! snake_case activity enum). They are deserialize-only — the pager never
//! produces them.

use serde::Deserialize;

/// Coarse activity state for a roster entry. Wire format is snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RosterActivity {
    Working,
    Idle,
    NeedsInput,
    Dormant,
    Completed,
    Dead,
}

/// Origin of a roster entry (local leader vs. a remote host). We don't
/// render origin yet, but must parse it without failing.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RosterOrigin {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub host: Option<String>,
}

/// A single session in the leader roster.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterEntry {
    pub session_id: String,
    #[serde(default)]
    pub title: Option<String>,
    pub cwd: String,
    #[serde(default)]
    pub is_worktree: bool,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub yolo: bool,
    pub activity: RosterActivity,
    /// Ultra-short summary of the session's most recent turn, shown as the
    /// row's secondary line.
    #[serde(default)]
    pub last_turn_summary: Option<String>,
    #[serde(default)]
    pub resident: bool,
    #[serde(default)]
    pub last_change_unix_ms: i64,
    #[serde(default)]
    pub origin: RosterOrigin,
}

/// Response to `echo.agent/sessions/list`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RosterListResponse {
    #[serde(default)]
    pub sessions: Vec<RosterEntry>,
}

/// Broadcast payload for `echo.agent/sessions/changed`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RosterChanged {
    #[serde(default)]
    pub upserted: Vec<RosterEntry>,
    #[serde(default)]
    pub removed: Vec<String>,
}

/// Parse an `echo.agent/sessions/list` ext-response body into a [`RosterListResponse`].
///
/// The agent serializes the response through
/// `ExtMethodResult::success(..).to_ext_response()` (see
/// `echo-agent-runtime/src/agent/handlers/session.rs::handle_roster_list`), which
/// wraps the payload in a JSON-RPC-style `{ "result": { "sessions": [...] } }`
/// envelope. A bare `{ "sessions": [...] }` body (no envelope) is tolerated too.
///
/// We MUST unwrap `result` *first*: [`RosterListResponse::sessions`] is
/// `#[serde(default)]` and the struct does not deny unknown fields, so a direct
/// `serde_json::from_str::<RosterListResponse>` on the wrapped body would
/// silently *succeed* with an empty roster (it never finds a top-level
/// `sessions` key, so it defaults to `[]` and ignores the unknown `result`
/// key). That was the original bug: the poll returned an empty roster on every
/// tick and — because [`crate::app::actions::TaskResult::RosterLoaded`] replaces
/// `leader_roster` wholesale — also clobbered any entry delivered by the
/// `echo.agent/sessions/changed` broadcast. Mirrors how `Effect::FetchSessionList`
/// unwraps `result` for `echo.agent/session/list`.
pub fn parse_roster_list_response(body: &str) -> Option<RosterListResponse> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let payload = value.get("result").unwrap_or(&value);
    serde_json::from_value::<RosterListResponse>(payload.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a representative agent-side roster entry with every field set to
    /// a non-default value so the round-trip exercises name/case mapping.
    fn agent_entry() -> echo_agent_runtime::agent::roster::RosterEntry {
        use echo_agent_runtime::agent::roster as agent;
        agent::RosterEntry {
            session_id: "sess-abc".to_string(),
            title: Some("Fix the roster".to_string()),
            cwd: "/repo/worktree".to_string(),
            is_worktree: true,
            model_id: Some("echo-agent-4".to_string()),
            reasoning_effort: None,
            yolo: true,
            activity: agent::RosterActivity::Working,
            last_turn_summary: Some("Fixed the roster merge".to_string()),
            resident: true,
            last_change_unix_ms: 1_725_000_000_123,
            origin: agent::RosterOrigin::Local,
        }
    }

    /// Serialize the agent's `RosterListResponse` EXACTLY as
    /// `handle_roster_list` does — through
    /// `ExtMethodResult::success(..).to_ext_response()` — and confirm the
    /// pager recovers the session.
    ///
    /// This reproduces the production bug: the agent wraps the payload in a
    /// `{ "result": { "sessions": [...] } }` envelope, and the pager's first
    /// parse attempt used to be a direct `from_str::<RosterListResponse>` that
    /// silently succeeded with an EMPTY roster (the `naive` assertion below).
    /// Before the fix `parse_roster_list_response` used that same direct parse
    /// first, so this test FAILED (0 sessions); after the fix it unwraps
    /// `result` first and PASSES.
    #[test]
    fn roster_list_response_survives_result_envelope() {
        use echo_agent_runtime::agent::roster as agent;
        use echo_agent_runtime::session::ExtMethodResult;

        let agent_resp = agent::RosterListResponse {
            sessions: vec![agent_entry()],
        };

        // Exact wire bytes the agent emits for `echo.agent/sessions/list`.
        let ext_response = ExtMethodResult::success(agent_resp)
            .to_ext_response()
            .expect("agent serializes the roster response");
        let body: &str = ext_response.0.get();
        assert!(
            body.contains("\"result\""),
            "agent wraps the payload in a `result` envelope: {body}"
        );

        // Repro of the original bug mechanism: a naive direct deserialize of
        // the wrapped body succeeds but drops every session.
        let naive: RosterListResponse =
            serde_json::from_str(body).expect("naive parse succeeds (that is the trap)");
        assert!(
            naive.sessions.is_empty(),
            "naive direct parse silently drops sessions from the envelope"
        );

        // The fixed parser unwraps `result` first and recovers the session.
        let parsed = parse_roster_list_response(body).expect("fixed parser must parse");
        assert_eq!(
            parsed.sessions.len(),
            1,
            "session must survive the `result` envelope"
        );
        let e = &parsed.sessions[0];
        assert_eq!(e.session_id, "sess-abc");
        assert_eq!(e.title.as_deref(), Some("Fix the roster"));
        assert_eq!(e.cwd, "/repo/worktree");
        assert!(e.is_worktree);
        assert_eq!(e.model_id.as_deref(), Some("echo-agent-4"));
        assert!(e.yolo);
        assert_eq!(e.activity, RosterActivity::Working);
        assert_eq!(
            e.last_turn_summary.as_deref(),
            Some("Fixed the roster merge")
        );
        assert!(e.resident);
        assert_eq!(e.last_change_unix_ms, 1_725_000_000_123);
        assert_eq!(e.origin.kind, "local");
    }

    /// A bare `{ "sessions": [...] }` body (no `result` envelope) must still
    /// parse — the parser tolerates both shapes.
    #[test]
    fn roster_list_response_parses_bare_body() {
        let body = r#"{"sessions":[{"sessionId":"s1","cwd":"/x","isWorktree":false,"yolo":false,"activity":"idle","resident":true,"lastChangeUnixMs":7,"origin":{"kind":"local"}}]}"#;
        let parsed = parse_roster_list_response(body).expect("bare body parses");
        assert_eq!(parsed.sessions.len(), 1);
        assert_eq!(parsed.sessions[0].session_id, "s1");
    }

    /// `echo.agent/sessions/changed` round-trip: serialize the agent's `RosterChanged`
    /// exactly as `emit_roster_changed` does (bare params, no `result`
    /// envelope) and confirm the pager's `RosterChanged` recovers `upserted` /
    /// `removed` and the nested entry fields (camelCase). Regression guard for
    /// the broadcast path's wire shape.
    #[test]
    fn roster_changed_round_trips() {
        use echo_agent_runtime::agent::roster as agent;

        let agent_changed = agent::RosterChanged {
            upserted: vec![agent_entry()],
            removed: vec!["sess-gone".to_string()],
        };
        // `emit_roster_changed` serializes the bare payload (no envelope).
        let params = serde_json::to_string(&agent_changed).expect("serialize RosterChanged");

        let parsed: RosterChanged =
            serde_json::from_str(&params).expect("pager parses the broadcast params");
        assert_eq!(parsed.upserted.len(), 1, "upserted entry must survive");
        assert_eq!(parsed.upserted[0].session_id, "sess-abc");
        assert_eq!(parsed.upserted[0].activity, RosterActivity::Working);
        assert_eq!(parsed.removed, vec!["sess-gone".to_string()]);
    }
}
