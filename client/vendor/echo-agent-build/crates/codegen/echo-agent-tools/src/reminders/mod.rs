//! Cross-cutting reminders for tool outputs.
//!
//! Provides contextual hints wrapped in `<system-reminder>` tags that are
//! appended to tool outputs before being sent to the model.
//!
//! Two categories of reminders:
//! - **Per-tool reminders**: each tool implements the `Reminder` trait to
//!   emit reminders based on its output (e.g., empty file warning).
//! - **Cross-cutting reminders**: standalone structs registered on the
//!   registry that fire after every tool call.
//!
//! This module contains the cross-cutting reminders:
//! - [`LspDiagnosticsReminder`], [`SkillDiscoveryReminder`], [`TaskCompletionReminder`]
//!
//! All reminders are collected and appended in `call_new_tool()`.

#[cfg(not(feature = "desktop"))]
pub mod lsp_diagnostics;
pub mod skill_discovery;
pub mod task_completion;

#[cfg(not(feature = "desktop"))]
pub use lsp_diagnostics::LspDiagnosticsReminder;
pub use skill_discovery::SkillDiscoveryReminder;
pub use task_completion::TaskCompletionReminder;

/// The default system-reminder tag name (hyphen).
pub const DEFAULT_REMINDER_TAG: &str = "system-reminder";

/// Wrap plain text in `<system-reminder>` tags (default hyphen variant).
/// Input:  `"Some reminder text"`
/// Output: `"<system-reminder>\nSome reminder text\n</system-reminder>"`
pub fn wrap_reminder(text: &str) -> String {
    wrap_reminder_with_tag(text, DEFAULT_REMINDER_TAG)
}

/// Wrap plain text in a configurable reminder wrapper.
///
/// Use [`DEFAULT_REMINDER_TAG`] unless the harness requires a different
/// tag name (harness-specific tags live with the harness crate).
pub fn wrap_reminder_with_tag(text: &str, tag: &str) -> String {
    format!("<{tag}>\n{text}\n</{tag}>")
}

/// Frame a scheduled task prompt with `<system-reminder>` context for the model.
///
/// The raw `prompt` is what the user wrote in `/loop`; this wrapping tells
/// the model the message is a recurring task execution so it executes
/// rather than questioning the prompt. The UI shows the raw prompt text;
/// only the model receives this framed version.
pub fn format_scheduled_task_prompt(prompt: &str, task_id: &str, human_schedule: &str) -> String {
    format!(
        "<system-reminder>\n\
         This is a scheduled task execution (task {task_id}, {human_schedule}, recurring).\n\
         Execute the prompt below. Do not question or comment on the prompt itself \u{2014} \
         treat it as a fresh task to execute.\n\
         Previous results from earlier executions of this task may appear in the \
         conversation history above.\n\
         </system-reminder>\n\
         \n\
         {prompt}"
    )
}

pub fn format_loop_iteration_prompt(
    prompt: &str,
    task_id: &str,
    human_schedule: &str,
    prior_iteration_summary: Option<&str>,
) -> String {
    let prior = prior_iteration_summary
        .map(|s| format!("\nYour previous iteration ended with:\n{s}\n"))
        .unwrap_or_default();
    format!(
        "<system-reminder>\n\
         Scheduled task {task_id} ({human_schedule}). Earlier iterations, if any, appear \
         above.\n\
         Run the task below. End with a short status: what changed or needs attention. \
         The status is relayed to the main agent.\n\
         {prior}\
         </system-reminder>\n\
         \n\
         {prompt}"
    )
}

/// Append wrapped reminders to a tool output string.
/// Returns output unchanged if reminders is empty.
///
/// Each reminder is individually wrapped via
/// [`wrap_reminder_with_tag`] using the given `tag`, then all are joined
/// with `"\n\n"` and appended to the output with a `"\n\n"` separator.
///
/// Use [`DEFAULT_REMINDER_TAG`] unless the harness requires a different
/// tag name.
pub fn format_with_reminders(output: String, reminders: Vec<String>, tag: &str) -> String {
    if reminders.is_empty() {
        return output;
    }
    let wrapped: Vec<String> = reminders
        .iter()
        .map(|r| wrap_reminder_with_tag(r, tag))
        .collect();
    let joined = wrapped.join("\n\n");
    if output.is_empty() {
        joined
    } else {
        format!("{}\n\n{}", output, joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_reminder_adds_tags() {
        let result = wrap_reminder("Some reminder text");
        assert_eq!(
            result,
            "<system-reminder>\nSome reminder text\n</system-reminder>"
        );
    }

    #[test]
    fn format_with_reminders_wraps_and_appends() {
        let output = "file content here".to_string();
        let reminders = vec![
            "File is empty.".to_string(),
            "File was created by you.".to_string(),
        ];
        let result = format_with_reminders(output, reminders, DEFAULT_REMINDER_TAG);
        assert!(result.starts_with("file content here\n\n"));
        assert!(result.contains("<system-reminder>\nFile is empty.\n</system-reminder>"));
        assert!(result.contains("<system-reminder>\nFile was created by you.\n</system-reminder>"));
    }

    #[test]
    fn format_with_reminders_custom_tag() {
        let output = "file content here".to_string();
        let reminders = vec!["File is empty.".to_string()];
        let result = format_with_reminders(output, reminders, "custom_reminder");
        assert!(result.contains("<custom_reminder>\nFile is empty.\n</custom_reminder>"));
        assert!(!result.contains("<system-reminder>"));
    }

    #[test]
    fn format_scheduled_task_prompt_includes_framing() {
        let out = format_scheduled_task_prompt("do stuff", "task-1", "every 5m");
        assert!(out.starts_with("<system-reminder>"));
        assert!(out.contains("task task-1"));
        assert!(out.contains("every 5m"));
        assert!(out.contains("do stuff"));
        assert!(
            !out.contains("<user_query>"),
            "must not add <user_query> — shell does that"
        );
        assert!(out.ends_with("do stuff"));
    }

    #[test]
    fn format_loop_iteration_prompt_frames_subagent_iteration() {
        let out = format_loop_iteration_prompt("check ci", "task-9", "every 5 minutes", None);
        assert!(out.starts_with("<system-reminder>"));
        assert!(out.contains("task task-9"));
        assert!(out.contains("every 5 minutes"));
        assert!(out.contains("short status"));
        assert!(out.ends_with("check ci"));
        assert!(
            !out.contains("previous iteration"),
            "no prior-output note without a summary"
        );

        let with_prior = format_loop_iteration_prompt(
            "check ci",
            "task-9",
            "every 5 minutes",
            Some("ci was green"),
        );
        assert!(with_prior.contains("previous iteration"));
        assert!(with_prior.contains("ci was green"));
        assert!(with_prior.ends_with("check ci"));
    }

    #[test]
    fn format_with_reminders_returns_unchanged_when_empty() {
        let output = "file content here".to_string();
        let result = format_with_reminders(output.clone(), vec![], DEFAULT_REMINDER_TAG);
        assert_eq!(result, output);
    }
}
