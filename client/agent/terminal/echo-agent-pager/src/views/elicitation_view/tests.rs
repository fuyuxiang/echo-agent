use echo_agent_tools::mcp_elicitation::{
    ElicitFieldKind, McpElicitExtRequest, McpElicitExtResponse, McpElicitModeFields,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;

use super::render::form_value_column;
use super::state::check_elicit_url;
use super::*;
use crate::theme::Theme;

fn form_req() -> McpElicitExtRequest {
    McpElicitExtRequest {
        session_id: "s".into(),
        tool_call_id: "mcp-elicit-1".into(),
        server_name: "demo".into(),
        message: "Fill in".into(),
        mode: McpElicitModeFields::Form {
            requested_schema: Some(json!({
                "type": "object",
                "properties": {
                    "email": { "type": "string", "format": "email" },
                    "ok": { "type": "boolean" }
                },
                "required": ["email"]
            })),
        },
    }
}

fn url_req(url: &str) -> McpElicitExtRequest {
    McpElicitExtRequest {
        session_id: "s".into(),
        tool_call_id: "mcp-elicit-2".into(),
        server_name: "oauth".into(),
        message: "Log in".into(),
        mode: McpElicitModeFields::Url {
            url: url.into(),
            elicitation_id: "eid-1".into(),
        },
    }
}

fn set_draft(state: &mut ElicitationViewState, idx: usize, draft: &str) {
    let form = state.form_mut().expect("form stage");
    form.fields[idx].set_draft(draft);
}

fn buffer_text(buf: &Buffer) -> String {
    let area = *buf.area();
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}

fn render_to_text(state: &mut ElicitationViewState, w: u16, h: u16) -> String {
    let theme = Theme::default();
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    render_elicitation_view(&mut buf, area, state, &theme, true, None);
    buffer_text(&buf)
}

#[test]
fn form_accept_requires_email() {
    let mut state = ElicitationViewState::from_request(form_req(), None, None);
    assert!(state.try_accept().is_none());
    assert!(
        state.form().unwrap().fields[0].error.is_some(),
        "failed accept must set the field error"
    );
    set_draft(&mut state, 0, "a@b.com");
    let resp = state.try_accept().expect("accept");
    match resp {
        McpElicitExtResponse::Accept { content } => {
            assert_eq!(content.unwrap()["email"], "a@b.com");
        }
        _ => panic!("expected accept"),
    }
}

#[test]
fn title_includes_server() {
    let state = ElicitationViewState::from_request(form_req(), None, None);
    assert!(state.title().contains("demo"));
}

#[test]
fn server_escapes_never_reach_the_buffer() {
    let mut req = form_req();
    req.message = "read\x1b]52;c;c3RvbGVu\x07this".into();
    req.mode = McpElicitModeFields::Form {
        requested_schema: Some(json!({
            "type": "object",
            "properties": {
                "choice": {
                    "type": "string",
                    "title": "Ti\x1b[31mtle",
                    "description": "de\x1bsc",
                    "default": "dr\x07aft",
                    "enum": ["o\x1b]52;c;x\x07k", "no"]
                }
            }
        })),
    };
    let mut state = ElicitationViewState::from_request(req, None, None);
    // Select the enum value so its label is painted too.
    state.toggle_bool_or_enum();
    let theme = Theme::default();
    let area = Rect::new(0, 0, 80, 16);
    let mut buf = Buffer::empty(area);
    render_elicitation_view(&mut buf, area, &mut state, &theme, true, None);
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                assert!(
                    !cell.symbol().chars().any(char::is_control),
                    "control char leaked into cell ({x},{y}): {:?}",
                    cell.symbol()
                );
            }
        }
    }
    assert_eq!(state.message, "read]52;c;c3RvbGVuthis");
    let field = &state.form().unwrap().fields[0];
    assert_eq!(field.spec.title, "Ti[31mtle");
    let ElicitFieldKind::SingleSelect { ref options, .. } = field.spec.kind else {
        panic!("expected single-select field");
    };
    assert_eq!(options[0].label, "o]52;c;xk");
}

#[test]
fn append_char_rejects_control_and_unsafe_chars() {
    let mut state = ElicitationViewState::from_request(form_req(), None, None);
    for c in "a\x1b@\u{202E}b\r".chars() {
        state.append_char(c);
    }
    assert_eq!(state.form().unwrap().fields[0].draft(), "a@b");
}

fn two_field_req() -> McpElicitExtRequest {
    let mut req = form_req();
    req.mode = McpElicitModeFields::Form {
        requested_schema: Some(json!({
            "type": "object",
            "properties": {
                "email": { "type": "string" },
                "name": { "type": "string" }
            }
        })),
    };
    req
}

fn zero_field_req() -> McpElicitExtRequest {
    let mut req = form_req();
    req.mode = McpElicitModeFields::Form {
        requested_schema: Some(json!({ "type": "object", "properties": {} })),
    };
    req
}

#[track_caller]
fn assert_focus(
    state: &ElicitationViewState,
    focus: ElicitationFocus,
    cursor: usize,
    action: ElicitationActionFocus,
) {
    assert_eq!(
        (state.focus, state.field_cursor(), state.action_focus),
        (focus, cursor, action)
    );
}

#[test]
fn move_focus_forward_wrap_walks_fields_then_actions() {
    use ElicitationActionFocus::{Accept, Decline};
    let mut state = ElicitationViewState::from_request(two_field_req(), None, None);
    state.move_focus(/*forward*/ true, /*wrap*/ true);
    assert_focus(&state, ElicitationFocus::Fields, 1, Accept);
    state.move_focus(true, true);
    assert_focus(&state, ElicitationFocus::Actions, 1, Accept);
    state.move_focus(true, true);
    assert_focus(&state, ElicitationFocus::Actions, 1, Decline);
    state.move_focus(true, true);
    assert_focus(&state, ElicitationFocus::Fields, 0, Decline);
}

#[test]
fn move_focus_backward_wrap_walks_actions_then_fields() {
    use ElicitationActionFocus::{Accept, Decline};
    let mut state = ElicitationViewState::from_request(two_field_req(), None, None);
    state.move_focus(/*forward*/ false, /*wrap*/ true);
    assert_focus(&state, ElicitationFocus::Actions, 0, Decline);
    state.move_focus(false, true);
    assert_focus(&state, ElicitationFocus::Actions, 0, Accept);
    state.move_focus(false, true);
    assert_focus(&state, ElicitationFocus::Fields, 1, Accept);
    state.move_focus(false, true);
    assert_focus(&state, ElicitationFocus::Fields, 0, Accept);
}

#[test]
fn move_focus_forward_no_wrap_stops_on_decline() {
    use ElicitationActionFocus::{Accept, Decline};
    let mut state = ElicitationViewState::from_request(two_field_req(), None, None);
    state.move_focus(/*forward*/ true, /*wrap*/ false);
    assert_focus(&state, ElicitationFocus::Fields, 1, Accept);
    state.move_focus(true, false);
    assert_focus(&state, ElicitationFocus::Actions, 1, Accept);
    state.move_focus(true, false);
    assert_focus(&state, ElicitationFocus::Actions, 1, Decline);
    state.move_focus(true, false);
    assert_focus(&state, ElicitationFocus::Actions, 1, Decline);
}

#[test]
fn move_focus_backward_no_wrap_stops_on_first_field() {
    use ElicitationActionFocus::Accept;
    let mut state = ElicitationViewState::from_request(two_field_req(), None, None);
    state.focus = ElicitationFocus::Actions;
    state.move_focus(/*forward*/ false, /*wrap*/ false);
    assert_focus(&state, ElicitationFocus::Fields, 1, Accept);
    state.move_focus(false, false);
    assert_focus(&state, ElicitationFocus::Fields, 0, Accept);
    state.move_focus(false, false);
    assert_focus(&state, ElicitationFocus::Fields, 0, Accept);
}

#[test]
fn move_focus_zero_field_form_toggles_actions() {
    use ElicitationActionFocus::{Accept, Decline};
    let mut state = ElicitationViewState::from_request(zero_field_req(), None, None);
    assert_eq!(state.focus, ElicitationFocus::Actions);
    state.move_focus(/*forward*/ true, /*wrap*/ true);
    assert_focus(&state, ElicitationFocus::Actions, 0, Decline);
    state.move_focus(true, true);
    assert_focus(&state, ElicitationFocus::Actions, 0, Accept);
    // Non-wrap backward on Accept with no fields stays put.
    state.move_focus(false, false);
    assert_focus(&state, ElicitationFocus::Actions, 0, Accept);
    state.move_focus(false, true);
    assert_focus(&state, ElicitationFocus::Actions, 0, Decline);
}

#[test]
fn value_column_caps_on_wide_content() {
    let state = ElicitationViewState::from_request(form_req(), None, None);
    let fields = &state.form().unwrap().fields;
    let col = form_value_column(fields, 200);
    assert!(
        col <= 36,
        "wide terminal must not pin values past 36, got {col}"
    );
    assert!(col > 2, "values should still sit to the right of labels");
}

#[test]
fn value_column_caps_survive_long_titles() {
    let mut req = form_req();
    req.mode = McpElicitModeFields::Form {
        requested_schema: Some(json!({
            "type": "object",
            "properties": {
                "a": { "type": "string", "title": "T".repeat(100) }
            }
        })),
    };
    let state = ElicitationViewState::from_request(req, None, None);
    let fields = &state.form().unwrap().fields;
    assert!(form_value_column(fields, 200) <= 36);
    assert!(form_value_column(fields, 20) <= 19);
}

// ── viewport ────────────────────────────────────────────────────────────────

fn many_field_req(n: usize) -> McpElicitExtRequest {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for i in 0..n {
        properties.insert(format!("field{i:02}"), json!({ "type": "string" }));
        required.push(format!("field{i:02}"));
    }
    let mut req = form_req();
    req.mode = McpElicitModeFields::Form {
        requested_schema: Some(json!({
            "type": "object",
            "properties": properties,
            "required": required
        })),
    };
    req
}

#[test]
fn tall_form_keeps_actions_visible_and_follows_cursor() {
    let mut state = ElicitationViewState::from_request(many_field_req(20), None, None);
    let screen_h = 24u16;
    let h = elicitation_view_height(&state, screen_h, 75);
    assert!(h <= screen_h, "card must not exceed the screen");

    // Cursor on the first field: early fields visible, actions pinned.
    let text = render_to_text(&mut state, 80, h);
    assert!(text.contains("field00"), "first field visible:\n{text}");
    assert!(text.contains("Accept"), "Accept pinned:\n{text}");
    assert!(text.contains("Decline"), "Decline pinned:\n{text}");
    assert!(
        !text.contains("field19"),
        "last field must be clipped at this height:\n{text}"
    );

    // Walk the cursor to the last field: the viewport follows.
    if let Some(form) = state.form_mut() {
        form.field_cursor = 19;
    }
    let text = render_to_text(&mut state, 80, h);
    assert!(
        text.contains("field19"),
        "cursor field scrolled into view:\n{text}"
    );
    assert!(text.contains("Accept"), "Accept still pinned:\n{text}");
    assert!(text.contains("↑ more"), "clipped-above cue:\n{text}");
}

#[test]
fn failed_accept_error_is_visible_on_clipped_form() {
    let mut state = ElicitationViewState::from_request(many_field_req(20), None, None);
    // Everything is required and empty: accept fails and every field errors.
    assert!(state.try_accept().is_none());
    if let Some(form) = state.form_mut() {
        form.field_cursor = 10;
    }
    let h = elicitation_view_height(&state, 24, 75);
    let text = render_to_text(&mut state, 80, h);
    assert!(text.contains("field10"), "cursor field visible:\n{text}");
    assert!(text.contains("required"), "its error row visible:\n{text}");
}

// ── URL mode ───────────────────────────────────────────────────────────────

#[test]
fn url_check_rejects_unsafe_and_malformed() {
    assert!(check_elicit_url("https://example.com/cb").is_ok());
    assert!(check_elicit_url("http://example.com").is_ok());
    for bad in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "data:text/html,x",
        "not a url",
        "https://user:pw@example.com/",
    ] {
        assert!(check_elicit_url(bad).is_err(), "{bad} must be rejected");
    }
}

#[test]
fn url_check_flags_punycode_hosts() {
    let display = check_elicit_url("https://xn--80ak6aa92e.com/login").unwrap();
    assert!(display.punycode_host);
    // The url crate normalizes Unicode hosts to Punycode, so spoofable
    // Unicode domains render in their Punycode form and get flagged.
    let display = check_elicit_url("https://аррӏе.com/login").unwrap();
    assert!(display.punycode_host, "IDN host must normalize + flag");
}

#[test]
fn unsafe_url_disables_accept() {
    let mut state = ElicitationViewState::from_request(url_req("javascript:alert(1)"), None, None);
    assert!(state.banner_error().is_some());
    assert!(
        state.try_accept().is_none(),
        "a rejected URL must never produce an Accept response"
    );
    // Decline/Cancel still resolve the request.
    assert!(matches!(
        ElicitationViewState::decline_response(),
        McpElicitExtResponse::Decline
    ));
}

#[test]
fn url_consent_shows_host_and_full_url() {
    let long_path = "a/".repeat(120);
    let url = format!("https://sub.example.com/{long_path}?token=tail-end-marker");
    let mut state = ElicitationViewState::from_request(url_req(&url), None, None);
    assert!(state.banner_error().is_none());
    let h = elicitation_view_height(&state, 60, 75);
    let text = render_to_text(&mut state, 80, h);
    assert!(text.contains("Host: "), "host emphasized:\n{text}");
    assert!(text.contains("sub.example.com"), "host shown:\n{text}");
    assert!(
        text.contains("tail-end-marker"),
        "the URL tail must be shown, not a trusted-looking prefix:\n{text}"
    );
}

#[test]
fn url_tail_reachable_by_scrolling_on_short_terminal() {
    let url = format!("https://example.com/{}?tail-end-marker", "a/".repeat(400));
    let mut state = ElicitationViewState::from_request(url_req(&url), None, None);
    let h = elicitation_view_height(&state, 14, 75);
    let text = render_to_text(&mut state, 80, h);
    assert!(
        !text.contains("tail-end-marker"),
        "precondition: the tail is clipped on a short terminal:\n{text}"
    );
    // Scroll to the bottom (render clamps to max scroll).
    state.scroll = usize::MAX;
    let text = render_to_text(&mut state, 80, h);
    assert!(
        text.contains("tail-end-marker"),
        "scrolling must reach the URL tail before consent:\n{text}"
    );
}

#[test]
fn long_text_draft_is_reviewable_in_full() {
    let mut state = ElicitationViewState::from_request(form_req(), None, None);
    let long = format!("{}tail-marker", "x".repeat(120));
    set_draft(&mut state, 0, &long);
    let h = elicitation_view_height(&state, 40, 75);
    let text = render_to_text(&mut state, 80, h);
    assert!(
        text.contains("tail-marker"),
        "the focused field's full value must be reviewable, not just the \
         truncated value cell:\n{text}"
    );
}

#[test]
fn url_accept_transitions_to_waiting_and_keeps_id() {
    let mut state =
        ElicitationViewState::from_request(url_req("https://example.com/cb"), None, None);
    let resp = state.try_accept().expect("valid URL accepts");
    assert!(matches!(
        resp,
        McpElicitExtResponse::Accept { content: None }
    ));
    // No live responder in this fixture: delivery reports false, and the
    // caller (the key handler) would dismiss instead of entering waiting.
    assert!(!state.send_response(resp));
    state.begin_url_waiting();
    assert!(state.is_url_waiting());
    assert_eq!(state.elicitation_id(), Some("eid-1"));
    assert_eq!(state.url(), Some("https://example.com/cb"));
    // Nothing left to accept or send.
    assert!(state.try_accept().is_none());
    assert!(state.take_response_tx().is_none());
}

// ── multi-select ───────────────────────────────────────────────────────────

fn multi_select_req() -> McpElicitExtRequest {
    let mut req = form_req();
    req.mode = McpElicitModeFields::Form {
        requested_schema: Some(json!({
            "type": "object",
            "properties": {
                "countries": {
                    "type": "array",
                    "items": {
                        "anyOf": [
                            { "const": "us", "title": "United States" },
                            { "const": "uk", "title": "United Kingdom" },
                            { "const": "de", "title": "Germany" }
                        ]
                    },
                    "minItems": 1
                }
            },
            "required": ["countries"]
        })),
    };
    req
}

#[test]
fn multi_select_toggles_and_submits_array() {
    let mut state = ElicitationViewState::from_request(multi_select_req(), None, None);
    assert!(state.enter_edit_or_options(), "multi-select expands");
    assert_eq!(state.focus, ElicitationFocus::Editing);
    state.toggle_current_option();
    assert!(state.move_option_cursor(1));
    state.move_option_cursor(1);
    state.toggle_current_option();
    let resp = state.try_accept().expect("two selections satisfy minItems");
    let McpElicitExtResponse::Accept { content } = resp else {
        panic!("expected accept");
    };
    assert_eq!(content.unwrap()["countries"], json!(["us", "de"]));
}

#[test]
fn multi_select_min_items_blocks_accept() {
    let mut state = ElicitationViewState::from_request(multi_select_req(), None, None);
    assert!(state.try_accept().is_none());
    let err = state.form().unwrap().fields[0].error.clone();
    assert_eq!(err.as_deref(), Some("select at least 1"));
}

#[test]
fn multi_select_options_render_when_expanded() {
    let mut state = ElicitationViewState::from_request(multi_select_req(), None, None);
    state.enter_edit_or_options();
    state.toggle_current_option();
    let h = elicitation_view_height(&state, 40, 75);
    let text = render_to_text(&mut state, 80, h);
    assert!(
        text.contains("[x] United States"),
        "toggled option:\n{text}"
    );
    assert!(text.contains("[ ] Germany"), "untoggled option:\n{text}");
}

// ── typed content ──────────────────────────────────────────────────────────

#[test]
fn integer_field_submits_lossless_i64() {
    let mut req = form_req();
    req.mode = McpElicitModeFields::Form {
        requested_schema: Some(json!({
            "type": "object",
            "properties": { "id": { "type": "integer" } },
            "required": ["id"]
        })),
    };
    let mut state = ElicitationViewState::from_request(req, None, None);
    set_draft(&mut state, 0, "9007199254740993");
    let McpElicitExtResponse::Accept { content } = state.try_accept().expect("accept") else {
        panic!("expected accept");
    };
    assert_eq!(content.unwrap()["id"], 9007199254740993_i64);

    set_draft(&mut state, 0, "1e20");
    assert!(state.try_accept().is_none(), "1e20 is not an integer");
}
