use std::time::Duration;

use ratatui::text::Span;

use super::{
    HookRunEntry, HookRunStatus, ToolCallHookData, render_hooks_inline_suffix,
    render_stop_hooks_summary,
};

fn run(status: HookRunStatus) -> HookRunEntry {
    HookRunEntry {
        name: "hook".to_owned(),
        status,
        output: None,
    }
}

fn text(spans: Vec<Span<'static>>) -> String {
    let mut text = String::new();
    for span in spans {
        text.push_str(span.content.as_ref());
    }
    text
}

#[test]
fn compact_suffix_keeps_blocked_and_failure_formatting() {
    let elapsed = Duration::from_millis(1);
    let data = ToolCallHookData {
        post_hooks: vec![
            run(HookRunStatus::Blocked {
                detail: "denied".to_owned(),
                elapsed,
            }),
            run(HookRunStatus::Failed {
                error: "exit 1".to_owned(),
                elapsed,
            }),
        ],
        ..ToolCallHookData::default()
    };
    assert_eq!(
        text(render_hooks_inline_suffix(&data).expect("hook suffix")),
        "  [hooks: 1/1]"
    );
    let stop_groups = [("stop".to_owned(), data.post_hooks)];
    assert_eq!(
        text(render_stop_hooks_summary(&stop_groups).expect("stop suffix")),
        "stop  [hooks: 1/1]"
    );
}
