use agent_client_protocol as acp;
use echo_agent_acp::AcpResult;

pub use echo_agent_tools::implementations::echo_agent_build::exit_plan_mode::{
    ExitPlanModeExtRequest, ExitPlanModeExtResponse,
};

use crate::views::prompt_widget::StashedPrompt;

/// Placeholder body for the plan-approval preview when `exit_plan_mode` parks
/// with no plan content (missing/empty `plan.md`, or a whitespace-only body).
///
/// Must be non-empty after trim so `LineViewerState::open_markdown_content`
/// accepts it — empty bodies are rejected there.
pub const EMPTY_PLAN_PLACEHOLDER: &str = "\
# No plan written yet

The agent exited plan mode without writing a plan.

- **Approve**: leave plan mode and start implementing
- **Request changes**: send the agent back to planning
- **Quit**: abandon and turn plan mode off
";

/// Status-line label while plan approval is parked.
///
/// Empty plans use an active decision prompt instead of "Waiting…", so the
/// UI doesn't look stuck when there is no preview body to open.
pub fn plan_approval_status_label(has_plan: bool) -> &'static str {
    if has_plan {
        "Waiting on plan approval"
    } else {
        "No plan written: approve or request changes"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanApprovalFocus {
    Preview,
    Prompt,
    Commenting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanReviewSource {
    Inline,
    FileBacked,
}

#[derive(Debug, Clone)]
pub struct PlanComment {
    pub id: u64,
    pub line_range: std::ops::Range<usize>,
    pub text: String,
}

pub struct PlanApprovalViewState {
    pub tool_call_id: String,
    pub has_plan: bool,
    pub plan_content: Option<String>,
    pub source: PlanReviewSource,
    pub stashed_prompt: StashedPrompt,
    pub response_tx: Option<tokio::sync::oneshot::Sender<AcpResult<acp::ExtResponse>>>,

    pub focus: PlanApprovalFocus,
    pub comments: Vec<PlanComment>,
    pub next_comment_id: u64,
    pub editing_comment_id: Option<u64>,
    pub commenting_range: Option<std::ops::Range<usize>>,

    pub stashed_feedback_prompt: Option<StashedPrompt>,
}

impl PlanApprovalViewState {
    pub fn new(
        request: ExitPlanModeExtRequest,
        stashed_prompt: StashedPrompt,
        response_tx: tokio::sync::oneshot::Sender<AcpResult<acp::ExtResponse>>,
    ) -> Self {
        Self::with_source(
            request,
            PlanReviewSource::Inline,
            stashed_prompt,
            response_tx,
        )
    }

    pub fn with_source(
        request: ExitPlanModeExtRequest,
        source: PlanReviewSource,
        stashed_prompt: StashedPrompt,
        response_tx: tokio::sync::oneshot::Sender<AcpResult<acp::ExtResponse>>,
    ) -> Self {
        let plan_content = request.plan_content.filter(|s| !s.trim().is_empty());
        let has_plan = plan_content.is_some();
        Self {
            tool_call_id: request.tool_call_id,
            has_plan,
            plan_content,
            source,
            stashed_prompt,
            response_tx: Some(response_tx),
            focus: PlanApprovalFocus::Preview,
            comments: Vec::new(),
            next_comment_id: 0,
            editing_comment_id: None,
            commenting_range: None,
            stashed_feedback_prompt: None,
        }
    }

    pub fn format_feedback(&self, freeform: Option<&str>) -> String {
        let mut parts: Vec<String> = self
            .comments
            .iter()
            .map(|comment| match self.source {
                PlanReviewSource::Inline => {
                    let label = if comment.line_range.len() == 1 {
                        format!("Proposed plan line {}:", comment.line_range.start)
                    } else {
                        format!(
                            "Proposed plan lines {}-{}:",
                            comment.line_range.start,
                            comment.line_range.end - 1
                        )
                    };
                    let snippets =
                        inline_plan_snippets(self.plan_content.as_deref(), &comment.line_range);
                    format!("{label}\n{snippets}\n\nComment:\n{}", comment.text)
                }
                PlanReviewSource::FileBacked => format_file_backed_plan_comment(comment),
            })
            .collect();

        if let Some(text) = freeform
            && !text.trim().is_empty()
        {
            let text = match (self.source, self.comments.is_empty()) {
                (PlanReviewSource::Inline, false) => format!("Additional feedback:\n{text}"),
                _ => text.to_owned(),
            };
            parts.push(text);
        }

        parts.join("\n\n")
    }
}

pub fn send_exit_plan_response(
    tx: tokio::sync::oneshot::Sender<AcpResult<acp::ExtResponse>>,
    outcome: &str,
    feedback: Option<String>,
) {
    let feedback = feedback.filter(|f| !f.trim().is_empty());
    let resp = ExitPlanModeExtResponse {
        outcome: outcome.into(),
        feedback,
    };
    let raw = serde_json::value::to_raw_value(&resp)
        .expect("ExitPlanModeExtResponse serialization should not fail");
    tx.send(Ok(acp::ExtResponse::new(raw.into()))).ok();
}

fn send_ext_response(
    tx: &mut Option<tokio::sync::oneshot::Sender<AcpResult<acp::ExtResponse>>>,
    outcome: &str,
    feedback: Option<String>,
) -> bool {
    let Some(tx) = tx.take() else {
        return false;
    };
    send_exit_plan_response(tx, outcome, feedback);
    true
}

impl PlanApprovalViewState {
    pub fn send_approved(&mut self) -> bool {
        send_ext_response(&mut self.response_tx, "approved", None)
    }

    pub fn send_abandoned(&mut self) -> bool {
        send_ext_response(&mut self.response_tx, "abandoned", None)
    }

    pub fn send_cancelled(&mut self, feedback: Option<String>) -> bool {
        send_ext_response(&mut self.response_tx, "cancelled", feedback)
    }

    pub fn send_stale_cancel(&mut self) -> bool {
        self.send_cancelled(None)
    }
}

fn format_file_backed_plan_comment(comment: &PlanComment) -> String {
    let range = if comment.line_range.len() == 1 {
        format!("@plan.md:{}", comment.line_range.start)
    } else {
        format!(
            "@plan.md:{}-{}",
            comment.line_range.start,
            comment.line_range.end - 1
        )
    };
    format!("{range}\n{}", comment.text)
}

pub(crate) fn inline_plan_snippets(
    plan_content: Option<&str>,
    range: &std::ops::Range<usize>,
) -> String {
    let Some(plan_content) = plan_content else {
        return "> [plan content unavailable]".to_owned();
    };
    let lines: Vec<&str> = plan_content.lines().collect();
    if range.start == 0 || range.start >= range.end || range.start > lines.len() {
        return "> [selected lines unavailable]".to_owned();
    }

    let end = range.end.saturating_sub(1).min(lines.len());
    if end < range.start {
        return "> [selected lines unavailable]".to_owned();
    }

    lines[range.start - 1..end]
        .iter()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn format_plan_comments(comments: &[PlanComment], plan_content: Option<&str>) -> String {
    comments
        .iter()
        .map(|comment| {
            let label = if comment.line_range.len() == 1 {
                format!("Proposed plan line {}:", comment.line_range.start)
            } else {
                format!(
                    "Proposed plan lines {}-{}:",
                    comment.line_range.start,
                    comment.line_range.end - 1
                )
            };
            let snippets = inline_plan_snippets(plan_content, &comment.line_range);
            format!("{label}\n{snippets}\n\nComment:\n{}", comment.text)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_state() -> (
        PlanApprovalViewState,
        tokio::sync::oneshot::Receiver<AcpResult<acp::ExtResponse>>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let request = ExitPlanModeExtRequest {
            session_id: "test-session".into(),
            tool_call_id: "call_123".into(),
            plan_content: Some("# Plan\n\n## Step 1\nDo something".into()),
        };
        let state = PlanApprovalViewState::new(
            request,
            StashedPrompt {
                text: "stashed text".into(),
                cursor: 0,
                images: Vec::new(),
                chip_elements: Vec::new(),
                image_counter: 0,
                image_undo_stash: Vec::new(),
            },
            tx,
        );
        (state, rx)
    }

    #[test]
    fn test_send_approved() {
        let (mut state, mut rx) = make_test_state();
        assert!(state.send_approved());
        let resp = rx.try_recv().expect("should receive response");
        let raw = resp.expect("should be Ok");
        let parsed: serde_json::Value =
            serde_json::from_str(raw.0.get()).expect("should be valid JSON");
        assert_eq!(parsed["outcome"], "approved");
        assert!(parsed.get("feedback").is_none());
    }

    #[test]
    fn test_send_cancelled_with_feedback() {
        let (mut state, mut rx) = make_test_state();
        assert!(state.send_cancelled(Some("fix auth flow".into())));
        let resp = rx.try_recv().expect("should receive response");
        let raw = resp.expect("should be Ok");
        let parsed: serde_json::Value =
            serde_json::from_str(raw.0.get()).expect("should be valid JSON");
        assert_eq!(parsed["outcome"], "cancelled");
        assert_eq!(parsed["feedback"], "fix auth flow");
    }

    #[test]
    fn test_send_cancelled_without_feedback() {
        let (mut state, mut rx) = make_test_state();
        assert!(state.send_cancelled(None));
        let resp = rx.try_recv().expect("should receive response");
        let raw = resp.expect("should be Ok");
        let parsed: serde_json::Value =
            serde_json::from_str(raw.0.get()).expect("should be valid JSON");
        assert_eq!(parsed["outcome"], "cancelled");
        assert!(parsed.get("feedback").is_none());
    }

    #[test]
    fn test_send_cancelled_empty_feedback_is_none() {
        let (mut state, mut rx) = make_test_state();
        assert!(state.send_cancelled(Some("   ".into())));
        let resp = rx.try_recv().expect("should receive response");
        let raw = resp.expect("should be Ok");
        let parsed: serde_json::Value =
            serde_json::from_str(raw.0.get()).expect("should be valid JSON");
        assert_eq!(parsed["outcome"], "cancelled");
        assert!(parsed.get("feedback").is_none());
    }

    #[test]
    fn test_send_stale_cancel() {
        let (mut state, mut rx) = make_test_state();
        assert!(state.send_stale_cancel());
        let resp = rx.try_recv().expect("should receive response");
        let raw = resp.expect("should be Ok");
        let parsed: serde_json::Value =
            serde_json::from_str(raw.0.get()).expect("should be valid JSON");
        assert_eq!(parsed["outcome"], "cancelled");
        assert!(parsed.get("feedback").is_none());
    }

    #[test]
    fn test_double_send_returns_false() {
        let (mut state, _rx) = make_test_state();
        assert!(state.send_approved());
        assert!(!state.send_approved());
        assert!(!state.send_cancelled(None));
    }

    #[test]
    fn test_constructor_defaults() {
        let (state, _rx) = make_test_state();
        assert_eq!(state.tool_call_id, "call_123");
        assert!(state.has_plan);
        assert_eq!(
            state.plan_content.as_deref(),
            Some("# Plan\n\n## Step 1\nDo something")
        );
        assert_eq!(state.source, PlanReviewSource::Inline);
        assert_eq!(state.stashed_prompt.text, "stashed text");
        assert!(state.response_tx.is_some());
        assert_eq!(state.focus, PlanApprovalFocus::Preview);
        assert!(state.comments.is_empty());
        assert_eq!(state.next_comment_id, 0);
        assert!(state.editing_comment_id.is_none());
        assert!(state.commenting_range.is_none());
        assert!(state.stashed_feedback_prompt.is_none());
    }

    fn make_empty_plan_state() -> (
        PlanApprovalViewState,
        tokio::sync::oneshot::Receiver<AcpResult<acp::ExtResponse>>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let request = ExitPlanModeExtRequest {
            session_id: "test-session".into(),
            tool_call_id: "call_456".into(),
            plan_content: None,
        };
        let state = PlanApprovalViewState::new(
            request,
            StashedPrompt {
                text: "stashed".into(),
                cursor: 0,
                images: Vec::new(),
                chip_elements: Vec::new(),
                image_counter: 0,
                image_undo_stash: Vec::new(),
            },
            tx,
        );
        (state, rx)
    }

    #[test]
    fn test_empty_plan_has_plan_false() {
        let (state, _rx) = make_empty_plan_state();
        assert!(!state.has_plan);
        assert!(state.plan_content.is_none());
    }

    #[test]
    fn plan_approval_status_label_distinguishes_empty() {
        assert_eq!(plan_approval_status_label(true), "Waiting on plan approval");
        assert_eq!(
            plan_approval_status_label(false),
            "No plan written: approve or request changes"
        );
        // Placeholder must be non-empty so the line viewer accepts it.
        assert!(!EMPTY_PLAN_PLACEHOLDER.trim().is_empty());
    }

    #[test]
    fn test_empty_plan_whitespace_only() {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = ExitPlanModeExtRequest {
            session_id: "test-session".into(),
            tool_call_id: "call_789".into(),
            plan_content: Some("   \n\n  ".into()),
        };
        let state = PlanApprovalViewState::new(
            request,
            StashedPrompt {
                text: "stashed".into(),
                cursor: 0,
                images: Vec::new(),
                chip_elements: Vec::new(),
                image_counter: 0,
                image_undo_stash: Vec::new(),
            },
            tx,
        );
        assert!(!state.has_plan);
        assert!(state.plan_content.is_none());
    }

    #[test]
    fn inline_plan_feedback_quotes_selected_line_snippets() {
        let (mut state, _rx) = make_test_state();
        state.plan_content = Some("alpha\nbravo\ncharlie\ndelta".into());
        state.comments.push(PlanComment {
            id: 0,
            line_range: 2..3,
            text: "rewrite this".into(),
        });
        state.comments.push(PlanComment {
            id: 1,
            line_range: 3..5,
            text: "combine these".into(),
        });

        let feedback = state.format_feedback(Some("overall note"));

        assert_eq!(
            feedback,
            "Proposed plan line 2:\n> bravo\n\nComment:\nrewrite this\n\nProposed plan lines 3-4:\n> charlie\n> delta\n\nComment:\ncombine these\n\nAdditional feedback:\noverall note"
        );
    }

    #[test]
    fn inline_plan_feedback_handles_out_of_range_lines() {
        let (mut state, _rx) = make_test_state();
        state.plan_content = Some("alpha".into());
        state.comments.push(PlanComment {
            id: 0,
            line_range: 9..10,
            text: "where is this".into(),
        });

        assert_eq!(
            state.format_feedback(None),
            "Proposed plan line 9:\n> [selected lines unavailable]\n\nComment:\nwhere is this"
        );
    }

    #[test]
    fn file_backed_plan_feedback_keeps_plan_md_references() {
        let (mut state, _rx) = make_test_state();
        state.source = PlanReviewSource::FileBacked;
        state.plan_content = Some("alpha\nbravo".into());
        state.comments.push(PlanComment {
            id: 0,
            line_range: 1..3,
            text: "keep file ref".into(),
        });

        assert_eq!(
            state.format_feedback(Some("freeform")),
            "@plan.md:1-2\nkeep file ref\n\nfreeform"
        );
    }
}
