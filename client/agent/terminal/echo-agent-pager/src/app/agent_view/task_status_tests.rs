use super::{AgentPane, AgentView, AppRenderParams, BannerSlotParams, test_fixtures};
use crate::actions::ActionRegistry;
use crate::app::app_view::InputOutcome;
use crate::app::bundle::BundleState;
use crate::scrollback::render::ScratchBuffer;
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::time::Instant;
fn workflow_run(status: &str) -> crate::views::workflows::WorkflowRunSnapshot {
    crate::views::workflows::WorkflowRunSnapshot {
        run_id: "wf-gate".to_owned(),
        name: "gate".to_owned(),
        objective: "objective".to_owned(),
        status: status.to_owned(),
        management_available: true,
        builtin: false,
        phases: Vec::new(),
        current_phase: None,
        agents: Vec::new(),
        agent_budget: None,
        agents_used: 0,
        agents_reserved: 0,
        agents_remaining: None,
        agent_usage_incomplete: false,
        active_agents: 0,
        elapsed_ms: 0,
        received_at: Instant::now(),
        pause_message: None,
        result_summary: None,
    }
}
fn draw_frame(agent: &mut AgentView, registry: &ActionRegistry) -> Buffer {
    let area = Rect::new(0, 0, 80, 30);
    let bundle = BundleState::default();
    let mut buf = Buffer::empty(area);
    let mut scratch = ScratchBuffer::new();
    agent.draw(
        area,
        &mut buf,
        registry,
        &mut scratch,
        None,
        false,
        BannerSlotParams {
            height: 0,
            announcements: &[],
            hidden_ids: &std::collections::BTreeSet::new(),
            privacy_banner: false,
            mouse_pos: None,
            tip: None,
        },
        &bundle,
        false,
        false,
        &mut Vec::new(),
        AppRenderParams::default(),
    );
    buf
}
fn mouse_down(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })
}
#[test]
fn paused_status_has_one_click_target_that_clears_when_terminal() {
    let _theme = crate::theme::cache::pin_theme();
    let registry = ActionRegistry::defaults();
    let mut agent = test_fixtures::make_agent();
    agent.last_terminal_size = (80, 30);
    agent.workflow_runs = vec![workflow_run("user_paused")];
    let _ = draw_frame(&mut agent, &registry);
    let rect = agent
        .hit_bg_status
        .rect
        .expect("paused workflow must arm one background status target");
    let outcome = agent.handle_input(&mouse_down(rect.x, rect.y), &registry);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(agent.tasks.overlay.visible && agent.tasks.overlay.focused);
    assert_eq!(agent.active_pane, AgentPane::Tasks);
    let outcome = agent.handle_input(&mouse_down(rect.x, rect.y), &registry);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(!agent.tasks.overlay.visible && !agent.tasks.overlay.focused);
    assert_eq!(agent.active_pane, AgentPane::Scrollback);
    agent.workflow_runs[0].status = "complete".to_owned();
    let _ = draw_frame(&mut agent, &registry);
    assert!(agent.hit_bg_status.rect.is_none());
}
