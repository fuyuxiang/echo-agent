use super::*;

fn summary_with_kind(kind: Option<&str>) -> Summary {
    Summary {
        session_kind: kind.map(String::from),
        hidden: None,
        ..Summary::new(
            &Info {
                id: acp::SessionId::new("test"),
                cwd: "/tmp".into(),
            },
            default_model_id(),
        )
        .unwrap()
    }
}

#[test]
fn summary_round_trips_and_defaults_reasoning_effort() {
    let mut s = summary_with_kind(None);
    s.reasoning_effort = None;
    let json = serde_json::to_string(&s).unwrap();
    assert!(
        !json.contains("reasoning_effort"),
        "a None effort must not be serialized"
    );
    let back: Summary = serde_json::from_str(&json).unwrap();
    assert_eq!(back.reasoning_effort, None);

    s.reasoning_effort = Some(ReasoningEffort::Xhigh);
    let json = serde_json::to_string(&s).unwrap();
    let back: Summary = serde_json::from_str(&json).unwrap();
    assert_eq!(back.reasoning_effort, Some(ReasoningEffort::Xhigh));
}

#[test]
fn hidden_for_all_subagent_kinds() {
    for kind in ["subagent", "subagent_fork", "subagent_resume"] {
        assert!(
            summary_with_kind(Some(kind)).is_hidden(),
            "{kind} should be hidden"
        );
    }
}

#[test]
fn not_hidden_for_regular_sessions() {
    assert!(!summary_with_kind(None).is_hidden());
    assert!(!summary_with_kind(Some("fork")).is_hidden());
    assert!(!summary_with_kind(Some("worktree")).is_hidden());
}

#[test]
fn explicit_hidden_overrides_session_kind() {
    let mut s = summary_with_kind(Some("subagent"));
    s.hidden = Some(false);
    assert!(!s.is_hidden(), "explicit hidden=false overrides kind");

    let mut s = summary_with_kind(None);
    s.hidden = Some(true);
    assert!(s.is_hidden(), "explicit hidden=true overrides kind");
}
