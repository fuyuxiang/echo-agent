use super::*;
use crate::session::events::ToolOutcome;
use echo_agent_tool_protocol::session_event::ToolCallOutcome;
use echo_agent_tool_protocol::turn_hook::TurnHookOutcome;
#[test]
fn map_tool_outcome_success() {
    assert_eq!(
        map_tool_outcome(ToolOutcome::Success),
        ToolCallOutcome::Success
    );
}
#[test]
fn map_tool_outcome_errors() {
    assert_eq!(map_tool_outcome(ToolOutcome::Error), ToolCallOutcome::Error);
    assert_eq!(
        map_tool_outcome(ToolOutcome::InvalidTool),
        ToolCallOutcome::Error
    );
}
#[test]
fn map_tool_outcome_cancellations() {
    for variant in [
        ToolOutcome::PermissionRejected,
        ToolOutcome::PermissionCancelled,
        ToolOutcome::Followup,
        ToolOutcome::HookDenied,
        ToolOutcome::Cancelled,
    ] {
        assert_eq!(
            map_tool_outcome(variant),
            ToolCallOutcome::Cancelled,
            "expected Cancelled for {variant:?}",
        );
    }
}
#[test]
fn turn_result_completed() {
    let result: Result<TurnOutcome, acp::Error> = Ok(TurnOutcome::Completed {
        snapshot: Box::new(None),
        tools_called: vec![],
        structured_output: None,
        refusal: None,
    });
    assert_eq!(
        turn_result_to_hook_outcome(&result),
        TurnHookOutcome::Completed
    );
}
#[test]
fn turn_result_cancelled() {
    let result: Result<TurnOutcome, acp::Error> = Ok(TurnOutcome::Cancelled {
        category: None,
        context: None,
    });
    assert_eq!(
        turn_result_to_hook_outcome(&result),
        TurnHookOutcome::Cancelled
    );
}
#[test]
fn turn_result_stationarity_ended_is_completed() {
    let result: Result<TurnOutcome, acp::Error> = Ok(TurnOutcome::StationarityEnded {
        snapshot: Box::new(None),
    });
    assert_eq!(
        turn_result_to_hook_outcome(&result),
        TurnHookOutcome::Completed
    );
}
#[test]
fn turn_result_error() {
    let result: Result<TurnOutcome, acp::Error> = Err(acp::Error::internal_error());
    assert_eq!(turn_result_to_hook_outcome(&result), TurnHookOutcome::Error);
}
#[test]
fn is_remote_image_url_classifies_schemes() {
    assert!(is_remote_image_url("https://example.com/x.png"));
    assert!(is_remote_image_url("http://example.com/x.png"));
    assert!(!is_remote_image_url("file:///Users/me/x.png"));
    assert!(!is_remote_image_url("data:image/png;base64,AAAA"));
    assert!(!is_remote_image_url(""));
    assert!(!is_remote_image_url("FILE:///Users/me/x.png"));
}
#[test]
fn pick_image_url_prefers_base64_over_file_uri() {
    let img = agent_client_protocol::ImageContent::new("AAAA", "image/png")
        .uri(Some("file:///Users/me/Downloads/screenshot.png".into()));
    assert_eq!(pick_user_image_url(&img), "data:image/png;base64,AAAA");
}
#[test]
fn pick_image_url_prefers_base64_when_https_uri_also_present() {
    let img = agent_client_protocol::ImageContent::new("BBBB", "image/jpeg")
        .uri(Some("https://example.com/x.jpg".into()));
    assert_eq!(pick_user_image_url(&img), "data:image/jpeg;base64,BBBB");
}
#[test]
fn pick_image_url_falls_back_to_https_uri_when_data_empty() {
    let img = agent_client_protocol::ImageContent::new(String::new(), "image/png")
        .uri(Some("https://example.com/x.png".into()));
    assert_eq!(pick_user_image_url(&img), "https://example.com/x.png");
}
#[test]
fn pick_image_url_ignores_file_uri_when_data_empty() {
    let img = agent_client_protocol::ImageContent::new(String::new(), "image/png")
        .uri(Some("file:///Users/me/missing.png".into()));
    assert_eq!(pick_user_image_url(&img), "data:image/png;base64,");
}
