//! Concise variant of the `run_terminal_cmd` (bash) tool.

use strip_ansi_escapes::strip_str;

use crate::implementations::echo_agent_build::bash::{
    BashTool, BashToolInput, BashToolOutput, KillReason,
};
use crate::types::output::BashOutput;
use crate::types::requirements::{Expr, ToolParamsRequirement, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};

use crate::util::truncate::format_bytes;

fn annotations(bash: &BashOutput) -> String {
    let mut s = String::new();
    if bash.truncated {
        let shown = format_bytes(bash.output.len() as u64);
        let total = format_bytes(bash.total_bytes as u64);
        s.push_str(&format!(
            " [truncated: showing last {} of {} - full output at: {}]",
            shown, total, bash.output_file
        ));
    }
    if let Some(signal) = &bash.signal {
        // Synthetic kill reasons are conveyed by the `Exit code: killed (reason)`
        // header — suppress the redundant `[signal=…]` / `[timeout]` here.
        if signal.parse::<KillReason>().is_err() {
            s.push_str(&format!(" [signal={}]", signal));
        }
    }
    s
}

/// CONCISE foreground format: `Exit code: N [annotations]\n\nCommand output:\n\n```...```\n\nCommand completed.\n...`
///
/// When the process was killed by the harness or a kernel signal
/// (see [`KillReason`]), the header reads
/// `Exit code: killed (reason)` instead of `Exit code: -1 [signal=…]`.
fn format_concise_foreground_prompt(bash: &BashOutput) -> String {
    let raw = String::from_utf8_lossy(&bash.output);
    let output_str = strip_str(&raw).to_string();
    let header = match bash
        .signal
        .as_deref()
        .and_then(|s| s.parse::<KillReason>().ok())
    {
        Some(reason) => format!("Exit code: killed ({}){}", reason, annotations(bash)),
        None => format!("Exit code: {}{}", bash.exit_code, annotations(bash)),
    };
    format!(
        "{}\n\n\
         Command output:\n\n\
         ```\n{}\n```\n\n\
         Command completed.\n\n\
         The previous shell command ended, so on the next invocation of this tool, \
         you will be using a new shell session.\n\n\
         On the next terminal tool call, the directory of the shell will be {}.",
        header, output_str, bash.current_dir
    )
}

/// CONCISE backgrounded format: same as DEFAULT backgrounded (code-fenced partial output).
fn format_concise_background_prompt(bash: &BashOutput) -> String {
    let raw = String::from_utf8_lossy(&bash.output);
    let output_str = strip_str(&raw).to_string();
    let shown = format_bytes(bash.output.len() as u64);
    let total = format_bytes(bash.total_bytes as u64);
    format!(
        "[Command moved to background]\n\n\
         Partial output ({} of {} total):\n\n\
         ```\n{}\n```\n\n\
         The command is still running in the background. You can continue with other tasks.\n\
         Full output is being written to: {}\n\n\
         On the next terminal tool call, the directory of the shell will be {}.",
        shown, total, output_str, bash.output_file, bash.current_dir
    )
}

/// Concise variant of `BashTool`.
///
/// Delegates to `BashTool::run()`, then overwrites `output_for_prompt` with
/// the concise format. The `concise` concept lives entirely in this file.
#[derive(Debug, Default)]
pub struct BashConciseTool;

impl crate::types::tool_metadata::ToolMetadata for BashConciseTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuildConcise
    }

    fn description_template(&self) -> &str {
        crate::types::tool_metadata::ToolMetadata::description_template(&BashTool)
    }

    fn emitted_notifications(&self) -> &'static [&'static str] {
        crate::types::tool_metadata::ToolMetadata::emitted_notifications(&BashTool)
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::And(vec![
            Expr::Value(ToolRequirement::if_params(
                ToolParamsRequirement::new("enabled_background", true),
                ToolRequirement::tool_kind(ToolKind::BackgroundTaskAction),
            )),
            Expr::Value(ToolRequirement::if_params(
                ToolParamsRequirement::new("enabled_background", true),
                ToolRequirement::tool_kind(ToolKind::KillTaskAction),
            )),
        ])
    }
}

impl echo_agent_tool_runtime::Tool for BashConciseTool {
    type Args = BashToolInput;
    type Output = BashToolOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("run_terminal_cmd").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "run_terminal_cmd",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.run_terminal_cmd_concise", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: BashToolInput,
    ) -> Result<BashToolOutput, echo_agent_tool_runtime::ToolError> {
        let result = echo_agent_tool_runtime::Tool::run(&BashTool, ctx, input).await?;

        match result {
            // TODO: Add different concise message for auto backgrounded terminal task
            BashToolOutput::Foreground(mut bash) => {
                let is_backgrounded = bash.signal.as_deref() == Some("backgrounded");
                bash.output_for_prompt = if is_backgrounded {
                    format_concise_background_prompt(&bash)
                } else {
                    format_concise_foreground_prompt(&bash)
                };
                Ok(BashToolOutput::Foreground(bash))
            }
            bg @ BashToolOutput::Background(_) => Ok(bg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bash(exit_code: i32, output: &str) -> BashOutput {
        BashOutput {
            output: output.as_bytes().to_vec(),
            output_for_prompt: BashOutput::make_output_for_prompt(output),
            exit_code,
            command: "echo test".to_string(),
            truncated: false,
            signal: None,
            timed_out: false,
            description: None,
            current_dir: "/tmp".to_string(),
            output_file: String::new(),
            total_bytes: output.len(),
            output_delta: None,
            was_bare_echo: false,
        }
    }

    #[test]
    fn concise_prompt_starts_with_exit_code() {
        let bash = make_bash(0, "hello world\n");
        let prompt = format_concise_foreground_prompt(&bash);
        assert!(
            prompt.starts_with("Exit code: 0"),
            "CONCISE should start with 'Exit code: 0', got: {prompt}"
        );
        assert!(prompt.contains("```"), "CONCISE should have code fences");
        assert!(prompt.contains("Command completed."));
        assert!(!prompt.contains("exit: 0"));
    }

    #[test]
    fn concise_prompt_no_double_header() {
        let bash = make_bash(0, "hello\n");
        let prompt = format_concise_foreground_prompt(&bash);
        assert_eq!(
            prompt.matches("Exit code:").count(),
            1,
            "Should have exactly one 'Exit code:' header, got: {prompt}"
        );
        assert!(
            !prompt.contains("exit: "),
            "Should NOT contain DEFAULT 'exit: ' header, got: {prompt}"
        );
    }

    #[test]
    fn concise_timeout_annotations() {
        let mut bash = make_bash(-1, "partial\n");
        bash.signal = Some("timeout".to_string());
        bash.timed_out = true;
        let prompt = format_concise_foreground_prompt(&bash);
        // Synthetic kill reasons render as `Exit code: killed (reason)` —
        // no redundant `[signal=…]` / `[timeout]` annotation.
        assert!(
            prompt.starts_with("Exit code: killed (timeout)"),
            "expected `Exit code: killed (timeout)` header, got: {prompt}"
        );
        assert!(!prompt.contains("[signal=timeout]"));
        assert!(!prompt.contains("[timeout]"));
        assert!(!prompt.starts_with("Exit code: -1"));
    }

    #[test]
    fn concise_killed_reasons() {
        for reason in ["timeout", "max_runtime", "cancelled", "killed", "signal 15"] {
            let mut bash = make_bash(-1, "partial\n");
            bash.signal = Some(reason.to_string());
            let prompt = format_concise_foreground_prompt(&bash);
            let expected = format!("Exit code: killed ({})", reason);
            assert!(
                prompt.starts_with(&expected),
                "expected header `{expected}`, got: {prompt}"
            );
            assert!(!prompt.contains("[signal="));
        }
    }

    #[test]
    fn concise_backgrounded() {
        let mut bash = make_bash(0, "partial output\n");
        bash.signal = Some("backgrounded".to_string());
        bash.output_file = "/tmp/bg.log".to_string();
        bash.total_bytes = 10000;
        let prompt = format_concise_background_prompt(&bash);
        assert!(prompt.starts_with("[Command moved to background]"));
        assert!(prompt.contains("```"));
        assert!(prompt.contains("still running in the background"));
    }

    #[test]
    fn no_double_header_after_default_prebake() {
        // Simulate the real flow: BashTool pre-bakes DEFAULT prompt into
        // output_for_prompt, then BashConciseTool overwrites with concise.
        // to_prompt_format() is a passthrough — it must NOT add another header.
        let mut bash = make_bash(0, "hello world\n");
        // Pre-bake DEFAULT (what BashTool::run() does)
        bash.output_for_prompt =
            crate::implementations::echo_agent_build::bash::format_default_prompt(&bash);
        assert!(bash.output_for_prompt.starts_with("exit: 0"));

        // Concise post-processing (what BashConciseTool::run() does)
        bash.output_for_prompt = format_concise_foreground_prompt(&bash);

        // to_prompt_format() is a passthrough
        let prompt = crate::types::output::ToolOutput::Bash(bash).to_prompt_format();
        assert!(
            prompt.starts_with("Exit code: 0"),
            "Should start with concise header, got: {prompt}"
        );
        assert_eq!(
            prompt.matches("exit:").count() + prompt.matches("Exit code:").count(),
            1,
            "Should have exactly one header, got: {prompt}"
        );
    }
}
