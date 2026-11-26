use super::{is_version_mismatch_banner, version_mismatch_banner};
use crate::glyphs::sanitize_toast_message;

fn expected_banner(client: &str, leader: &str) -> String {
    sanitize_toast_message(&format!(
        "⚠ Version mismatch: client {client}, leader {leader}. Restart echoagent to match"
    ))
    .into_owned()
}

#[test]
fn formats_both_versions_and_ignores_wire_message() {
    assert_eq!(
        version_mismatch_banner(
            r#"{"clientVersion":"0.1.157","leaderVersion":"0.1.150","message":"ignore me"}"#
        ),
        Some(expected_banner("0.1.157", "0.1.150"))
    );
}

#[test]
fn formats_without_message_field() {
    assert_eq!(
        version_mismatch_banner(r#"{"clientVersion":"0.2.1","leaderVersion":"0.2.0"}"#),
        Some(expected_banner("0.2.1", "0.2.0"))
    );
}

#[test]
fn rejects_unusable_payloads() {
    for params in [
        "{}",
        r#"{"message":"only a message\nwith\nnewlines"}"#,
        r#"{"clientVersion":"0.1.157"}"#,
        r#"{"leaderVersion":"0.1.150"}"#,
        r#"{"clientVersion":"","leaderVersion":"0.1.150"}"#,
        r#"{"clientVersion":"0.1.157","leaderVersion":""}"#,
        r#"{"clientVersion":"\n\t","leaderVersion":"0.1.150"}"#,
        r#"{"clientVersion":"   ","leaderVersion":"0.1.150"}"#,
        r#"{"clientVersion":"0.1.157","leaderVersion":"\n\t"}"#,
        r#"{"clientVersion":1,"leaderVersion":"0.1.150"}"#,
        r#""not-an-object""#,
        "null",
        "",
        "[]",
    ] {
        assert_eq!(version_mismatch_banner(params), None, "{params}");
    }
}

#[test]
fn scrubs_control_chars_in_versions() {
    let text = version_mismatch_banner(
        r#"{"clientVersion":"0.1.157\n\u0007x","leaderVersion":"0.1.150\r\n"}"#,
    )
    .expect("toastable after scrub");
    assert_eq!(text, expected_banner("0.1.157  x", "0.1.150  "));
    assert!(
        !text.chars().any(char::is_control),
        "control chars must not reach toast: {text:?}"
    );
}

#[test]
fn full_banner_matches_sanitize_toast_message() {
    let text = version_mismatch_banner(r#"{"clientVersion":"0.1.157","leaderVersion":"0.1.150"}"#)
        .expect("banner");
    assert_eq!(text, expected_banner("0.1.157", "0.1.150"));
    assert!(
        is_version_mismatch_banner(&text),
        "marker must survive glyph fallback: {text:?}"
    );
    assert!(is_version_mismatch_banner(
        "! Version mismatch: client x, leader y"
    ));
}
