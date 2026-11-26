use super::{AgentPane, AgentView, BlockingCard, EscStep, KeyOwner};
use crate::actions::ActionRegistry;
use crate::app::agent_view::test_fixtures::{make_agent, make_followup_permission_state};
use crate::views::modal::{CancelTurnChoice, CancelTurnViewState};
use crate::views::permission_view::PermissionFocus;
use crate::views::prompt_widget::StashedPrompt;
use crate::views::question_view::QuestionViewState;
use agent_client_protocol as acp;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
    Question, QuestionOption,
};
use std::sync::Arc;

const SHIFT_TAB: [(KeyCode, KeyModifiers); 3] = [
    (KeyCode::BackTab, KeyModifiers::NONE),
    (KeyCode::BackTab, KeyModifiers::SHIFT),
    (KeyCode::Tab, KeyModifiers::SHIFT),
];

fn option(id: &str, kind: acp::PermissionOptionKind) -> acp::PermissionOption {
    acp::PermissionOption::new(
        acp::PermissionOptionId::new(Arc::from(id)),
        id.to_string(),
        kind,
    )
}

fn open_permission(agent: &mut AgentView) {
    let mut perm = make_followup_permission_state();
    perm.focus = PermissionFocus::Options;
    perm.options = vec![
        option("allow-once", acp::PermissionOptionKind::AllowOnce),
        option("allow-always", acp::PermissionOptionKind::AllowAlways),
        option("reject-once", acp::PermissionOptionKind::RejectOnce),
        option("reject-always", acp::PermissionOptionKind::RejectAlways),
    ];
    agent.permission_queue.push_back(perm);
}

fn open_cancel_turn(agent: &mut AgentView) {
    agent.cancel_turn_view = Some(CancelTurnViewState {
        active_idx: 0,
        running_count: 1,
    });
}

fn question(prompt: &str) -> Question {
    Question {
        question: prompt.into(),
        options: ["Alpha", "Beta"]
            .into_iter()
            .map(|label| QuestionOption {
                label: label.into(),
                description: "why".into(),
                preview: None,
                id: None,
            })
            .collect(),
        multi_select: Some(false),
        id: None,
    }
}

fn open_question(agent: &mut AgentView) {
    agent.question_view = Some(QuestionViewState::new(
        "tc-card".into(),
        vec![question("Which?")],
        StashedPrompt::default(),
    ));
}

fn open_plan(agent: &mut AgentView) {
    agent.plan_approval_view =
        Some(crate::app::agent_view::test_fixtures::make_plan_approval_view_state());
}

fn open_plan_over_question(agent: &mut AgentView) {
    open_question(agent);
    open_plan(agent);
}

fn open_two_questions(agent: &mut AgentView) {
    agent.question_view = Some(QuestionViewState::new(
        "tc-card".into(),
        vec![question("Which?"), question("And then?")],
        StashedPrompt::default(),
    ));
}

fn permission_cursor(agent: &AgentView) -> usize {
    agent
        .permission_queue
        .front()
        .expect("permission open")
        .active_idx
}

fn permission_key(agent: &mut AgentView, code: KeyCode, modifiers: KeyModifiers) {
    let _ = agent.handle_permission_key(&KeyEvent::new(code, modifiers));
}

fn hint_labels(agent: &AgentView) -> Vec<String> {
    agent
        .current_shortcut_hints(&ActionRegistry::defaults(), false)
        .iter()
        .map(|hint| hint.label.to_string())
        .collect()
}

fn tab_from_scrollback(agent: &mut AgentView) {
    let registry = ActionRegistry::defaults();
    let _ =
        agent.handle_scrollback_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &registry);
}

#[test]
fn permission_tab_walks_the_options_and_wraps() {
    let mut agent = make_agent();
    open_permission(&mut agent);

    let mut visited = vec![permission_cursor(&agent)];
    for _ in 0..4 {
        permission_key(&mut agent, KeyCode::Tab, KeyModifiers::NONE);
        visited.push(permission_cursor(&agent));
    }
    assert_eq!(
        visited,
        vec![0, 1, 2, 3, 0],
        "Tab walks every option row and wraps at the end"
    );
    assert_eq!(
        agent.active_pane,
        AgentPane::Prompt,
        "the card keeps the keyboard — Tab is no longer a defocus"
    );
}

#[test]
fn permission_shift_tab_walks_backwards_in_every_encoding() {
    for (code, modifiers) in SHIFT_TAB {
        let mut agent = make_agent();
        open_permission(&mut agent);

        permission_key(&mut agent, code, modifiers);
        assert_eq!(
            permission_cursor(&agent),
            3,
            "before the first option, Shift+Tab wraps to the last ({code:?}/{modifiers:?})"
        );
        permission_key(&mut agent, code, modifiers);
        assert_eq!(permission_cursor(&agent), 2);
        assert_eq!(agent.active_pane, AgentPane::Prompt);
    }
}

#[test]
fn permission_ctrl_tab_is_not_a_walk() {
    let mut agent = make_agent();
    open_permission(&mut agent);

    permission_key(&mut agent, KeyCode::Tab, KeyModifiers::CONTROL);
    assert_eq!(permission_cursor(&agent), 0);
}

#[test]
fn permission_esc_parks_focus_without_answering() {
    let mut agent = make_agent();
    open_permission(&mut agent);

    permission_key(&mut agent, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(agent.active_pane, AgentPane::Scrollback);
    assert_eq!(
        agent.permission_queue.len(),
        1,
        "Esc must not answer or dismiss the request"
    );

    tab_from_scrollback(&mut agent);
    assert_eq!(
        agent.active_pane,
        AgentPane::Prompt,
        "Tab from the scrollback hands the keyboard back to the card"
    );
}

#[test]
fn parking_releases_a_focused_side_pane() {
    let mut agent = make_agent();
    open_permission(&mut agent);
    agent.todo.overlay.focused = true;

    permission_key(&mut agent, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(agent.active_pane, AgentPane::Scrollback);
    assert!(
        !agent.todo.overlay.focused,
        "parking must release the side pane's focus, not leave it stale"
    );
}

#[test]
fn permission_hints_follow_focus() {
    let mut agent = make_agent();
    open_permission(&mut agent);

    let focused = hint_labels(&agent);
    assert!(
        focused.contains(&"next option".to_string()),
        "the bar names the option walk, got {focused:?}"
    );
    assert!(
        focused.contains(&"scrollback".to_string()),
        "and the way out, got {focused:?}"
    );

    agent.active_pane = AgentPane::Scrollback;
    let parked = hint_labels(&agent);
    assert!(
        !parked.contains(&"next option".to_string())
            && !parked.contains(&"always-approve".to_string()),
        "parked in the scrollback the bar must drop the card's keys, got {parked:?}"
    );
    assert!(
        parked.contains(&"permission".to_string()),
        "the bar must name the way back into the card, got {parked:?}"
    );
}

#[test]
fn permission_tab_is_inert_with_a_single_option() {
    let mut agent = make_agent();
    let mut perm = make_followup_permission_state();
    perm.focus = PermissionFocus::Options;
    perm.options = vec![option("allow-once", acp::PermissionOptionKind::AllowOnce)];
    agent.permission_queue.push_back(perm);

    permission_key(&mut agent, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(permission_cursor(&agent), 0);
    assert_eq!(agent.active_pane, AgentPane::Prompt);
}

#[test]
fn cancel_turn_tab_walks_the_choices_and_wraps() {
    let mut agent = make_agent();
    open_cancel_turn(&mut agent);
    let last = CancelTurnChoice::ALL.len() - 1;

    for expected in 1..=last {
        let _ = agent.handle_cancel_turn_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            agent
                .cancel_turn_view
                .as_ref()
                .expect("panel open")
                .active_idx,
            expected
        );
    }
    let _ = agent.handle_cancel_turn_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(
        agent
            .cancel_turn_view
            .as_ref()
            .expect("panel open")
            .active_idx,
        0,
        "past the last choice, back to the first"
    );

    for (code, modifiers) in SHIFT_TAB {
        let _ = agent.handle_cancel_turn_key(&KeyEvent::new(code, modifiers));
        assert_eq!(
            agent
                .cancel_turn_view
                .as_ref()
                .expect("panel open")
                .active_idx,
            last,
            "Shift+Tab wraps back to the last choice ({code:?}/{modifiers:?})"
        );
        let _ = agent.handle_cancel_turn_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    }
    assert_eq!(
        agent.active_pane,
        AgentPane::Prompt,
        "the panel keeps the keyboard"
    );
}

#[test]
fn cancel_turn_panel_parks_and_returns_like_the_others() {
    let mut agent = make_agent();
    open_cancel_turn(&mut agent);
    assert!(hint_labels(&agent).contains(&"next choice".to_string()));

    agent.active_pane = AgentPane::Scrollback;
    let parked = hint_labels(&agent);
    assert!(
        !parked.contains(&"next choice".to_string()),
        "parked, the panel's keys leave the bar, got {parked:?}"
    );
    assert!(
        parked.contains(&"cancel turn".to_string()),
        "the bar must name the way back into the panel, got {parked:?}"
    );

    tab_from_scrollback(&mut agent);
    assert_eq!(agent.active_pane, AgentPane::Prompt);
}

#[test]
fn a_parked_card_contributes_one_route_back() {
    let mut agent = make_agent();
    open_question(&mut agent);
    assert!(hint_labels(&agent).contains(&"next answer".to_string()));

    agent.active_pane = AgentPane::Scrollback;
    let hints = agent.current_shortcut_hints(&ActionRegistry::defaults(), false);
    let labels: Vec<String> = hints.iter().map(|h| h.label.to_string()).collect();
    assert!(
        !labels.contains(&"next answer".to_string()),
        "parked, the card's own keys leave the bar, got {labels:?}"
    );
    assert_eq!(
        labels.iter().filter(|l| *l == "question").count(),
        1,
        "one hint names the card, got {labels:?}"
    );
    assert!(
        !labels.contains(&"prompt".to_string()),
        "and it replaces the pane's own focus hint rather than joining it, got {labels:?}"
    );

    let back = hints.first().expect("hints are not empty");
    assert_eq!(back.label, "question", "the route back leads the bar");
    assert!(
        back.pinned,
        "and is pinned, so a narrow bar's trim cannot drop it"
    );
    assert!(
        back.keys.contains(&crate::key!(Tab)),
        "Tab is a route back, so the hint must name it: {:?}",
        back.keys
    );
}

#[test]
fn the_bar_follows_the_router_when_two_cards_are_open() {
    let mut agent = make_agent();
    open_question(&mut agent);
    open_cancel_turn(&mut agent);
    assert_eq!(agent.focused_card(), Some(BlockingCard::CancelTurn));

    let labels = hint_labels(&agent);
    assert!(
        labels.contains(&"next choice".to_string()) && !labels.contains(&"next answer".to_string()),
        "the cancel-turn panel takes the keys, so it takes the bar too, got {labels:?}"
    );

    open_permission(&mut agent);
    assert_eq!(agent.focused_card(), Some(BlockingCard::Permission));
    let labels = hint_labels(&agent);
    assert!(
        labels.contains(&"next option".to_string()) && !labels.contains(&"next choice".to_string()),
        "and the permission card outranks both, got {labels:?}"
    );
}

#[test]
fn elicitation_shares_the_question_layer_under_cancel_turn() {
    let mut agent = make_agent();
    open_elicitation(&mut agent);
    assert_eq!(agent.blocking_card(), Some(BlockingCard::McpElicitation));
    assert_eq!(agent.focused_card(), Some(BlockingCard::McpElicitation));

    open_question(&mut agent);
    assert_eq!(
        agent.blocking_card(),
        Some(BlockingCard::Question),
        "question and elicitation share a layer; the painted question keeps the keys"
    );
    assert_eq!(agent.focused_card(), Some(BlockingCard::Question));
    let labels = hint_labels(&agent);
    assert!(
        labels.contains(&"next answer".to_string()),
        "the bar must name the question the user can see, got {labels:?}"
    );

    open_cancel_turn(&mut agent);
    assert_eq!(
        agent.blocking_card(),
        Some(BlockingCard::CancelTurn),
        "cancel-turn outranks both question-style cards"
    );
    assert_eq!(agent.focused_card(), Some(BlockingCard::CancelTurn));
    let labels = hint_labels(&agent);
    assert!(
        labels.contains(&"next choice".to_string()) && !labels.contains(&"next answer".to_string()),
        "the cancel-turn panel takes the keys, so it takes the bar too, got {labels:?}"
    );

    agent.question_view = None;
    assert_eq!(
        agent.blocking_card(),
        Some(BlockingCard::CancelTurn),
        "cancel-turn still occupies the slot over a leftover elicitation"
    );
    assert_eq!(agent.focused_card(), Some(BlockingCard::CancelTurn));
}

#[test]
fn the_esc_hint_names_the_rung_the_key_takes() {
    let mut agent = make_agent();
    open_question(&mut agent);

    assert_eq!(agent.card_esc(), Some(EscStep::ParkFocus));
    assert!(hint_labels(&agent).contains(&"scrollback".to_string()));

    let _ =
        agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    assert_eq!(agent.card_esc(), Some(EscStep::ClearSelection));
    assert!(hint_labels(&agent).contains(&"unselect".to_string()));
}

#[test]
fn the_overlay_owns_the_park_rung_and_the_bar_says_so() {
    let mut agent = make_agent();
    open_question(&mut agent);
    agent.in_dashboard_overlay = true;

    assert_eq!(agent.card_esc(), Some(EscStep::BackOutOverlay));
    assert!(
        agent.overlay_esc_backs_out(),
        "the overlay cascade and the ladder must agree"
    );
    assert!(
        hint_labels(&agent).contains(&"dashboard".to_string()),
        "the bar names where Esc actually goes, got {:?}",
        hint_labels(&agent)
    );

    let _ =
        agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    assert_eq!(agent.card_esc(), Some(EscStep::ClearSelection));
    assert!(
        !agent.overlay_esc_backs_out(),
        "a selection to clear keeps Esc in the card"
    );
}

#[test]
fn a_parked_card_does_not_hand_esc_to_the_turn_cancel() {
    for open in [
        open_permission as fn(&mut AgentView),
        open_question as fn(&mut AgentView),
        open_elicitation as fn(&mut AgentView),
    ] {
        let mut agent = make_agent();
        open(&mut agent);
        agent.session.state = crate::app::agent::AgentState::TurnRunning;

        agent.park_focused_card();
        assert_eq!(agent.active_pane, AgentPane::Scrollback);

        let outcome = agent.handle_input(
            &crossterm::event::Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &ActionRegistry::defaults(),
        );
        assert!(
            !matches!(
                outcome,
                crate::app::agent_view::InputOutcome::Action(
                    crate::app::actions::Action::CancelTurn
                )
            ),
            "Esc behind a parked card must not cancel the turn the card is blocking, got {outcome:?}"
        );
        assert!(
            agent.blocking_card().is_some(),
            "and the request must survive"
        );
    }
}

#[test]
fn plan_approval_takes_the_bar_wherever_it_takes_the_keys() {
    let mut agent = make_agent();
    open_question(&mut agent);
    agent.plan_approval_view =
        Some(crate::app::agent_view::test_fixtures::make_plan_approval_view_state());

    assert_eq!(
        agent.key_owner(),
        KeyOwner::PlanApproval,
        "the router hands plan approval the keyboard ahead of any card"
    );
    assert_eq!(agent.focused_card(), None, "so no card is taking keys");
    let labels = hint_labels(&agent);
    assert!(
        !labels.contains(&"next answer".to_string()),
        "and the bar must not advertise the question card's walk, got {labels:?}"
    );
    assert!(
        labels.contains(&"copy plan".to_string()),
        "it names the surface the keys actually reach, got {labels:?}"
    );
}

/// The open plan preview is the state a plan approval spends most of its life
/// in. The line viewer ranks above Question/CancelTurn (not Permission) and
/// paints its own hints over the bar's row; what the bar must not do is speak
/// for the card behind the viewer.
#[test]
fn a_question_under_the_open_plan_viewer_does_not_take_the_bar() {
    let mut agent = make_agent();
    open_question(&mut agent);
    agent.plan_approval_view =
        Some(crate::app::agent_view::test_fixtures::make_plan_approval_view_state());
    agent.reopen_plan_approval();

    assert!(agent.line_viewer.is_some(), "the preview is open");
    assert_eq!(
        agent.key_owner(),
        KeyOwner::LineViewer,
        "the viewer, not the approval prompt or the card, holds the keyboard"
    );
    assert_eq!(agent.focused_card(), None);
    assert_eq!(
        agent.blocking_card(),
        Some(BlockingCard::Question),
        "the question card is still drawn and still waiting behind it"
    );
    assert!(
        hint_labels(&agent).is_empty(),
        "the viewer paints its own hints over the row, so the bar stays quiet"
    );
}

#[test]
fn permission_interrupts_open_plan_viewer() {
    let mut agent = make_agent();
    agent.plan_approval_view =
        Some(crate::app::agent_view::test_fixtures::make_plan_approval_view_state());
    agent.reopen_plan_approval();
    assert!(agent.line_viewer.is_some());

    open_permission(&mut agent);

    assert_eq!(agent.key_owner(), KeyOwner::Card(BlockingCard::Permission),);
    assert_eq!(agent.focused_card(), Some(BlockingCard::Permission));
}

/// A file preview from the prompt is the same shape as the plan preview: it
/// takes the keys ahead of a card that opened behind it.
#[test]
fn a_card_under_any_open_line_viewer_does_not_take_the_bar() {
    let mut agent = make_agent();
    open_question(&mut agent);
    let path = std::env::temp_dir().join("key_owner_line_viewer.txt");
    std::fs::write(&path, "one\ntwo\n").expect("write fixture");
    agent.open_line_viewer(&path, None);

    assert!(agent.line_viewer.is_some(), "the preview is open");
    assert_eq!(agent.key_owner(), KeyOwner::LineViewer);
    assert!(
        !hint_labels(&agent).contains(&"next answer".to_string()),
        "the bar must not advertise a walk the viewer would swallow"
    );
}

/// Opening a card stashes the composer and blanks it without leaving
/// `EditingQueued`, so the dirty-edit lock would read the blank as an unsaved
/// edit and refuse the park — leaving `Esc`, the card's only keyboard exit,
/// answering with a toast instead.
#[test]
fn esc_parks_even_under_a_latent_queued_edit() {
    for open in [
        open_permission as fn(&mut AgentView),
        open_question as fn(&mut AgentView),
        open_elicitation as fn(&mut AgentView),
    ] {
        let mut agent = make_agent();
        agent.prompt_mode = crate::app::queue_edit::PromptMode::EditingQueued {
            id: 1,
            original: "the queued prompt".into(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
        open(&mut agent);
        agent.prompt.set_text("");

        agent.park_focused_card();
        assert_eq!(
            agent.active_pane,
            AgentPane::Scrollback,
            "the queued-edit lock must not hold the keyboard inside the card"
        );
        assert!(
            agent.blocking_card().is_some(),
            "and the card stays open and answerable"
        );
    }
}

#[test]
fn focus_accessors_agree_about_a_parked_card() {
    let mut agent = make_agent();
    open_permission(&mut agent);
    assert_eq!(agent.blocking_card(), Some(BlockingCard::Permission));
    assert_eq!(agent.focused_card(), Some(BlockingCard::Permission));
    assert_eq!(agent.parked_card(), None);
    assert!(agent.focused_permission().is_some());

    agent.active_pane = AgentPane::Scrollback;
    assert_eq!(agent.blocking_card(), Some(BlockingCard::Permission));
    assert_eq!(agent.focused_card(), None);
    assert_eq!(agent.parked_card(), Some(BlockingCard::Permission));
    assert!(agent.focused_permission().is_none());
    assert_eq!(agent.card_esc(), None);
}

#[test]
fn the_permission_esc_ladder_steps_out_one_rung_at_a_time() {
    let mut agent = make_agent();
    open_permission(&mut agent);
    let focus = |agent: &AgentView| agent.permission_queue.front().expect("open").focus;

    agent.permission_queue.front_mut().expect("open").focus = PermissionFocus::FollowupInput;
    assert_eq!(agent.card_esc(), Some(EscStep::LeaveTextInput));
    assert!(hint_labels(&agent).contains(&"back".to_string()));
    permission_key(&mut agent, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(focus(&agent), PermissionFocus::Options);

    agent.permission_queue.front_mut().expect("open").focus = PermissionFocus::PatternEdit;
    agent.permission_pattern_edit = Some(crate::views::permission_view::PatternEditState::new(
        String::from("git status"),
    ));
    assert_eq!(agent.card_esc(), Some(EscStep::DiscardPatternEdit));
    assert!(hint_labels(&agent).contains(&"cancel".to_string()));
    permission_key(&mut agent, KeyCode::Esc, KeyModifiers::NONE);
    assert!(agent.permission_pattern_edit.is_none());
    assert_eq!(focus(&agent), PermissionFocus::Options);

    assert_eq!(agent.card_esc(), Some(EscStep::ParkFocus));
    assert!(hint_labels(&agent).contains(&"scrollback".to_string()));
    permission_key(&mut agent, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(agent.active_pane, AgentPane::Scrollback);
    assert_eq!(
        agent.permission_queue.len(),
        1,
        "no rung of the ladder answers the request"
    );
}

#[test]
fn the_cancel_turn_panel_resolves_instead_of_parking() {
    let mut agent = make_agent();
    agent.session.state = crate::app::agent::AgentState::TurnRunning;
    open_cancel_turn(&mut agent);

    assert_eq!(agent.card_esc(), Some(EscStep::KeepRunning));
    assert!(hint_labels(&agent).contains(&"keep running".to_string()));

    let outcome = agent.handle_cancel_turn_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, crate::app::app_view::InputOutcome::Changed),
        "Esc must dismiss the panel without cancelling the turn, got {outcome:?}"
    );
    assert!(
        agent.cancel_turn_view.is_none(),
        "keep-running closes the panel"
    );
    assert!(
        agent.session.state.is_turn_running(),
        "dismissing is not a cancel"
    );
    assert_eq!(
        agent.active_pane,
        AgentPane::Prompt,
        "resolving is the way out, so the panel never parks"
    );
}

#[test]
fn esc_on_the_cancel_turn_panel_does_not_cancel_the_turn() {
    let mut agent = make_agent();
    agent.session.state = crate::app::agent::AgentState::TurnRunning;
    open_cancel_turn(&mut agent);

    let outcome = agent.handle_input(
        &crossterm::event::Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &ActionRegistry::defaults(),
    );
    assert!(
        !matches!(
            outcome,
            crate::app::app_view::InputOutcome::Action(
                crate::app::actions::Action::CancelTurn
                    | crate::app::actions::Action::CancelTurnChoice(_)
            )
        ),
        "the bar's 'keep running' must not cancel the turn, got {outcome:?}"
    );
    assert!(agent.cancel_turn_view.is_none());
    assert!(agent.session.state.is_turn_running());
}

/// Inside the dashboard overlay the ladder's last rung is the dashboard, and
/// anything parked behind a bare scrollback is on it — a card that parks
/// rather than backing out (a later question, or a permission prompt, which
/// has no back-out rung at all), and a plan approval, alone or on top of a
/// parked card. None of them hold the keyboard there, so none can consume
/// `Esc`, and the swallow that protects the turn would otherwise leave the
/// key inert until the user tabbed back in.
#[test]
fn anything_parked_in_the_overlay_keeps_an_esc_route_to_the_dashboard() {
    for (label, setup) in [
        ("permission", open_permission as fn(&mut AgentView)),
        ("question", open_question as fn(&mut AgentView)),
        ("elicitation", open_elicitation as fn(&mut AgentView)),
        ("plan approval", open_plan as fn(&mut AgentView)),
        (
            "plan approval over a parked question",
            open_plan_over_question as fn(&mut AgentView),
        ),
    ] {
        let mut agent = make_agent();
        agent.in_dashboard_overlay = true;
        setup(&mut agent);
        agent.set_active_pane(AgentPane::Scrollback, true);

        assert!(
            agent.overlay_esc_backs_out(),
            "{label}: parked behind the scrollback, the next Esc leaves the overlay"
        );

        agent.scrollback_search = Some(crate::scrollback::search::ScrollbackSearchState::open());
        assert!(
            !agent.overlay_esc_backs_out(),
            "{label}: but a layered scrollback sub-state still consumes Esc first"
        );
    }
}

/// The new rung is for surfaces the keyboard has left behind — it must not
/// turn a plain scrollback `Esc` into a detach, which still belongs to the
/// turn-cancel / rewind policy.
#[test]
fn a_bare_overlay_scrollback_esc_still_belongs_to_the_esc_policy() {
    let mut agent = make_agent();
    agent.in_dashboard_overlay = true;
    agent.set_active_pane(AgentPane::Scrollback, true);

    assert!(agent.is_bare_scrollback());
    assert!(
        !agent.overlay_esc_backs_out(),
        "with nothing pending there is nothing parked, so Esc keeps its policy meaning"
    );
}

#[test]
fn esc_on_a_later_question_parks_before_it_leaves_the_overlay() {
    let mut agent = make_agent();
    open_two_questions(&mut agent);
    agent.in_dashboard_overlay = true;
    agent
        .question_view
        .as_mut()
        .expect("card open")
        .next_question();

    assert_eq!(
        agent.card_esc(),
        Some(EscStep::ParkFocus),
        "Esc must not throw the user out of the session from question 2 — \
         Left still walks back there, so the card keeps the first press"
    );
    assert!(!agent.overlay_esc_backs_out());
    assert!(hint_labels(&agent).contains(&"scrollback".to_string()));

    let _ = agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(agent.active_pane, AgentPane::Scrollback);
    assert!(
        agent.overlay_esc_backs_out(),
        "and the next press leaves for the dashboard"
    );
}

/// The scrollback's focus hint names where `Tab` goes, so it has to be asked
/// through the same ranking as the keys themselves: with a plan approval
/// pending over a parked card, `Tab` lands in the approval, not the card.
#[test]
fn the_route_back_never_names_a_card_the_plan_approval_outranks() {
    let mut agent = make_agent();
    open_plan_over_question(&mut agent);
    agent.set_active_pane(AgentPane::Scrollback, true);

    assert_eq!(
        agent.blocking_card(),
        Some(BlockingCard::Question),
        "the card is still drawn and still waiting"
    );
    assert_eq!(
        agent.parked_card(),
        None,
        "but it is not what the keyboard would come back to"
    );

    let labels = hint_labels(&agent);
    assert!(
        !labels.contains(&"question".to_string()),
        "so the bar must not offer it as the route back, got {labels:?}"
    );

    agent.plan_approval_view = None;
    assert_eq!(
        agent.parked_card(),
        Some(BlockingCard::Question),
        "with the approval gone the card is the route back again"
    );
    assert!(hint_labels(&agent).contains(&"question".to_string()));
}

/// With the preview closed, the plan approval's own bar is what renders, and
/// it must name `Tab` the way the preview's bar does — the key does the same
/// thing in both states, so it cannot answer to two names.
#[test]
fn the_plan_preview_names_tab_the_way_its_viewer_does() {
    let mut agent = make_agent();
    open_plan(&mut agent);
    agent
        .plan_approval_view
        .as_mut()
        .expect("approval open")
        .focus = crate::views::plan_approval_view::PlanApprovalFocus::Preview;

    assert!(
        agent.line_viewer.is_none(),
        "an open preview paints its own row instead"
    );
    assert_eq!(agent.key_owner(), KeyOwner::PlanApproval);

    let labels = hint_labels(&agent);
    assert!(
        labels.contains(&"prompt".to_string()),
        "Tab moves focus to the plan prompt, and the viewer's bar calls it \
         `Tab:prompt` too, got {labels:?}"
    );
    assert!(labels.contains(&"copy plan".to_string()));
}

/// Blanking a free-text answer unmarks it, however the user leaves the text
/// field. The nav-button mouse path used to keep its own copy of the commit
/// that skipped the unmark, which left a stale selection behind — and the
/// `Esc` ladder reads that selection, so the next `Esc` would say `unselect`
/// with nothing selected instead of parking.
#[test]
fn blanking_a_free_text_answer_unmarks_it_from_the_nav_buttons_too() {
    use crate::views::question_view::QuestionFocus;

    let mut agent = make_agent();
    open_two_questions(&mut agent);
    agent.question_nav_buttons = vec![('l', ratatui::layout::Rect::new(0, 0, 3, 1))];

    // Mark a free-text answer, then blank the composer and leave by clicking
    // the nav bar's "next question" button.
    let qv = agent.question_view.as_mut().expect("card open");
    qv.focus = QuestionFocus::InputMode;
    qv.per_question_freeform[0] = "typed then deleted".into();
    qv.per_question_freeform_selected[0] = true;
    agent.prompt.set_text("   ");

    let _ = agent.handle_question_mouse(&crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 1,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });

    let qv = agent.question_view.as_ref().expect("card open");
    assert_eq!(
        qv.focus,
        QuestionFocus::Navigation,
        "the draft is committed"
    );
    assert!(
        !qv.per_question_freeform_selected[0],
        "a blank answer is not an answer, so its mark goes with it"
    );
    // The text itself is a draft, not an answer: `swap_question_freeform`
    // carries the composer across questions so coming back restores what was
    // typed. Only the mark decides what is submitted, and what `Esc` reads.

    agent.question_view.as_mut().expect("card open").active_tab = 0;
    assert_eq!(
        agent.card_esc(),
        Some(EscStep::ParkFocus),
        "with nothing marked, Esc parks rather than offering an empty unselect"
    );
}

// ── vim mode ──────────────────────────────────────────────────────────────
// The Tab/Esc contract is mode-independent: card intercepts run ahead of the
// scrollback's vim letter bindings. These go through `handle_input` so the
// full router (not just the card handlers) is under test.

fn press(agent: &mut AgentView, code: KeyCode, modifiers: KeyModifiers) {
    let registry = ActionRegistry::defaults();
    let _ = agent.handle_input(
        &crossterm::event::Event::Key(KeyEvent::new(code, modifiers)),
        &registry,
    );
}

fn question_cursor(agent: &AgentView) -> usize {
    agent
        .question_view
        .as_ref()
        .expect("question open")
        .cursor()
}

fn question_tab(agent: &AgentView) -> usize {
    agent
        .question_view
        .as_ref()
        .expect("question open")
        .active_tab
}

/// With vim mode on, the focused card still owns j/k/Tab/Esc — the same
/// walk-and-park contract as the default mode.
#[test]
fn vim_mode_focused_card_keeps_the_tab_contract() {
    let mut agent = make_agent();
    agent.vim_mode = true;
    open_two_questions(&mut agent);

    assert_eq!(question_cursor(&agent), 0);
    press(&mut agent, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(
        question_cursor(&agent),
        1,
        "j walks the answer rows while the card holds the keyboard"
    );
    press(&mut agent, KeyCode::Tab, KeyModifiers::NONE);
    // Two options + freeform row → Tab from option 1 lands on freeform (2).
    assert_eq!(question_cursor(&agent), 2, "Tab still walks answers");
    assert_eq!(
        agent.active_pane,
        AgentPane::Prompt,
        "Tab never parks the card"
    );

    // Leave freeform with an arrow (a letter would enter InputMode), then
    // `l` must still switch questions under vim mode.
    press(&mut agent, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(question_cursor(&agent), 1);
    press(&mut agent, KeyCode::Char('l'), KeyModifiers::NONE);
    assert_eq!(
        question_tab(&agent),
        1,
        "l still crosses questions when the card is focused"
    );

    press(&mut agent, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(agent.active_pane, AgentPane::Scrollback);
    assert!(
        agent.question_view.is_some(),
        "Esc parks without dismissing the card"
    );
    assert!(
        hint_labels(&agent).contains(&"question".to_string()),
        "parked bar names the route back, got {:?}",
        hint_labels(&agent)
    );
}

/// Once parked, vim letter keys belong to the scrollback — they must not
/// keep walking the card behind the pane.
#[test]
fn vim_mode_parked_card_does_not_eat_scrollback_jk() {
    let mut agent = make_agent();
    agent.vim_mode = true;
    open_two_questions(&mut agent);

    press(&mut agent, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(agent.active_pane, AgentPane::Scrollback);
    let cursor_before = question_cursor(&agent);
    let tab_before = question_tab(&agent);

    press(&mut agent, KeyCode::Char('j'), KeyModifiers::NONE);
    press(&mut agent, KeyCode::Char('k'), KeyModifiers::NONE);
    press(&mut agent, KeyCode::Char('l'), KeyModifiers::NONE);
    press(&mut agent, KeyCode::Char('h'), KeyModifiers::NONE);
    assert_eq!(
        question_cursor(&agent),
        cursor_before,
        "parked j/k must not move the card's answer cursor"
    );
    assert_eq!(
        question_tab(&agent),
        tab_before,
        "parked h/l must not switch questions"
    );
    assert_eq!(
        agent.active_pane,
        AgentPane::Scrollback,
        "scrollback keeps the keyboard"
    );

    press(&mut agent, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        agent.active_pane,
        AgentPane::Prompt,
        "Tab from the scrollback still hands the keyboard back"
    );
    assert_eq!(
        agent.key_owner(),
        KeyOwner::Card(BlockingCard::Question),
        "and the card owns keys again"
    );
}

/// Permission options walk the same way under vim mode, including the Esc
/// park that must never answer the request.
#[test]
fn vim_mode_permission_tab_and_esc_match_default() {
    let mut agent = make_agent();
    agent.vim_mode = true;
    open_permission(&mut agent);

    press(&mut agent, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(permission_cursor(&agent), 1);
    press(&mut agent, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(
        permission_cursor(&agent),
        2,
        "j walks permission options while the card holds the keyboard"
    );
    assert_eq!(agent.permission_queue.len(), 1);
    assert_eq!(agent.active_pane, AgentPane::Prompt);

    press(&mut agent, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(agent.active_pane, AgentPane::Scrollback);
    assert_eq!(agent.permission_queue.len(), 1);
    let parked_cursor = permission_cursor(&agent);

    // Parked j is scrollback nav — it must not walk or answer the options.
    press(&mut agent, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(agent.permission_queue.len(), 1);
    assert_eq!(permission_cursor(&agent), parked_cursor);
    assert_eq!(agent.active_pane, AgentPane::Scrollback);

    press(&mut agent, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(agent.active_pane, AgentPane::Prompt);
    assert_eq!(agent.key_owner(), KeyOwner::Card(BlockingCard::Permission));
}

fn open_elicitation(agent: &mut AgentView) {
    use crate::views::elicitation_view::ElicitationViewState;
    use echo_agent_tools::mcp_elicitation::{McpElicitExtRequest, McpElicitModeFields};
    agent.elicitation_view = Some(ElicitationViewState::from_request(
        McpElicitExtRequest {
            session_id: "s".into(),
            tool_call_id: "mcp-elicit-1".into(),
            server_name: "demo".into(),
            message: "Fill in".into(),
            mode: McpElicitModeFields::Form {
                requested_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "email": { "type": "string", "format": "email" }
                    },
                    "required": ["email"]
                })),
            },
        },
        Some(StashedPrompt::default()),
        None,
    ));
}

#[test]
fn question_keys_win_over_rewind() {
    use crate::views::rewind::RewindState;
    use crossterm::event::Event;
    let mut agent = make_agent();
    open_question(&mut agent);
    agent.rewind_state = Some(RewindState::new_cancel_offer(0, None, None));
    let registry = ActionRegistry::defaults();
    let _ = agent.handle_input(
        &Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        &registry,
    );
    let qv = agent.question_view.as_ref().expect("question stays open");
    assert_eq!(qv.cursor(), 1, "the question card consumed the key");
    assert!(
        agent.rewind_state.is_some(),
        "the cancel-offer stays parked behind the card"
    );
}

#[test]
fn elicitation_keys_win_over_rewind() {
    use crate::views::elicitation_view::ElicitationFocus;
    use crate::views::rewind::RewindState;
    use crossterm::event::Event;
    let mut agent = make_agent();
    open_elicitation(&mut agent);
    agent.rewind_state = Some(RewindState::new_cancel_offer(0, None, None));
    let registry = ActionRegistry::defaults();
    let _ = agent.handle_input(
        &Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        &registry,
    );
    let ev = agent.elicitation_view.as_ref().unwrap();
    assert_eq!(ev.focus, ElicitationFocus::Editing);
    assert_eq!(ev.form().unwrap().fields[0].draft(), "y");
    assert!(agent.rewind_state.is_some());
}

#[test]
fn elicitation_esc_leaves_edit_then_parks() {
    use crate::views::elicitation_view::ElicitationFocus;
    let mut agent = make_agent();
    open_elicitation(&mut agent);

    agent.elicitation_view.as_mut().unwrap().focus = ElicitationFocus::Editing;
    assert_eq!(agent.card_esc(), Some(EscStep::LeaveTextInput));
    assert!(hint_labels(&agent).contains(&"back".to_string()));
    let _ = agent.handle_elicitation_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(
        agent.elicitation_view.as_ref().unwrap().focus,
        ElicitationFocus::Fields
    );
    assert!(
        agent.elicitation_view.is_some(),
        "leaving edit must not cancel the request"
    );

    assert_eq!(agent.card_esc(), Some(EscStep::ParkFocus));
    assert!(hint_labels(&agent).contains(&"scrollback".to_string()));
    let _ = agent.handle_elicitation_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(agent.active_pane, AgentPane::Scrollback);
    assert!(
        agent.elicitation_view.is_some(),
        "park must not cancel the request"
    );
}

#[test]
fn elicitation_form_printable_keys_enter_edit() {
    use crate::views::elicitation_view::ElicitationFocus;
    let mut agent = make_agent();
    open_elicitation(&mut agent);
    assert_eq!(
        agent.elicitation_view.as_ref().unwrap().focus,
        ElicitationFocus::Fields
    );
    let _ = agent.handle_elicitation_key(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let ev = agent.elicitation_view.as_ref().unwrap();
    assert_eq!(ev.focus, ElicitationFocus::Editing);
    assert_eq!(ev.form().unwrap().fields[0].draft(), "y");
    let _ = agent.handle_elicitation_key(&KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
    let ev = agent.elicitation_view.as_ref().unwrap();
    assert_eq!(ev.focus, ElicitationFocus::Editing);
    assert_eq!(ev.form().unwrap().fields[0].draft(), "yd");
}

#[test]
fn elicitation_paste_on_fields_enters_edit() {
    use crate::views::elicitation_view::ElicitationFocus;
    let mut agent = make_agent();
    open_elicitation(&mut agent);
    assert_eq!(
        agent.elicitation_view.as_ref().unwrap().focus,
        ElicitationFocus::Fields
    );
    let _ = agent.handle_elicitation_paste("user@example.com");
    let ev = agent.elicitation_view.as_ref().unwrap();
    assert_eq!(ev.focus, ElicitationFocus::Editing);
    assert_eq!(ev.form().unwrap().fields[0].draft(), "user@example.com");
}

#[test]
fn elicitation_paste_strips_control_chars() {
    let mut agent = make_agent();
    open_elicitation(&mut agent);
    let _ = agent.handle_elicitation_paste("user\x1b]52;c;c3RvbGVu\x07@example.com");
    let draft = agent
        .elicitation_view
        .as_ref()
        .unwrap()
        .form()
        .unwrap()
        .fields[0]
        .draft();
    assert!(
        !draft.chars().any(char::is_control),
        "pasted escapes must not reach the draft: {draft:?}"
    );
    assert_eq!(draft, "user]52;c;c3RvbGVu@example.com");
}

#[test]
fn elicitation_draft_stops_at_named_cap() {
    use echo_agent_tools::mcp_elicitation::MAX_ELICIT_DRAFT_CHARS;
    let mut agent = make_agent();
    open_elicitation(&mut agent);
    let over = "a".repeat(MAX_ELICIT_DRAFT_CHARS + 32);
    let _ = agent.handle_elicitation_paste(&over);
    let draft = agent
        .elicitation_view
        .as_ref()
        .unwrap()
        .form()
        .unwrap()
        .fields[0]
        .draft();
    assert_eq!(draft.chars().count(), MAX_ELICIT_DRAFT_CHARS);
}

fn open_url_elicitation(
    agent: &mut AgentView,
    response_tx: Option<crate::views::elicitation_view::ElicitResponseTx>,
) {
    use crate::views::elicitation_view::ElicitationViewState;
    use echo_agent_tools::mcp_elicitation::{McpElicitExtRequest, McpElicitModeFields};
    agent.elicitation_view = Some(ElicitationViewState::from_request(
        McpElicitExtRequest {
            session_id: "s".into(),
            tool_call_id: "mcp-elicit-url".into(),
            server_name: "demo".into(),
            message: "Open".into(),
            mode: McpElicitModeFields::Url {
                url: format!("https://example.com/{}", "a/".repeat(200)),
                elicitation_id: "eid-1".into(),
            },
        },
        Some(StashedPrompt::default()),
        response_tx,
    ));
}

#[test]
fn url_accept_on_dead_request_dismisses_without_waiting() {
    let mut agent = make_agent();
    let (tx, rx) = tokio::sync::oneshot::channel();
    drop(rx);
    open_url_elicitation(&mut agent, Some(tx));
    let _ = agent.handle_elicitation_key(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(
        agent.elicitation_view.is_none(),
        "an accept the MCP side can no longer hear must dismiss the card, \
         not park it in waiting"
    );
}

#[test]
fn url_walk_keys_scroll_the_viewport() {
    let mut agent = make_agent();
    open_url_elicitation(&mut agent, None);
    let scroll = |agent: &AgentView| agent.elicitation_view.as_ref().unwrap().scroll;
    assert_eq!(scroll(&agent), 0);
    let _ = agent.handle_elicitation_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(scroll(&agent), 1);
    let _ = agent.handle_elicitation_key(&KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
    assert_eq!(scroll(&agent), 5);
    let _ = agent.handle_elicitation_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(scroll(&agent), 4);
    let _ = agent.handle_elicitation_key(&KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    assert_eq!(scroll(&agent), 0);
}

fn open_two_field_elicitation(agent: &mut AgentView) {
    use crate::views::elicitation_view::ElicitationViewState;
    use echo_agent_tools::mcp_elicitation::{McpElicitExtRequest, McpElicitModeFields};
    agent.elicitation_view = Some(ElicitationViewState::from_request(
        McpElicitExtRequest {
            session_id: "s".into(),
            tool_call_id: "mcp-elicit-2".into(),
            server_name: "demo".into(),
            message: "Fill in".into(),
            mode: McpElicitModeFields::Form {
                requested_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "email": { "type": "string" },
                        "name": { "type": "string" }
                    },
                    "required": ["email", "name"]
                })),
            },
        },
        Some(StashedPrompt::default()),
        None,
    ));
}

#[test]
fn elicitation_shift_tab_walks_fields_backwards() {
    use crate::views::elicitation_view::{ElicitationActionFocus, ElicitationFocus};
    for (code, modifiers) in SHIFT_TAB {
        let mut agent = make_agent();
        open_two_field_elicitation(&mut agent);
        let ev = agent.elicitation_view.as_ref().unwrap();
        assert_eq!(ev.focus, ElicitationFocus::Fields);
        assert_eq!(ev.field_cursor(), 0);

        let _ = agent.handle_elicitation_key(&KeyEvent::new(code, modifiers));
        let ev = agent.elicitation_view.as_ref().unwrap();
        assert_eq!(ev.focus, ElicitationFocus::Actions);
        assert_eq!(
            ev.action_focus,
            ElicitationActionFocus::Decline,
            "Shift+Tab from the first field wraps to Decline ({code:?})"
        );

        let _ = agent.handle_elicitation_key(&KeyEvent::new(code, modifiers));
        let ev = agent.elicitation_view.as_ref().unwrap();
        assert_eq!(ev.focus, ElicitationFocus::Actions);
        assert_eq!(ev.action_focus, ElicitationActionFocus::Accept);

        let _ = agent.handle_elicitation_key(&KeyEvent::new(code, modifiers));
        let ev = agent.elicitation_view.as_ref().unwrap();
        assert_eq!(ev.focus, ElicitationFocus::Fields);
        assert_eq!(ev.field_cursor(), 1);

        let _ = agent.handle_elicitation_key(&KeyEvent::new(code, modifiers));
        let ev = agent.elicitation_view.as_ref().unwrap();
        assert_eq!(ev.focus, ElicitationFocus::Fields);
        assert_eq!(ev.field_cursor(), 0);
    }
}
