//! Task output tool — old `impl Tool` deleted.
//! Helper functions remain for use by `echo_agent_build/task_output/`.

use crate::computer::types::TaskSnapshot;
use crate::types::process_manager::format_system_time_rfc3339;
use crate::util::truncate::{
    DEFAULT_SOFT_WRAP_WIDTH, PREVIEW_SIZE, soft_wrap_lines, truncate_with_preview,
};
use echo_agent_tool_types::TaskOutputResult;

/// Convert a TaskSnapshot to a TaskOutputResult, with output truncation.
pub(crate) fn snapshot_to_result(
    s: TaskSnapshot,
    read_file_tool_name: &str,
    max_output_bytes: usize,
) -> TaskOutputResult {
    let output_view = s.output_view();
    let raw_output_bytes = output_view.total_bytes();

    // Truncate output to protect model's context window
    let (output, truncated) = if s.output.len() > max_output_bytes {
        truncate_with_preview(
            output_view,
            max_output_bytes,
            PREVIEW_SIZE,
            Some(&format!(
                "Use {} on {} for full content",
                read_file_tool_name,
                s.output_file.display()
            )),
        )
    } else {
        // Soft-wrap long lines for model comprehension.
        // Output is already size-bounded (30KB via bash backend,
        // 400KB safety net for ACP). Wrapping adds structure
        // without losing content.
        (
            soft_wrap_lines(&s.output, DEFAULT_SOFT_WRAP_WIDTH),
            s.truncated,
        )
    };

    let truncation_hint = format!(
        "[truncated - use {} on output_file for full content]",
        read_file_tool_name
    );

    // Compute duration before moving fields.
    let duration_secs = s.duration_secs();

    TaskOutputResult {
        task_id: s.task_id,
        command: s.display_command.unwrap_or(s.command),
        status: if s.completed {
            if s.explicitly_killed {
                // A user-initiated kill is not a failure; report a distinct
                // status so callers don't treat an intentional kill as an
                // error. Matches the subagent "cancelled" status.
                "cancelled"
            } else if s.signal.as_deref() == Some("timeout") {
                // Wrapper-timeout kill (backend sets the sentinel "timeout"
                // signal); a generic "failed" would hide why the task died.
                "timed_out"
            } else if s.exit_code == Some(0) {
                "completed"
            } else {
                "failed"
            }
        } else {
            "running"
        }
        .to_string(),
        exit_code: s.exit_code,
        started: format_system_time_rfc3339(s.start_time),
        ended: s.end_time.map(format_system_time_rfc3339),
        duration_secs,
        output,
        output_file: s.output_file.display().to_string(),
        truncated,
        truncation_hint,
        raw_output_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_TOOL_OUTPUT_BYTES;
    use crate::types::output::ToolOutput;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn make_test_snapshot(task_id: &str, completed: bool, exit_code: Option<i32>) -> TaskSnapshot {
        TaskSnapshot {
            task_id: task_id.to_string(),
            command: "echo hello".to_string(),
            display_command: None,
            cwd: "/tmp".to_string(),
            start_time: SystemTime::now(),
            end_time: if completed {
                Some(SystemTime::now())
            } else {
                None
            },
            output: "test output".to_string(),
            output_file: PathBuf::from(format!("/tmp/{}.log", task_id)),
            truncated: false,
            exit_code,
            signal: None,
            completed,
            kind: Default::default(),
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        }
    }

    /// A snapshot that holds only part of a large log must still report the
    /// task's real size, which polling uses to tell progress from a stall.
    #[test]
    fn raw_output_bytes_reports_the_task_total_not_the_part_held() {
        let mut snapshot = make_test_snapshot("test-1", true, Some(0));
        snapshot.output = "x".repeat(1024);
        snapshot.output_total_bytes = 5 * 1024 * 1024;
        snapshot.truncated = true;

        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);

        assert_eq!(result.raw_output_bytes, 5 * 1024 * 1024);
    }

    #[test]
    fn test_snapshot_to_result_running() {
        let snapshot = make_test_snapshot("test-1", false, None);
        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);

        assert_eq!(result.task_id, "test-1");
        assert_eq!(result.status, "running");
        assert!(result.exit_code.is_none());
        assert!(result.ended.is_none());
    }

    #[test]
    fn test_snapshot_to_result_completed_success() {
        let snapshot = make_test_snapshot("test-1", true, Some(0));
        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);

        assert_eq!(result.status, "completed");
        assert_eq!(result.exit_code, Some(0));
        assert!(result.ended.is_some());
    }

    #[test]
    fn test_snapshot_to_result_completed_failed() {
        let snapshot = make_test_snapshot("test-1", true, Some(1));
        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);

        assert_eq!(result.status, "failed");
        assert_eq!(result.exit_code, Some(1));
    }

    #[test]
    fn test_snapshot_to_result_killed_is_cancelled_not_failed() {
        let mut snapshot = make_test_snapshot("test-1", true, None);
        snapshot.explicitly_killed = true;
        snapshot.signal = Some("killed".to_string());
        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);

        // An intentional kill must be distinct from a genuine failure.
        assert_eq!(result.status, "cancelled");
        assert_ne!(result.status, "failed");
    }

    /// A wrapper-timeout kill (backend sets the sentinel "timeout" signal,
    /// no exit code) must surface as "timed_out", not a generic "failed".
    #[test]
    fn test_snapshot_to_result_timeout_kill_is_timed_out() {
        let mut snapshot = make_test_snapshot("test-1", true, None);
        snapshot.signal = Some("timeout".to_string());
        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);

        assert_eq!(result.status, "timed_out");
        assert_eq!(result.exit_code, None);
    }

    /// An explicit kill wins over the timeout signal: the model asked for the
    /// kill, so it must see "cancelled" regardless of how the process died.
    #[test]
    fn test_snapshot_to_result_explicit_kill_beats_timeout_signal() {
        let mut snapshot = make_test_snapshot("test-1", true, None);
        snapshot.explicitly_killed = true;
        snapshot.signal = Some("timeout".to_string());
        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);

        assert_eq!(result.status, "cancelled");
    }

    /// Empty output renders "(no output yet)" only while running; terminal
    /// states must not imply more output may arrive.
    #[test]
    fn test_no_output_wording_tracks_task_state() {
        let mut running = make_test_snapshot("test-1", false, None);
        running.output = String::new();
        let result = snapshot_to_result(running, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);
        let prompt =
            ToolOutput::TaskOutput(echo_agent_tool_types::TaskOutputOutput::Result(result))
                .to_prompt_format();
        assert!(prompt.contains("(no output yet)"), "running: {prompt}");

        let mut timed_out = make_test_snapshot("test-2", true, None);
        timed_out.output = String::new();
        timed_out.signal = Some("timeout".to_string());
        let result = snapshot_to_result(timed_out, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);
        let prompt =
            ToolOutput::TaskOutput(echo_agent_tool_types::TaskOutputOutput::Result(result))
                .to_prompt_format();
        assert!(prompt.contains("Status: timed_out"), "prompt: {prompt}");
        assert!(prompt.contains("(no output)"), "prompt: {prompt}");
        assert!(!prompt.contains("(no output yet)"), "prompt: {prompt}");
    }

    #[test]
    fn test_snapshot_to_result_truncates_large_output() {
        let mut snapshot = make_test_snapshot("test-1", true, Some(0));
        snapshot.output = "x".repeat(500_000);

        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);

        assert!(result.truncated);
        assert!(result.output.contains("[Output truncated"));
        assert!(result.output.len() < 10_000);
    }

    #[test]
    fn test_snapshot_to_result_uses_resolved_tool_name() {
        let mut snapshot = make_test_snapshot("test-1", true, Some(0));
        snapshot.output = "x".repeat(500_000);

        let result = snapshot_to_result(snapshot, "custom_reader", DEFAULT_TOOL_OUTPUT_BYTES);

        assert!(result.output.contains("custom_reader"));
        assert!(result.truncation_hint.contains("custom_reader"));
    }

    #[test]
    fn test_snapshot_to_result_wraps_long_lines() {
        let long_line = "Q".repeat(5_000);
        let mut snapshot = make_test_snapshot("test-1", true, Some(0));
        snapshot.output = long_line;

        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);

        // All content preserved in the output field
        let q_count = result.output.chars().filter(|c| *c == 'Q').count();
        assert_eq!(q_count, 5_000);

        // Prompt rendering also preserves content
        let output =
            ToolOutput::TaskOutput(echo_agent_tool_types::TaskOutputOutput::Result(result));
        let prompt = output.to_prompt_format();
        let q_count = prompt.chars().filter(|c| *c == 'Q').count();
        assert_eq!(q_count, 5_000);
        // No individual line exceeds the wrap width (+ some slack for header lines)
        for line in prompt.lines() {
            assert!(line.len() <= 2_100, "Line too long: {} chars", line.len());
        }
    }

    #[test]
    fn test_snapshot_to_result_preserves_short_output() {
        let snapshot = make_test_snapshot("test-1", true, Some(0));
        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);

        // Short output is not modified
        assert_eq!(result.output, "test output");
        assert!(!result.truncated);
    }

    #[test]
    fn test_snapshot_to_result_respects_custom_max_output_bytes() {
        let mut snapshot = make_test_snapshot("test-1", true, Some(0));
        // Output is 10KB — above our 5KB custom limit, below the 40KB default.
        snapshot.output = "x".repeat(10_000);

        // With the default 40KB limit, output should NOT be truncated.
        let result_default =
            snapshot_to_result(snapshot.clone(), "read_file", DEFAULT_TOOL_OUTPUT_BYTES);
        assert!(
            !result_default.truncated,
            "10KB output should not be truncated with 40KB default limit"
        );

        // With a custom 5KB limit, output SHOULD be truncated.
        let result_custom = snapshot_to_result(snapshot, "read_file", 5_000);
        assert!(
            result_custom.truncated,
            "10KB output should be truncated with 5KB custom limit"
        );
        assert!(
            result_custom.output.contains("[Output truncated"),
            "should contain truncation marker"
        );
        assert!(
            result_custom.output.len() < 5_000,
            "truncated output should be much smaller than 10KB"
        );
    }

    // ── display_command preference ──────────────────────────────────

    /// When `display_command` is set (isolation-wrapped command), the model
    /// should see the original user command, not the `unshare`/mount wrapper.
    #[test]
    fn test_snapshot_to_result_prefers_display_command() {
        let mut snapshot = make_test_snapshot("test-dc", true, Some(0));
        snapshot.command = "unshare -m /bin/bash -c 'mount ...; exec cargo test'".to_string();
        snapshot.display_command = Some("cargo test -p echo-agent-runtime".to_string());

        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);
        assert_eq!(
            result.command, "cargo test -p echo-agent-runtime",
            "model-facing command must be the display (original) command"
        );
    }

    /// When `display_command` is `None` (no isolation wrapping), the actual
    /// executed command is shown as before.
    #[test]
    fn test_snapshot_to_result_falls_back_to_command_when_no_display() {
        let snapshot = make_test_snapshot("test-no-dc", true, Some(0));
        // make_test_snapshot sets display_command = None, command = "echo hello"
        let result = snapshot_to_result(snapshot, "read_file", DEFAULT_TOOL_OUTPUT_BYTES);
        assert_eq!(
            result.command, "echo hello",
            "without display_command, the actual command must be used"
        );
    }
}
