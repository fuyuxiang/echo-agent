//! Who the keyboard reaches, and what `Esc` means once it gets there.
//!
//!

use super::{AgentPane, AgentView};
use crate::app::app_view::InputOutcome;
use crate::views::permission_view::{PermissionFocus, PermissionViewState};
use crate::views::question_view::{QuestionFocus, QuestionViewState};
use crate::views::shortcuts_bar::HintItem;

/// A card that blocks the agent until the user answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockingCard {
    Permission,
    CancelTurn,
    Question,
    McpElicitation,
}

impl BlockingCard {
    /// The scrollback's focus hint while this card is parked: `Tab` hands the
    /// keyboard back, and the label names the card rather than the prompt.
    /// Pinned, so a narrow bar's trim can never drop the only route back.
    pub(crate) fn focus_hint(self) -> HintItem {
        let label = match self {
            Self::Permission => "permission",
            Self::CancelTurn => "cancel turn",
            Self::Question => "question",
            Self::McpElicitation => "elicitation",
        };
        HintItem {
            keys: vec![crate::key!(Tab), crate::key!(' ')],
            label: label.into(),
            custom_display: Some("Tab/Space"),
            description: None,
            pinned: true,
        }
    }
}

/// Which surface the keyboard reaches, in the order [`AgentView::handle_input`]
/// asks for it.
///
/// The fullscreen takeovers ahead of these — the subagent view, the media
/// viewers, `/gboom`, and the modal stack — answer for themselves and draw
/// their own chrome, so they are not ranked here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyOwner {
    /// An open line viewer: the plan preview, or a file preview from the
    /// prompt. Ranks below Permission (so followup can type) and above other
    /// cards; forwards keys to the plan-approval prompt when that has focus.
    LineViewer,
    BlockViewer,
    /// A blocking card with the keyboard. Permission outranks the line viewer
    Card(BlockingCard),
    /// The plan-approval prompt with its preview closed.
    PlanApproval,
    /// Nothing is blocking: the focused pane and the global bindings.
    Pane,
}

/// What `Esc` does on the focused card right now, one rung at a time: clear
/// whatever the card has pending, then leave it.
///
/// Every hint that names `Esc` on a card reads this, and every handler that
/// owns the key dispatches on it ([`AgentView::handle_card_esc`]), so the bar
/// cannot promise a rung the key does not take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EscStep {
    /// Close the `@` file-search dropdown over a card's text input.
    DismissFileSearch,
    /// Leave the card's text input for its rows (question free-text answer,
    /// permission followup message).
    LeaveTextInput,
    /// Close the bare `/feedback` pane, which has no rows to leave the input for.
    DismissFeedbackPane,
    /// Skip the `/feedback` trace question (the report still sends).
    SkipFeedbackTrace,
    /// Throw away an in-progress always-allow pattern edit.
    DiscardPatternEdit,
    /// Unmark this question's answer.
    ClearSelection,
    /// Return to the dashboard, leaving the card pending. Owned by the
    /// dashboard overlay's own cascade, which runs ahead of the router.
    BackOutOverlay,
    /// Hand the keyboard to the scrollback with the card still drawn.
    ParkFocus,
    /// Dismiss the cancel-turn panel and leave the turn (and subagents) running.
    KeepRunning,
    DismissElicitWaiting,
}

impl EscStep {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::DismissFileSearch => "dismiss",
            Self::LeaveTextInput => "back",
            Self::DismissFeedbackPane => "dismiss",
            Self::SkipFeedbackTrace => "skip",
            Self::DiscardPatternEdit => "cancel",
            Self::ClearSelection => "unselect",
            Self::BackOutOverlay => "dashboard",
            Self::ParkFocus => "scrollback",
            Self::KeepRunning => "keep running",
            Self::DismissElicitWaiting => "dismiss",
        }
    }
}

impl AgentView {
    /// The card that is drawn, focused or parked. Permission first: a
    /// permission request interrupts whatever else is waiting.
    pub(crate) fn blocking_card(&self) -> Option<BlockingCard> {
        if !self.permission_queue.is_empty() {
            Some(BlockingCard::Permission)
        } else if self.cancel_turn_view.is_some() {
            Some(BlockingCard::CancelTurn)
        } else if self.question_view.is_some() {
            Some(BlockingCard::Question)
        } else if self.elicitation_view.is_some() {
            Some(BlockingCard::McpElicitation)
        } else {
            None
        }
    }

    pub(crate) fn key_owner(&self) -> KeyOwner {
        self.key_owner_when_parked(self.active_pane == AgentPane::Scrollback)
    }

    /// The ranking itself. `parked` — the keyboard sitting in the scrollback —
    /// is what takes a card or the plan approval out of the running, so
    /// [`Self::parked_card`] can ask the same question with it set false to
    /// learn who the keyboard would come back to.
    fn key_owner_when_parked(&self, parked: bool) -> KeyOwner {
        let card = self.blocking_card().filter(|_| !parked);
        if card == Some(BlockingCard::Permission) {
            KeyOwner::Card(BlockingCard::Permission)
        } else if self.line_viewer.is_some() {
            KeyOwner::LineViewer
        } else if self.block_viewer.is_some() {
            KeyOwner::BlockViewer
        } else if self.plan_approval_view.is_some() && !parked {
            KeyOwner::PlanApproval
        } else if let Some(card) = card {
            KeyOwner::Card(card)
        } else {
            KeyOwner::Pane
        }
    }

    /// The card the keys actually reach — `None` while something above it in
    /// [`Self::key_owner`] holds them, or while the card is parked.
    pub(crate) fn focused_card(&self) -> Option<BlockingCard> {
        match self.key_owner() {
            KeyOwner::Card(card) => Some(card),
            _ => None,
        }
    }

    /// A card that is still drawn and still waiting, with the keyboard handed
    /// to the scrollback so the context behind it can be read — and that the
    /// keyboard would come back to.
    ///
    /// Asked through the same ranking as [`Self::key_owner`], because the
    /// scrollback's focus hint names where `Tab` goes: a card the plan
    /// approval outranks is not the route back, and naming it would be the
    /// exact mismatch this ordering exists to prevent.
    pub(crate) fn parked_card(&self) -> Option<BlockingCard> {
        if self.active_pane != AgentPane::Scrollback {
            return None;
        }
        match self.key_owner_when_parked(false) {
            KeyOwner::Card(card) => Some(card),
            _ => None,
        }
    }

    pub(crate) fn focused_permission(&self) -> Option<&PermissionViewState> {
        self.permission_queue
            .front()
            .filter(|_| self.focused_card() == Some(BlockingCard::Permission))
    }

    pub(crate) fn focused_question(&self) -> Option<&QuestionViewState> {
        self.question_view
            .as_ref()
            .filter(|_| self.focused_card() == Some(BlockingCard::Question))
    }

    pub(crate) fn card_esc(&self) -> Option<EscStep> {
        Some(match self.focused_card()? {
            BlockingCard::Permission => match self.permission_queue.front()?.focus {
                PermissionFocus::FollowupInput => EscStep::LeaveTextInput,
                PermissionFocus::PatternEdit => EscStep::DiscardPatternEdit,
                PermissionFocus::Options => EscStep::ParkFocus,
            },
            // Never a dead end: Esc closes the panel and keeps the turn
            // running. Enter / 1–4 still pick a cancel-and-subagent choice.
            BlockingCard::CancelTurn => EscStep::KeepRunning,
            BlockingCard::Question => {
                let qv = self.question_view.as_ref()?;
                if qv.focus == QuestionFocus::InputMode {
                    if self.prompt.file_search_visible() {
                        EscStep::DismissFileSearch
                    } else if qv.is_feedback() {
                        EscStep::DismissFeedbackPane
                    } else {
                        EscStep::LeaveTextInput
                    }
                } else if qv.is_feedback_trace() {
                    // Defaults to a selection, so the generic ladder would
                    // read Esc as unselect.
                    EscStep::SkipFeedbackTrace
                } else if qv.is_feedback_report() {
                    // Safety net: the report stage stays in InputMode by
                    // design, so this arm only fires if that ever changes.
                    EscStep::DismissFeedbackPane
                } else if qv.active_tab_has_selection() {
                    EscStep::ClearSelection
                } else if self.in_dashboard_overlay && qv.active_tab == 0 {
                    // On later questions `Left` still walks back, so `Esc`
                    // stays in the card.
                    EscStep::BackOutOverlay
                } else {
                    EscStep::ParkFocus
                }
            }
            BlockingCard::McpElicitation => {
                use crate::views::elicitation_view::ElicitationFocus;
                let ev = self.elicitation_view.as_ref()?;
                if ev.focus == ElicitationFocus::Editing {
                    EscStep::LeaveTextInput
                } else if ev.is_url_waiting() {
                    EscStep::DismissElicitWaiting
                } else {
                    EscStep::ParkFocus
                }
            }
        })
    }

    /// Take one rung of the focused card's `Esc` ladder.
    pub(crate) fn handle_card_esc(&mut self) -> InputOutcome {
        let Some(step) = self.card_esc() else {
            return InputOutcome::Unchanged;
        };
        match step {
            EscStep::DismissFileSearch => self.prompt.file_search.clear_context(),
            EscStep::LeaveTextInput => {
                if self.focused_card() == Some(BlockingCard::Permission) {
                    self.permission_back_to_options();
                } else if self.focused_card() == Some(BlockingCard::McpElicitation) {
                    if let Some(ev) = self.elicitation_view.as_mut() {
                        ev.focus = crate::views::elicitation_view::ElicitationFocus::Fields;
                    }
                } else {
                    self.commit_question_freeform();
                }
            }
            EscStep::DismissFeedbackPane | EscStep::SkipFeedbackTrace => {
                return self.submit_question_answers(true);
            }
            EscStep::DiscardPatternEdit => {
                self.permission_pattern_edit = None;
                self.permission_back_to_options();
            }
            EscStep::ClearSelection => {
                if let Some(qv) = self.question_view.as_mut() {
                    let active = qv.active_tab;
                    qv.clear_selection(active);
                }
            }
            // The dashboard overlay's cascade runs ahead of the router and
            // takes this rung itself; reaching it here means the overlay
            // declined, and the card keeps the key rather than letting it
            // fall through to the turn-cancel policy.
            EscStep::BackOutOverlay => {}
            EscStep::ParkFocus => self.park_focused_card(),
            EscStep::KeepRunning => {
                // The bar promises "keep running". Mapping this to
                // ContinueToRun would still cancel the parent turn (only
                // the subagents would survive). Close the panel instead.
                self.cancel_turn_view = None;
                self.cancel_turn_buttons.clear();
            }
            EscStep::DismissElicitWaiting => return self.resolve_elicitation_cancel(),
        }
        InputOutcome::Changed
    }

    fn permission_back_to_options(&mut self) {
        if let Some(perm) = self.permission_queue.front_mut() {
            perm.focus = PermissionFocus::Options;
        }
    }

    /// Hand the keyboard to the scrollback with the card still drawn.
    ///
    /// Forced past the queued-prompt edit lock: opening a card stashes the
    /// composer and blanks it without leaving `EditingQueued`, so an unforced
    /// switch would read the blank as a dirty edit and answer the card's only
    /// keyboard exit with a "press Enter to save" toast.
    pub(crate) fn park_focused_card(&mut self) {
        self.set_active_pane(AgentPane::Scrollback, true);
    }
}

#[cfg(test)]
#[path = "key_owner_tests.rs"]
mod tests;
