//! Mouse-routing tests for the line viewer's plan preview: the scrollbar
//! must own a click+drag gesture end-to-end. A press on the track was
//! previously also treated as a comment-gutter anchor (row-only hit test),
//! so dragging the thumb selected plan lines for a comment instead of
//! scrolling (GB-4579: "can't click and drag scrollbar to view plan").

use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::actions::ActionRegistry;
use crate::app::agent_view::AgentView;
use crate::app::agent_view::test_fixtures::make_agent;
use crate::views::plan_approval_view::PlanApprovalFocus;

const POPUP: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 10,
};
/// Scrollbar track column as split off by the list pane render
/// (`maybe_split_for_scrollbar`): last column of the popup area.
const TRACK_X: u16 = 79;

fn mouse(kind: MouseEventKind, col: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::empty(),
    })
}

/// Agent showing a plan-approval preview whose plan overflows the
/// viewport, with the render-time areas planted so mouse dispatch works.
fn agent_with_scrollable_plan() -> AgentView {
    let mut agent = make_agent();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let plan: String = (1..=60).fold(String::new(), |mut acc, i| {
        acc.push_str(&format!("step {i}\n"));
        acc
    });
    let request = crate::views::plan_approval_view::ExitPlanModeExtRequest {
        session_id: "test-session".into(),
        tool_call_id: "call-1".into(),
        plan_content: Some(plan),
    };
    agent.plan_approval_view = Some(
        crate::views::plan_approval_view::PlanApprovalViewState::new(
            request,
            crate::views::prompt_widget::StashedPrompt {
                text: String::new(),
                cursor: 0,
                images: Vec::new(),
                chip_elements: Vec::new(),
                image_counter: 0,
                image_undo_stash: Vec::new(),
            },
            tx,
        ),
    );
    agent.show_plan_preview();

    let viewer = agent
        .line_viewer
        .as_mut()
        .expect("plan preview opens the line viewer");
    viewer.prepare_layout(POPUP.width, POPUP.height);
    viewer.last_popup_area = Some(POPUP);
    viewer.last_modal_area = Some(Rect::new(0, 0, 80, 12));
    viewer
        .list_state
        .set_scrollbar_area(Some(Rect::new(TRACK_X, POPUP.y, 1, POPUP.height)));
    assert!(
        viewer.list_state.total_height() > POPUP.height as usize,
        "plan must overflow the viewport so the scrollbar is live"
    );
    agent
}

/// Presses on the modal border column next to the track (users read the
/// thumb + border as one two-column scrollbar) used to fall into the
/// click-outside-modal path instead of grabbing the thumb.
#[test]
fn border_column_press_grabs_scrollbar() {
    let mut agent = agent_with_scrollable_plan();
    let registry = ActionRegistry::defaults();

    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), TRACK_X + 1, 5),
        &registry,
    );

    let viewer = agent.line_viewer.as_ref().expect("viewer stays open");
    assert!(
        viewer.list_state.is_scrollbar_dragging(),
        "press one column right of the track (modal border) must grab the thumb"
    );
    assert!(
        viewer.list_state.scroll_offset() > 0,
        "the press must scroll toward the clicked track position"
    );
    assert!(
        viewer
            .plan_ref()
            .and_then(|p| p.gutter_drag_start)
            .is_none(),
        "a border-column press must not anchor a comment-gutter drag"
    );
    let pav = agent.plan_approval_view.as_ref().unwrap();
    assert_eq!(pav.focus, PlanApprovalFocus::Preview);

    let offset_after_press = agent
        .line_viewer
        .as_ref()
        .unwrap()
        .list_state
        .scroll_offset();
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Drag(MouseButton::Left), TRACK_X + 1, 9),
        &registry,
    );
    let viewer = agent.line_viewer.as_ref().unwrap();
    assert!(
        viewer.list_state.scroll_offset() > offset_after_press,
        "dragging on the border column must keep scrolling (offset {} -> {})",
        offset_after_press,
        viewer.list_state.scroll_offset()
    );
}

#[test]
fn gap_column_press_grabs_scrollbar() {
    let mut agent = agent_with_scrollable_plan();
    let registry = ActionRegistry::defaults();

    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), TRACK_X - 1, 5),
        &registry,
    );

    let viewer = agent.line_viewer.as_ref().unwrap();
    assert!(
        viewer.list_state.is_scrollbar_dragging(),
        "press on the gap column must grab the thumb"
    );
    assert!(
        viewer
            .plan_ref()
            .and_then(|p| p.gutter_drag_start)
            .is_none(),
        "a gap-column press must not anchor a comment-gutter drag"
    );
}

#[test]
fn border_column_press_does_not_close_casual_preview() {
    let mut agent = agent_with_scrollable_plan();
    agent.plan_approval_view = None;
    let registry = ActionRegistry::defaults();

    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), TRACK_X + 1, 5),
        &registry,
    );

    let viewer = agent
        .line_viewer
        .as_ref()
        .expect("a border-column press must not close the casual preview");
    assert!(viewer.list_state.is_scrollbar_dragging());
}

#[test]
fn press_beyond_grab_zone_still_closes_casual_preview() {
    let mut agent = agent_with_scrollable_plan();
    agent.plan_approval_view = None;
    let registry = ActionRegistry::defaults();

    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), TRACK_X + 2, 5),
        &registry,
    );

    assert!(
        agent.line_viewer.is_none(),
        "a click two columns right of the track is outside the modal and must close it"
    );
}

#[test]
fn scrollbar_press_does_not_enter_commenting() {
    let mut agent = agent_with_scrollable_plan();
    let registry = ActionRegistry::defaults();

    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), TRACK_X, 5),
        &registry,
    );

    let viewer = agent.line_viewer.as_ref().unwrap();
    assert!(
        viewer.list_state.is_scrollbar_dragging(),
        "press on the track must latch a scrollbar drag"
    );
    assert!(
        viewer
            .plan_ref()
            .and_then(|p| p.gutter_drag_start)
            .is_none(),
        "press on the track must not anchor a comment-gutter drag"
    );
    let pav = agent.plan_approval_view.as_ref().unwrap();
    assert_eq!(
        pav.focus,
        PlanApprovalFocus::Preview,
        "press on the track must not enter commenting"
    );
}

#[test]
fn scrollbar_drag_scrolls_plan_instead_of_selecting_lines() {
    let mut agent = agent_with_scrollable_plan();
    let registry = ActionRegistry::defaults();

    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), TRACK_X, 2),
        &registry,
    );
    let offset_after_press = agent
        .line_viewer
        .as_ref()
        .unwrap()
        .list_state
        .scroll_offset();

    // Drag the thumb to the bottom of the track.
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Drag(MouseButton::Left), TRACK_X, 9),
        &registry,
    );

    let viewer = agent.line_viewer.as_ref().unwrap();
    assert!(
        viewer.list_state.scroll_offset() > offset_after_press,
        "dragging the thumb down must scroll the plan (offset {} -> {})",
        offset_after_press,
        viewer.list_state.scroll_offset()
    );
    assert!(
        viewer.plan_ref().and_then(|p| p.gutter_drag_end).is_none(),
        "thumb drag must not extend a comment line selection"
    );

    let _ = agent.handle_input(
        &mouse(MouseEventKind::Up(MouseButton::Left), TRACK_X, 9),
        &registry,
    );
    let viewer = agent.line_viewer.as_ref().unwrap();
    assert!(
        !viewer.list_state.is_scrollbar_dragging(),
        "release must end the scrollbar drag"
    );
    let pav = agent.plan_approval_view.as_ref().unwrap();
    assert_eq!(
        pav.commenting_range, None,
        "releasing the thumb must not open a comment on the dragged lines"
    );
    assert_eq!(pav.focus, PlanApprovalFocus::Preview);
}

/// The thumb must keep following the pointer when a drag drifts off the
/// popup rect (standard scrollbar behavior in every toolkit).
#[test]
fn scrollbar_drag_outside_popup_keeps_scrolling() {
    let mut agent = agent_with_scrollable_plan();
    let registry = ActionRegistry::defaults();

    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), TRACK_X, 8),
        &registry,
    );
    let offset_after_press = agent
        .line_viewer
        .as_ref()
        .unwrap()
        .list_state
        .scroll_offset();
    assert!(offset_after_press > 0, "press near the bottom scrolls down");

    // Pointer drifts left of the track and above the popup while dragging.
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Drag(MouseButton::Left), 40, 0),
        &registry,
    );

    let viewer = agent.line_viewer.as_ref().unwrap();
    assert!(
        viewer.list_state.scroll_offset() < offset_after_press,
        "drag toward the top of the track must scroll back up (offset {} -> {})",
        offset_after_press,
        viewer.list_state.scroll_offset()
    );
    assert!(
        viewer.plan_ref().and_then(|p| p.gutter_drag_end).is_none(),
        "scrollbar drag must never turn into a comment line selection"
    );
}

/// A gutter line-selection whose Up was lost must not survive a later
/// scrollbar gesture: the track press drops the stale anchor, so a stray
/// release afterwards cannot commit the leftover lines as a comment.
#[test]
fn scrollbar_gesture_drops_stale_gutter_anchor() {
    let mut agent = agent_with_scrollable_plan();
    let registry = ActionRegistry::defaults();

    // Anchor + extend a comment line selection, then lose the Up.
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), 10, 4),
        &registry,
    );
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Drag(MouseButton::Left), 10, 6),
        &registry,
    );
    {
        let viewer = agent.line_viewer.as_ref().unwrap();
        let start = viewer.plan_ref().and_then(|p| p.gutter_drag_start);
        let end = viewer.plan_ref().and_then(|p| p.gutter_drag_end);
        assert!(
            start.is_some() && end.is_some() && start != end,
            "precondition: a multi-line gutter drag is live (start {start:?}, end {end:?})"
        );
    }
    // Scrollbar click + release: the track press must drop the stale anchor.
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), TRACK_X, 5),
        &registry,
    );
    {
        let viewer = agent.line_viewer.as_ref().unwrap();
        assert!(viewer.list_state.is_scrollbar_dragging());
        assert!(
            viewer
                .plan_ref()
                .and_then(|p| p.gutter_drag_start)
                .is_none()
                && viewer.plan_ref().and_then(|p| p.gutter_drag_end).is_none(),
            "track press must drop a stale comment-gutter anchor"
        );
    }
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Up(MouseButton::Left), TRACK_X, 5),
        &registry,
    );

    // The track press also discarded the in-progress comment draft
    // (same rule as clicking back into the modal).
    let pav = agent.plan_approval_view.as_ref().unwrap();
    assert_eq!(pav.commenting_range, None);
    assert_eq!(pav.focus, PlanApprovalFocus::Preview);

    // A stray release on content must not commit the leftover lines.
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Up(MouseButton::Left), 10, 6),
        &registry,
    );
    let pav = agent.plan_approval_view.as_ref().unwrap();
    assert_eq!(
        pav.commenting_range, None,
        "stale gutter lines must not be committed as a comment range"
    );
    assert_eq!(
        pav.focus,
        PlanApprovalFocus::Preview,
        "a stray release must not re-enter commenting"
    );
}

/// A second multi-line gutter drag while already Commenting must not
/// replace the frozen freeform stash with the unsaved comment draft.
#[test]
fn gutter_drag_while_commenting_does_not_clobber_freeform_stash() {
    let mut agent = agent_with_scrollable_plan();
    let registry = ActionRegistry::defaults();

    agent.prompt.set_text("keep my freeform notes");
    // First multi-line drag: enter commenting and freeze freeform.
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), 10, 4),
        &registry,
    );
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Drag(MouseButton::Left), 10, 6),
        &registry,
    );
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Up(MouseButton::Left), 10, 6),
        &registry,
    );
    {
        let pav = agent.plan_approval_view.as_ref().unwrap();
        assert_eq!(pav.focus, PlanApprovalFocus::Commenting);
        assert_eq!(
            pav.stashed_feedback_prompt
                .as_ref()
                .map(|s| s.text.as_str()),
            Some("keep my freeform notes")
        );
    }
    agent.prompt.set_text("unsaved comment draft");

    // Second multi-line drag: new range, must keep original freeform stash.
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), 10, 5),
        &registry,
    );
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Drag(MouseButton::Left), 10, 7),
        &registry,
    );
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Up(MouseButton::Left), 10, 7),
        &registry,
    );
    {
        let pav = agent.plan_approval_view.as_ref().unwrap();
        assert_eq!(pav.focus, PlanApprovalFocus::Commenting);
        assert_eq!(
            pav.stashed_feedback_prompt
                .as_ref()
                .map(|s| s.text.as_str()),
            Some("keep my freeform notes"),
            "second gutter drag must not replace freeform with comment draft"
        );
    }
    // Cancel commenting: freeform must restore, not the abandoned draft.
    agent.prompt.set_text("another draft");
    let esc = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    );
    let _ = agent.handle_plan_feedback_key(&esc);
    assert_eq!(agent.prompt.text(), "keep my freeform notes");
}

/// A lost mouse-up after a track press must not make the next plan-line
/// click skip gutter / click-to-comment (sticky `is_scrollbar_dragging`).
#[test]
fn lost_scrollbar_up_does_not_block_next_line_click() {
    let mut agent = agent_with_scrollable_plan();
    let registry = ActionRegistry::defaults();

    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), TRACK_X, 5),
        &registry,
    );
    assert!(
        agent
            .line_viewer
            .as_ref()
            .unwrap()
            .list_state
            .is_scrollbar_dragging(),
        "precondition: track press latched a thumb drag"
    );

    // No Up — simulate a dropped release, then click a plan line.
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), 10, 4),
        &registry,
    );

    let viewer = agent.line_viewer.as_ref().unwrap();
    assert!(
        !viewer.list_state.is_scrollbar_dragging(),
        "content Down must clear the stale scrollbar latch"
    );
    assert!(
        viewer
            .plan_ref()
            .and_then(|p| p.gutter_drag_start)
            .is_some(),
        "content Down must still anchor a comment-gutter drag"
    );
    let pav = agent.plan_approval_view.as_ref().unwrap();
    assert_eq!(
        pav.focus,
        PlanApprovalFocus::Commenting,
        "content Down must still enter click-to-comment"
    );
}

#[test]
fn wheel_on_border_column_scrolls_plan() {
    let mut agent = agent_with_scrollable_plan();
    let registry = ActionRegistry::defaults();

    let _ = agent.handle_input(
        &mouse(MouseEventKind::Down(MouseButton::Left), TRACK_X + 1, 9),
        &registry,
    );
    let _ = agent.handle_input(
        &mouse(MouseEventKind::Up(MouseButton::Left), TRACK_X + 1, 9),
        &registry,
    );
    let off = agent
        .line_viewer
        .as_ref()
        .unwrap()
        .list_state
        .scroll_offset();
    assert!(off > 0, "border click near track bottom scrolls down");

    agent.handle_scroll(-3, TRACK_X + 1, 5);
    let off_after = agent
        .line_viewer
        .as_ref()
        .unwrap()
        .list_state
        .scroll_offset();
    assert!(
        off_after < off,
        "wheel-up on the border column must scroll up ({off} -> {off_after})"
    );
}
