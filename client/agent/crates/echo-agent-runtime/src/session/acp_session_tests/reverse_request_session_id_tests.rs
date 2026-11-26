//! Regression guard: every blocking reverse-request
//! (permission / `ask_user_question` / plan-approval) must carry a
//! non-empty `sessionId`, otherwise Tier-2 routing silently drops it
//! (`server.rs`). The invariant holds today; these tests pin it.
use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
    AskUserQuestionExtRequest, AskUserQuestionMode,
};
use echo_agent_tools::implementations::echo_agent_build::exit_plan_mode::ExitPlanModeExtRequest;

#[test]
fn ask_user_question_request_carries_session_id() {
    let req = AskUserQuestionExtRequest {
        session_id: "sess-abc".to_string(),
        tool_call_id: "call-1".to_string(),
        questions: vec![],
        mode: AskUserQuestionMode::Default,
        timeout_secs: None,
    };
    assert!(!req.session_id.is_empty());
    // Wire format is camelCase (`sessionId`); Tier-2 routing reads it.
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["sessionId"], "sess-abc");
    assert!(!json["sessionId"].as_str().unwrap().is_empty());
}

#[test]
fn exit_plan_mode_request_carries_session_id() {
    let req = ExitPlanModeExtRequest {
        session_id: "sess-xyz".to_string(),
        tool_call_id: "call-2".to_string(),
        plan_content: Some("plan".to_string()),
    };
    assert!(!req.session_id.is_empty());
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["sessionId"], "sess-xyz");
    assert!(!json["sessionId"].as_str().unwrap().is_empty());
}
