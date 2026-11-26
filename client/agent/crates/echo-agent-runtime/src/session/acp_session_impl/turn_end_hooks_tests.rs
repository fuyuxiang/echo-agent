use super::{
    TURN_END_DRAIN_BUDGET, cancel_details, cancel_reason_for_completion, cancel_reason_for_options,
};
use crate::session::CancelOptions;
use crate::session::CancelTrigger as T;
use crate::session::commands::PromptCompletionKind;
use crate::session::events::CancellationCategory as Cat;
use echo_agent_hooks::event::StopCancelledReason as Reason;

fn cancelled(category: Option<Cat>) -> PromptCompletionKind {
    PromptCompletionKind::Cancelled {
        category,
        context: None,
    }
}

fn options(trigger: Option<T>, user_initiated: bool) -> CancelOptions {
    CancelOptions {
        trigger,
        user_initiated,
        ..Default::default()
    }
}

#[test]
fn classifies_every_cancel() {
    let cases = [
        (options(Some(T::CtrlC), true), Some(Reason::UserInterrupt)),
        (options(Some(T::Esc), true), Some(Reason::UserInterrupt)),
        (
            options(Some(T::Client("stop_button".into())), false),
            Some(Reason::UserInterrupt),
        ),
        (options(Some(T::SendNow), false), None),
        (options(Some(T::Shutdown), false), None),
        (options(Some(T::SessionClose), false), None),
        (options(Some(T::SessionDelete), false), None),
        (options(None, true), Some(Reason::UserInterrupt)),
        (options(None, false), None),
    ];
    for (options, expected) in cases {
        assert_eq!(
            cancel_reason_for_options(&options),
            expected,
            "{:?} user_initiated={}",
            options.trigger,
            options.user_initiated
        );
    }
}

#[test]
fn classifies_every_completion_kind() {
    let cases = [
        (
            cancelled(Some(Cat::PermissionRejected)),
            Some(Reason::PermissionRejected),
        ),
        (
            cancelled(Some(Cat::PermissionCancelled)),
            Some(Reason::PermissionCancelled),
        ),
        (
            cancelled(Some(Cat::MidTurnAbort)),
            Some(Reason::UserInterrupt),
        ),
        (cancelled(None), Some(Reason::Unknown)),
        (cancelled(Some(Cat::HookDenied)), Some(Reason::Unknown)),
        (
            PromptCompletionKind::MaxTurnsReached { limit: 5 },
            Some(Reason::MaxTurns),
        ),
        (
            PromptCompletionKind::StationarityEnded,
            Some(Reason::NoProgress),
        ),
        (PromptCompletionKind::Completed, None),
        (PromptCompletionKind::Rewound, None),
        (PromptCompletionKind::RemovedFromQueue, None),
    ];
    for (kind, expected) in cases {
        assert_eq!(cancel_reason_for_completion(&kind), expected);
    }
}

#[test]
fn the_drain_budget_leaves_room_for_the_upload_tail() {
    use crate::session::feedback_manager::{SHUTDOWN_DRAIN_HARD_MAX, SHUTDOWN_SIGNAL_SYNC_TIMEOUT};
    assert!(
        // Twice: teardown flushes for ordering, then drains to close.
        2 * TURN_END_DRAIN_BUDGET + SHUTDOWN_SIGNAL_SYNC_TIMEOUT + SHUTDOWN_DRAIN_HARD_MAX
            < crate::agent::activity::SESSION_FLUSH_GRACE
    );
}

#[test]
fn cancel_detail_names_the_subject_and_reason() {
    let with = |tool: Option<&str>, hook: Option<&str>, reason: Option<&str>| {
        cancel_details(&PromptCompletionKind::Cancelled {
            category: None,
            context: Some(crate::session::commands::CancellationContext {
                tool_name: tool.map(str::to_string),
                hook_name: hook.map(str::to_string),
                reason: reason.map(str::to_string),
                trigger: None,
            }),
        })
    };
    assert_eq!(
        with(Some("read_file"), None, Some("user declined")),
        Some("read_file: user declined".into())
    );
    assert_eq!(with(None, Some("verify"), None), Some("verify".into()));
    assert_eq!(
        with(None, None, Some("no progress")),
        Some("no progress".into())
    );
    assert_eq!(cancel_details(&cancelled(None)), None);
}
