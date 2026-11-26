//! Validation rules for every identifier newtype, plus the synthetic
//! `ServerId` helper invariants.

use std::str::FromStr;

use echo_agent_tool_protocol::{
    ConnectionId, IdError, RequestId, ServerId, SessionId, ToolCallId, ToolId, UserId,
};

#[test]
fn tool_id_accepts_bare_and_namespaced_names() {
    let bare = ToolId::new("read_file").unwrap();
    assert_eq!(bare.as_str(), "read_file");

    let namespaced = ToolId::new("EchoAgentBuild:read_file").unwrap();
    assert_eq!(namespaced.as_str(), "EchoAgentBuild:read_file");

    assert_eq!(
        ToolId::from_str("github:list_repos").unwrap().as_str(),
        "github:list_repos"
    );
}

#[test]
fn tool_id_rejects_empty() {
    assert_eq!(ToolId::new("").unwrap_err(), IdError::Empty);
}

#[test]
fn tool_id_rejects_more_than_one_separator() {
    let err = ToolId::new("foo:bar:baz").unwrap_err();
    assert_eq!(
        err,
        IdError::InvalidFormat {
            value: "foo:bar:baz".to_owned()
        }
    );
}

#[test]
fn tool_id_rejects_disallowed_characters() {
    for bad in [
        "foo bar",
        "foo!bar",
        "foo/bar",
        "foo.bar",
        "foo:bar baz",
        " foo",
        "foo ",
        "foo\tbar",
        "foo\u{00e9}bar",
        "f\u{00f6}\u{00f6}bar",
    ] {
        let err = ToolId::new(bad).unwrap_err();
        assert!(
            matches!(err, IdError::InvalidFormat { ref value } if value == bad),
            "expected InvalidFormat for {bad:?}, got {err:?}"
        );
    }
}

#[test]
fn tool_id_accepts_digits_only_and_other_boundary_inputs() {
    assert_eq!(ToolId::new("123").unwrap().as_str(), "123");
    assert_eq!(ToolId::new("v2:42").unwrap().as_str(), "v2:42");
    assert_eq!(ToolId::new("a").unwrap().as_str(), "a");
    assert_eq!(ToolId::new("a:b").unwrap().as_str(), "a:b");
    assert_eq!(ToolId::new("-_-").unwrap().as_str(), "-_-");
}

#[test]
fn tool_id_rejects_empty_segments_around_separator() {
    for bad in [":foo", "foo:", ":"] {
        let err = ToolId::new(bad).unwrap_err();
        assert!(
            matches!(err, IdError::InvalidFormat { ref value } if value == bad),
            "expected InvalidFormat for {bad:?}, got {err:?}"
        );
    }
}

#[test]
fn tool_id_try_from_string_works() {
    let id: ToolId = "EchoAgentBuild:read_file".to_owned().try_into().unwrap();
    assert_eq!(id.as_str(), "EchoAgentBuild:read_file");
}

#[test]
fn server_id_accepts_arbitrary_non_empty_strings() {
    for ok in ["my-uuid-v7", "srv_42", "x", "abc.def"] {
        let s = ServerId::new(ok).unwrap();
        assert_eq!(s.as_str(), ok);
    }
}

#[test]
fn server_id_rejects_empty() {
    assert_eq!(ServerId::new("").unwrap_err(), IdError::Empty);
}

#[test]
fn server_id_rejects_reserved_auto_prefix() {
    for bad in ["auto:my-server", "auto:", "auto:tool:read_file"] {
        let err = ServerId::new(bad).unwrap_err();
        assert!(
            matches!(err, IdError::ReservedPrefix { ref value } if value == bad),
            "expected ReservedPrefix for {bad:?}, got {err:?}"
        );
    }
}

#[test]
fn server_id_synthesis_starts_with_auto_prefix() {
    let conn = ConnectionId::new("conn-abc").unwrap();
    let bare = ToolId::new("read_file").unwrap();
    let synth = ServerId::synthesize_for_tool(&conn, &bare);
    assert_eq!(synth.as_str(), "auto:tool:read_file");

    let namespaced = ToolId::new("EchoAgentBuild:read_file").unwrap();
    let synth_ns = ServerId::synthesize_for_tool(&conn, &namespaced);
    assert_eq!(synth_ns.as_str(), "auto:tool:EchoAgentBuild:read_file");
}

#[test]
fn server_id_synthesis_is_deterministic() {
    let conn = ConnectionId::new("conn-abc").unwrap();
    let tool = ToolId::new("EchoAgentBuild:read_file").unwrap();
    let a = ServerId::synthesize_for_tool(&conn, &tool);
    let b = ServerId::synthesize_for_tool(&conn, &tool);
    assert_eq!(
        a, b,
        "synthesis must be a pure function of (connection, tool)"
    );
}

#[test]
fn server_id_synthesis_bypasses_reserved_prefix_check() {
    // The synthesised id starts with `auto:`; the reserved-prefix rule
    // only applies to client-supplied values via `ServerId::new`.
    let conn = ConnectionId::new("conn-abc").unwrap();
    let tool = ToolId::new("read_file").unwrap();
    let synth = ServerId::synthesize_for_tool(&conn, &tool);
    assert!(synth.as_str().starts_with("auto:"));

    let err = ServerId::new(synth.as_str()).unwrap_err();
    assert!(matches!(err, IdError::ReservedPrefix { .. }));
}

#[test]
fn opaque_ids_reject_empty() {
    assert_eq!(SessionId::new("").unwrap_err(), IdError::Empty);
    assert_eq!(UserId::new("").unwrap_err(), IdError::Empty);
    assert_eq!(ConnectionId::new("").unwrap_err(), IdError::Empty);
    assert_eq!(RequestId::new("").unwrap_err(), IdError::Empty);
    assert_eq!(ToolCallId::new("").unwrap_err(), IdError::Empty);
}

#[test]
fn opaque_ids_accept_arbitrary_non_empty_strings() {
    assert_eq!(
        SessionId::new("anything goes").unwrap().as_str(),
        "anything goes"
    );
    assert_eq!(
        UserId::new("alice@example.com").unwrap().as_str(),
        "alice@example.com"
    );
    assert_eq!(RequestId::new("req-9c4f").unwrap().as_str(), "req-9c4f");
}

#[test]
fn tool_call_id_uuid_v7_helper_is_unique_and_valid_uuid() {
    let a = ToolCallId::new_v7();
    let b = ToolCallId::new_v7();
    assert_ne!(a, b, "two consecutive v7 ids must differ");
    for id in [&a, &b] {
        let parsed = uuid::Uuid::parse_str(id.as_str()).expect("parse uuid");
        assert_eq!(
            parsed.get_version_num(),
            7,
            "expected UUID v7, got {parsed}"
        );
    }
}

#[test]
fn opaque_id_display_matches_inner_string() {
    let s = SessionId::new("sess_abc").unwrap();
    assert_eq!(format!("{s}"), "sess_abc");
    let t = ToolId::new("github:list_repos").unwrap();
    assert_eq!(format!("{t}"), "github:list_repos");
}
