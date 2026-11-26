use super::*;
use echo_agent_chat_proxy_types::feedback_types::{
    ClientType, FeedbackSubmission, FeedbackType, RatingType,
};

fn make_submission(thumbs_up: bool) -> FeedbackSubmission {
    FeedbackSubmission {
        session_id: "session-abc".into(),
        user_id: None,
        client_type: ClientType::Tui,
        feedback_type: if thumbs_up {
            FeedbackType::Rating
        } else {
            FeedbackType::RatingWithText
        },
        turn_number: Some(7),
        rating_type: Some(RatingType::Thumbs),
        rating_value: Some(if thumbs_up { 1 } else { -1 }),
        feedback_text: if thumbs_up {
            None
        } else {
            Some("could be better".into())
        },
        model_id: Some("echo-agent-3-fast".into()),
        resolved_model_id: Some("echo-agent-4.5".into()),
        ..Default::default()
    }
}

#[test]
fn test_user_feedback_spontaneous_roundtrip() {
    let entry = LocalFeedbackEntry::UserFeedback(UserFeedbackEntry {
        submitted_at: chrono::Utc::now(),
        session_id: "session-abc".into(),
        turn_number: Some(7),
        solicited: false,
        request_id: None,
        dismissed: false,
        submission: Some(make_submission(true)),
    });

    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains(r#""type":"user_feedback""#));
    assert!(!json.contains("dismissed")); // skip_serializing_if = is_false
    assert!(!json.contains("requestId")); // skip_serializing_if = Option::is_none

    let parsed: LocalFeedbackEntry = serde_json::from_str(&json).unwrap();
    let LocalFeedbackEntry::UserFeedback(ref uf) = parsed;
    assert!(!uf.solicited);
    assert!(!uf.dismissed);
    assert!(uf.submission.is_some());
    assert_eq!(uf.session_id, "session-abc");
}

#[test]
fn test_user_feedback_solicited_roundtrip() {
    let entry = LocalFeedbackEntry::UserFeedback(UserFeedbackEntry {
        submitted_at: chrono::Utc::now(),
        session_id: "session-abc".into(),
        turn_number: Some(14),
        solicited: true,
        request_id: Some("req-123".into()),
        dismissed: false,
        submission: Some(make_submission(false)),
    });

    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains(r#""requestId":"req-123""#));
    assert!(json.contains(r#""solicited":true"#));

    let parsed: LocalFeedbackEntry = serde_json::from_str(&json).unwrap();
    let LocalFeedbackEntry::UserFeedback(ref uf) = parsed;
    assert!(uf.solicited);
    assert_eq!(uf.request_id.as_deref(), Some("req-123"));
    let sub = uf.submission.as_ref().unwrap();
    assert_eq!(sub.feedback_text.as_deref(), Some("could be better"));
}

#[test]
fn test_user_feedback_dismiss_roundtrip() {
    let entry = LocalFeedbackEntry::UserFeedback(UserFeedbackEntry {
        submitted_at: chrono::Utc::now(),
        session_id: "session-abc".into(),
        turn_number: None,
        solicited: true,
        request_id: Some("req-456".into()),
        dismissed: true,
        submission: None,
    });

    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains(r#""dismissed":true"#));
    assert!(!json.contains("submission")); // skip_serializing_if = Option::is_none

    let parsed: LocalFeedbackEntry = serde_json::from_str(&json).unwrap();
    let LocalFeedbackEntry::UserFeedback(ref uf) = parsed;
    assert!(uf.dismissed);
    assert!(uf.submission.is_none());
}

#[test]
fn test_feedback_jsonl_multi_line_roundtrip() {
    // Simulate multiple entries written to a JSONL file
    let entries = vec![
        LocalFeedbackEntry::UserFeedback(UserFeedbackEntry {
            submitted_at: chrono::Utc::now(),
            session_id: "s1".into(),
            turn_number: Some(1),
            solicited: false,
            request_id: None,
            dismissed: false,
            submission: Some(make_submission(true)),
        }),
        LocalFeedbackEntry::UserFeedback(UserFeedbackEntry {
            submitted_at: chrono::Utc::now(),
            session_id: "s1".into(),
            turn_number: None,
            solicited: true,
            request_id: Some("req-1".into()),
            dismissed: true,
            submission: None,
        }),
    ];

    // Serialize to JSONL
    let mut jsonl = String::new();
    for entry in &entries {
        let line = serde_json::to_string(entry).unwrap();
        jsonl.push_str(&line);
        jsonl.push('\n');
    }

    // Deserialize each line
    let parsed: Vec<LocalFeedbackEntry> = jsonl
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert_eq!(parsed.len(), 2);
    assert!(matches!(parsed[0], LocalFeedbackEntry::UserFeedback(_)));
    assert!(matches!(parsed[1], LocalFeedbackEntry::UserFeedback(_)));

    // Verify the dismiss entry
    let LocalFeedbackEntry::UserFeedback(ref uf) = parsed[1];
    assert!(uf.dismissed);
    assert!(uf.solicited);
}
