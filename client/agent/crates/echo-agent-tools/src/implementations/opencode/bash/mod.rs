//! `bash` tool — OpenCode namespace.
//!
//! Executes shell commands with optional timeout and working directory
//! override. Delegates to the shared `TerminalBackend` for process
//! management, output streaming, and background task support. The
//! echo-agent-tools-server serves this tool on the stateless local backend
//! (fresh shell per command; persistent shell state is only enabled when
//! `Cursor:Shell` is served), which is what the description documents.
//!
//! ## Resources
//!
//! - `Terminal` — terminal backend for running commands (required)
//! - `Cwd` — default working directory (required)
//! - `ToolCallId` — notification correlation + output file naming (required)
//! - `SessionFolder` — output file path prefix (required)
//! - `SessionEnv` — environment variables (optional, defaults empty)
//! - `NotificationHandle` — bash execution notifications (optional, noop fallback)
//! - `TruncationCfg` — max output bytes override (optional)

use std::path::PathBuf;
use std::time::Duration;

use crate::DEFAULT_TOOL_OUTPUT_CHARS;
use crate::computer::types::TerminalRunRequest;
use crate::notification::types::{
    BashExecutionComplete, BashExecutionFailed, BashExecutionTimeout, BashNotificationBase,
};

use crate::types::output::{BashOutput, BashToolOutput};
use crate::types::requirements::{Expr, ToolRequirement};
#[allow(unused_imports)]
use crate::types::resources::{
    Cwd, NotificationHandle, SessionEnv, SessionFolder, SharedResources, Terminal, TruncationCfg,
};
use crate::types::tool::{ToolKind, ToolNamespace};
use crate::types::tool_io::ToolInput;

// ───────────────────────────────────────────────────────────────────────────
// Constants
// ───────────────────────────────────────────────────────────────────────────

const MAX_TIMEOUT_MS: u64 = 600_000; // 10 minutes
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

// ───────────────────────────────────────────────────────────────────────────
// Description
// ───────────────────────────────────────────────────────────────────────────

const DESCRIPTION: &str = r#"Executes a given bash command in a shell session with optional timeout, ensuring proper handling and security measures. Each command runs in a fresh shell: working directory changes and environment variables do not persist between calls.
IMPORTANT: This tool is for terminal operations like git, npm, docker, etc. DO NOT use it for file operations (reading, writing, editing, searching, finding files) - use the specialized tools for this instead.

Before executing the command, please follow these steps:

1. Directory Verification:
   - If the command will create new directories or files, first use `ls` to verify the parent directory exists and is the correct location
   - For example, before running "mkdir foo/bar", first use `ls foo` to check that "foo" exists and is the intended parent directory

2. Command Execution:
   - Always quote file paths that contain spaces with double quotes (e.g., rm "path with spaces/file.txt")
   - Examples of proper quoting:
     - mkdir "/Users/name/My Documents" (correct)
     - mkdir /Users/name/My Documents (incorrect - will fail)
     - python "/path/with spaces/script.py" (correct)
     - python /path/with spaces/script.py (incorrect - will fail)
   - After ensuring proper quoting, execute the command.
   - Capture the output of the command.

Usage notes:
  - The ${{ params.execute.command }} argument is required.
  - You can specify an optional ${{ params.execute.timeout }} in milliseconds. If not specified, commands will use the default timeout.
  - It is very helpful if you write a clear, concise description of what this command does in 5-10 words.
  - If the output exceeds {max_output_bytes} characters, output will be truncated before being returned to you.
${%- if tools.by_kind.list or tools.by_kind.search or tools.by_kind.read or tools.by_kind.edit or tools.by_kind.write %}
${%- if has_unix_utilities %}
  - Avoid using this tool with the `find`, `grep`, `cat`, `head`, `tail`, `sed`, `awk`, or `echo` commands, unless explicitly instructed or when these commands are truly necessary for the task. Instead, always prefer using the dedicated tools for these commands:${%- if tools.by_kind.list %}
    - File search: Use the ${{ tools.by_kind.list }} tool (NOT the `find` or `ls` shell commands)${%- endif %}${%- if tools.by_kind.search %}
    - Content search: Use the ${{ tools.by_kind.search }} tool (NOT the `grep` or `rg` shell commands)${%- endif %}${%- if tools.by_kind.read %}
    - Read files: Use ${{ tools.by_kind.read }} (NOT cat/head/tail)${%- endif %}${%- if tools.by_kind.edit %}
    - Edit files: Use ${{ tools.by_kind.edit }} (NOT sed/awk)${%- endif %}${%- if tools.by_kind.write %}
    - Write files: Use ${{ tools.by_kind.write }} (NOT echo >/cat <<EOF)${%- endif %}
    - Communication: Output text directly (NOT echo/printf)
${%- else %}
  - The Unix utilities `grep`, `head`, `tail`, `sed`, `awk`, and `find` are NOT available in this shell — they will fail with `'<name>' is not recognized` (PowerShell adds `as a cmdlet`; cmd.exe adds `as an internal or external command`). Never invoke them. Use the dedicated tools instead:${%- if tools.by_kind.list %}
    - File search: Use ${{ tools.by_kind.list }} (NOT `Get-ChildItem` / `gci` / `dir` / `ls`)${%- endif %}${%- if tools.by_kind.search %}
    - Content search: Use ${{ tools.by_kind.search }} (NOT `Select-String` / `sls`)${%- endif %}${%- if tools.by_kind.read %}
    - Read files: Use ${{ tools.by_kind.read }} (NOT `Get-Content` / `gc` / `cat`)${%- endif %}${%- if tools.by_kind.edit %}
    - Edit files: Use ${{ tools.by_kind.edit }} (NOT `(Get-Content ...) -replace ... | Set-Content`; `sed` and `awk` are unavailable)${%- endif %}${%- if tools.by_kind.write %}
    - Write files: Use ${{ tools.by_kind.write }} (NOT `Set-Content` / `Out-File` / `echo >`)${%- endif %}
    - Communication: Output text directly (NOT `Write-Output` / `echo`)
${%- endif %}
${%- endif %}
  - When issuing multiple commands:
    - If the commands are independent and can run in parallel, make multiple calls to this tool in a single message. For example, if you need to run "git status" and "git diff", send a single message with two tool calls in parallel.
    - If the commands depend on each other and must run sequentially, use a single call with '&&' to chain them together (e.g., `git add . && git commit -m "message" && git push`).${%- if tools.by_kind.edit %} For instance, if one operation must complete before another starts (like mkdir before cp, ${{ tools.by_kind.edit }} before this tool for git operations, or git add before git commit), run these operations sequentially instead.${%- endif %}
    - Use ';' only when you need to run commands sequentially but don't care if earlier commands fail
    - DO NOT use newlines to separate commands (newlines are ok in quoted strings)
  - Try to maintain your current working directory throughout the session by using absolute paths and avoiding usage of `cd`. You may use `cd` if the User explicitly requests it.

# Committing changes with git

Only create commits when requested by the user. If unclear, ask first. When the user asks you to create a new git commit, follow these steps carefully:

Git Safety Protocol:
- NEVER update the git config
- NEVER run destructive/irreversible git commands (like push --force, hard reset, etc) unless the user explicitly requests them
- NEVER skip hooks (--no-verify, --no-gpg-sign, etc) unless the user explicitly requests it
- NEVER run force push to main/master, warn the user if they request it
- Avoid git commit --amend. ONLY use --amend when ALL conditions are met:
  (1) User explicitly requested amend, OR commit SUCCEEDED but pre-commit hook auto-modified files that need including
  (2) HEAD commit was created by you in this conversation (verify: git log -1 --format='%an %ae')
  (3) Commit has NOT been pushed to remote (verify: git status shows "Your branch is ahead")
- CRITICAL: If commit FAILED or was REJECTED by hook, NEVER amend - fix the issue and create a NEW commit
- CRITICAL: If you already pushed to remote, NEVER amend unless user explicitly requests it (requires force push)
- NEVER commit changes unless the user explicitly asks you to. It is VERY IMPORTANT to only commit when explicitly asked, otherwise the user will feel that you are being too proactive.

1. You can call multiple tools in a single response. When multiple independent pieces of information are requested and all commands are likely to succeed, run multiple tool calls in parallel for optimal performance. run the following bash commands in parallel, each using this tool:
  - Run a git status command to see all untracked files.
  - Run a git diff command to see both staged and unstaged changes that will be committed.
  - Run a git log command to see recent commit messages, so that you can follow this repository's commit message style.
2. Analyze all staged changes (both previously staged and newly added) and draft a commit message:
  - Summarize the nature of the changes (eg. new feature, enhancement to an existing feature, bug fix, refactoring, test, docs, etc.). Ensure the message accurately reflects the changes and their purpose (i.e. "add" means a wholly new feature, "update" means an enhancement to an existing feature, "fix" means a bug fix, etc.).
  - Do not commit files that likely contain secrets (.env, credentials.json, etc.). Warn the user if they specifically request to commit those files
  - Draft a concise (1-2 sentences) commit message that focuses on the "why" rather than the "what"
  - Ensure it accurately reflects the changes and their purpose
3. You can call multiple tools in a single response. When multiple independent pieces of information are requested and all commands are likely to succeed, run multiple tool calls in parallel for optimal performance. run the following commands:
   - Add relevant untracked files to the staging area.
   - Create the commit with a message
   - Run git status after the commit completes to verify success.
   Note: git status depends on the commit completing, so run it sequentially after the commit.
4. If the commit fails due to pre-commit hook, fix the issue and create a NEW commit (see amend rules above)

Important notes:
- NEVER run additional commands to read or explore code, besides git bash commands
- DO NOT push to the remote repository unless the user explicitly asks you to do so
- IMPORTANT: Never use git commands with the -i flag (like git rebase -i or git add -i) since they require interactive input which is not supported.
- If there are no changes to commit (i.e., no untracked files and no modifications), do not create an empty commit

# Creating pull requests
Use the gh command via this tool for ALL GitHub-related tasks including working with issues, pull requests, checks, and releases. If given a GitHub URL use the gh command to get the information needed.

IMPORTANT: When the user asks you to create a pull request, follow these steps carefully:

1. You can call multiple tools in a single response. When multiple independent pieces of information are requested and all commands are likely to succeed, run multiple tool calls in parallel for optimal performance. run the following bash commands in parallel using this tool, in order to understand the current state of the branch since it diverged from the main branch:
   - Run a git status command to see all untracked files
   - Run a git diff command to see both staged and unstaged changes that will be committed
   - Check if the current branch tracks a remote branch and is up to date with the remote, so you know if you need to push to the remote
   - Run a git log command and `git diff [base-branch]...HEAD` to understand the full commit history for the current branch (from the time it diverged from the base branch)
2. Analyze all changes that will be included in the pull request, making sure to look at all relevant commits (NOT just the latest commit, but ALL commits that will be included in the pull request!!!), and draft a pull request summary
3. You can call multiple tools in a single response. When multiple independent pieces of information are requested and all commands are likely to succeed, run multiple tool calls in parallel for optimal performance. run the following commands in parallel:
   - Create new branch if needed
   - Push to remote with -u flag if needed
   - Create PR using gh pr create with the format below. Use a HEREDOC to pass the body to ensure correct formatting.
<example>
gh pr create --title "the pr title" --body "$(cat <<'EOF'
## Summary
<1-3 bullet points>
</example>

Important:
- Return the PR URL when you're done, so the user can see it

# Other common operations
- View comments on a GitHub PR: gh api repos/foo/bar/pulls/123/comments"#;

// ───────────────────────────────────────────────────────────────────────────
// Input
// ───────────────────────────────────────────────────────────────────────────

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Input for the opencode bash tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BashInput {
    /// The bash command to run.
    #[schemars(description = "The bash command to run.")]
    pub command: String,

    /// Optional timeout in milliseconds.
    #[schemars(description = "Optional timeout in milliseconds.")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,

    /// Optional working directory. Defaults to the session working directory.
    #[schemars(
        description = "Optional working directory. Defaults to the session working directory."
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,

    /// A clear, concise description of what this command does in 5-10 words.
    #[schemars(
        description = "One sentence explanation as to why this command needs to be run and how it contributes to the goal."
    )]
    pub description: String,
}

// Manual conversions for the ToolInput enum (BashInput is not a variant).

impl TryFrom<ToolInput> for BashInput {
    type Error = String;
    fn try_from(value: ToolInput) -> Result<Self, Self::Error> {
        match value {
            ToolInput::Bash(b) => Ok(BashInput {
                command: b.command,
                timeout: b.timeout,
                workdir: None,
                description: b.description,
            }),
            ToolInput::Dynamic(v) => {
                serde_json::from_value(v).map_err(|e| format!("BashInput: {e}"))
            }
            _ => Err("expected Bash or Dynamic variant for BashInput".into()),
        }
    }
}

impl From<BashInput> for ToolInput {
    fn from(value: BashInput) -> Self {
        ToolInput::Bash(crate::implementations::BashToolInput {
            command: value.command,
            timeout: value.timeout,
            description: value.description,
            is_background: false,
        })
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tool implementation
// ───────────────────────────────────────────────────────────────────────────

/// OpenCode bash tool — executes shell commands via the `Terminal` backend.
#[derive(Debug, Default)]
pub struct BashTool;

impl BashTool {
    /// Compute the effective timeout: clamp user-provided ms, or use default.
    fn effective_timeout(input_timeout_ms: Option<u64>) -> Duration {
        match input_timeout_ms {
            Some(ms) => {
                let clamped = ms.min(MAX_TIMEOUT_MS);
                Duration::from_millis(clamped)
            }
            None => DEFAULT_TIMEOUT,
        }
    }

    /// Resolve the working directory from the optional `workdir` param,
    /// falling back to the session `Cwd`.
    fn resolve_cwd(session_cwd: &std::path::Path, workdir: Option<&str>) -> PathBuf {
        match workdir {
            Some(dir) => {
                let p = std::path::Path::new(dir);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    session_cwd.join(p)
                }
            }
            None => session_cwd.to_path_buf(),
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Into<ToolOutput> for BashToolOutput
// ───────────────────────────────────────────────────────────────────────────

impl From<BashToolOutput> for crate::types::output::ToolOutput {
    fn from(o: BashToolOutput) -> Self {
        match o {
            BashToolOutput::Bash(b) => crate::types::output::ToolOutput::Bash(b),
            BashToolOutput::BackgroundTaskStarted(b) => {
                crate::types::output::ToolOutput::BackgroundTaskStarted(b)
            }
        }
    }
}

impl crate::types::tool_metadata::ToolMetadata for BashTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::OpenCode
    }

    fn description_template(&self) -> &str {
        DESCRIPTION
    }

    fn emitted_notifications(&self) -> &'static [&'static str] {
        // Foreground-only: OpenCode never backgrounds, so no
        // `BashExecutionBackgrounded`/`TaskCompleted`.
        &[
            "BashExecutionComplete",
            "BashExecutionFailed",
            "BashExecutionTimeout",
            "BashOutputChunk",
        ]
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for BashTool {
    type Args = BashInput;
    type Output = BashToolOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("bash").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "bash",
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

    #[tracing::instrument(name = "tool.opencode.bash", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: BashInput,
    ) -> Result<BashToolOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        // --- Read resources ---
        let backend = resources.lock().await.require::<Terminal>()?.0.clone();
        let session_cwd = crate::types::tool_metadata::resolve_cwd(&ctx, &resources).await?;
        let tool_call_id = ctx.call_id.as_str().to_owned();
        let session_folder = resources.lock().await.require::<SessionFolder>()?.0.clone();
        let env = resources
            .lock()
            .await
            .require::<SessionEnv>()?
            .0
            .as_ref()
            .clone();
        let notification_handle = resources
            .lock()
            .await
            .require::<NotificationHandle>()?
            .0
            .clone();

        let output_byte_limit = resources
            .lock()
            .await
            .get::<TruncationCfg>()
            .map(|cfg| {
                cfg.0
                    .max_output_bytes_for("bash", DEFAULT_TOOL_OUTPUT_CHARS)
            })
            .unwrap_or(DEFAULT_TOOL_OUTPUT_CHARS);

        // --- Resolve working directory ---
        let cwd = Self::resolve_cwd(&session_cwd, input.workdir.as_deref());

        // --- Compute effective timeout ---
        let timeout = Self::effective_timeout(input.timeout);

        // --- Build output file path ---
        let output_file = session_folder
            .join("terminal")
            .join(format!("{}.log", tool_call_id));

        // --- Foreground execution ---
        let request = TerminalRunRequest {
            command: input.command.clone(),
            working_directory: cwd.clone(),
            env,
            timeout,
            output_byte_limit,
            output_file: output_file.clone(),
            notification_handle: notification_handle.clone(),
            tool_call_id: tool_call_id.clone(),
            display_command: None, // OpenCode doesn't use isolation wrapping
            auto_background_on_timeout: false, // OpenCode doesn't support auto-backgrounding
            foreground_block_budget: None,
            kind: crate::computer::types::TaskKind::Bash,
            // OpenCode doesn't use shared terminal backends.
            owner_session_id: None,
            description: None,
        };

        let result = match backend.run(request).await {
            Ok(r) => r,
            Err(e) => {
                notification_handle.send_failed(BashExecutionFailed {
                    tool_call_id: tool_call_id.clone(),
                    command: input.command.clone(),
                    cwd: cwd.clone(),
                    error: e.to_string(),
                });
                return Err(echo_agent_tool_runtime::ToolError::execution(
                    echo_agent_tool_protocol::ToolId::new("run_terminal_command").expect("valid"),
                    e.to_string(),
                ));
            }
        };

        // --- Send completion notification ---
        let base = BashNotificationBase {
            tool_call_id: tool_call_id.clone(),
            command: input.command.clone(),
            output: result.combined_output.as_bytes().to_vec(),
            total_bytes: result.total_bytes,
            truncated: result.truncated,
            cwd: cwd.clone(),
        };

        if result.timed_out {
            notification_handle.send_timeout(BashExecutionTimeout {
                base,
                elapsed: timeout,
                timeout,
            });
        } else {
            notification_handle.send_complete(BashExecutionComplete {
                base,
                exit_code: result.exit_code,
                signal: result.signal.clone(),
            });
        }

        Ok(BashToolOutput::Bash(BashOutput {
            output_for_prompt: BashOutput::make_output_for_prompt(&result.combined_output),
            output: result.combined_output.into_bytes(),
            exit_code: result.exit_code.unwrap_or(-1),
            command: input.command,
            truncated: result.truncated,
            signal: result.signal,
            timed_out: result.timed_out,
            description: Some(input.description),
            current_dir: cwd.to_string_lossy().to_string(),
            output_file: output_file.to_string_lossy().to_string(),
            total_bytes: result.total_bytes,
            output_delta: None,
            was_bare_echo: false,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx;

    fn rt_ctx_with_call_id(
        resources: Resources,
        call_id: &str,
    ) -> echo_agent_tool_runtime::ToolCallContext {
        let id = echo_agent_tool_protocol::ToolCallId::new(call_id).unwrap();
        let mut ctx = echo_agent_tool_runtime::ToolCallContext::new(id);
        ctx.extensions.insert(resources.into_shared());
        ctx
    }
    use crate::computer::types::{
        BackgroundHandle, ComputerError, KillOutcome, TaskSnapshot, TerminalBackend,
        TerminalRunRequest, TerminalRunResult,
    };
    use crate::notification::types::ToolNotificationHandle;
    use crate::types::resources::Resources;
    use std::collections::HashMap;
    use std::sync::Arc;

    // ─── Mock terminal ───

    struct MockTerminal {
        foreground_result: Result<TerminalRunResult, ComputerError>,
    }

    impl MockTerminal {
        fn success(output: &str, exit_code: i32) -> Self {
            Self {
                foreground_result: Ok(TerminalRunResult {
                    combined_output: output.to_string(),
                    exit_code: Some(exit_code),
                    truncated: false,
                    signal: None,
                    timed_out: false,
                    output_file: PathBuf::from("/tmp/test.log"),
                    total_bytes: output.len(),
                    pid: None,
                }),
            }
        }

        fn timed_out(output: &str) -> Self {
            Self {
                foreground_result: Ok(TerminalRunResult {
                    combined_output: output.to_string(),
                    exit_code: None,
                    truncated: false,
                    signal: None,
                    timed_out: true,
                    output_file: PathBuf::from("/tmp/test.log"),
                    total_bytes: output.len(),
                    pid: None,
                }),
            }
        }

        fn failing() -> Self {
            Self {
                foreground_result: Err(ComputerError::io("command failed")),
            }
        }
    }

    #[async_trait::async_trait]
    impl TerminalBackend for MockTerminal {
        async fn run(
            &self,
            _request: TerminalRunRequest,
        ) -> Result<TerminalRunResult, ComputerError> {
            self.foreground_result.clone()
        }

        async fn run_background(
            &self,
            _request: TerminalRunRequest,
        ) -> Result<BackgroundHandle, ComputerError> {
            Err(ComputerError::io("not supported"))
        }

        async fn get_task(&self, _task_id: &str) -> Option<TaskSnapshot> {
            None
        }

        async fn kill_task(&self, _task_id: &str) -> KillOutcome {
            KillOutcome::NotFound
        }

        async fn wait_for_completion(
            &self,
            _task_id: &str,
            _timeout: Option<Duration>,
        ) -> Option<TaskSnapshot> {
            None
        }

        async fn list_tasks(&self) -> Vec<TaskSnapshot> {
            vec![]
        }
    }

    // ─── Test helpers ───

    fn make_resources(mock: MockTerminal) -> Resources {
        let mut resources = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(mock);
        resources.insert(Terminal(backend));
        resources.insert(Cwd(PathBuf::from("/tmp")));
        resources.insert(SessionFolder(PathBuf::from("/tmp/session")));
        resources.insert(SessionEnv(Arc::new(HashMap::new())));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        resources
    }

    #[test]
    fn description_template_tracks_renamed_timeout() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;
        use crate::types::tool_metadata::ToolMetadata;
        use std::collections::HashMap;

        let tools = HashMap::from([(ToolKind::Execute, "bash".to_string())]);
        let params = HashMap::from([(
            ToolKind::Execute,
            HashMap::from([("timeout".to_string(), "max_wait".to_string())]),
        )]);
        let rendered = TemplateRenderer::new(tools, params)
            .render(ToolMetadata::description_template(&BashTool))
            .unwrap();
        assert!(
            rendered.contains("optional max_wait in milliseconds"),
            "renamed timeout must appear:\n{rendered}"
        );
        assert!(
            !rendered.contains("optional timeout in milliseconds"),
            "canonical timeout must not remain after rename:\n{rendered}"
        );
    }

    #[test]
    fn description_template_tracks_renamed_command() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;
        use crate::types::tool_metadata::ToolMetadata;
        use std::collections::HashMap;

        let tools = HashMap::from([(ToolKind::Execute, "run_command".to_string())]);
        let params = HashMap::from([(
            ToolKind::Execute,
            HashMap::from([
                ("command".to_string(), "script".to_string()),
                ("timeout".to_string(), "timeout".to_string()),
            ]),
        )]);
        let rendered = TemplateRenderer::new(tools, params)
            .render(ToolMetadata::description_template(&BashTool))
            .unwrap();
        assert!(
            rendered.contains("The script argument is required."),
            "renamed command must appear:\n{rendered}"
        );
        assert!(
            !rendered.contains("The command argument is required.")
                && !rendered.contains("Bash tool"),
            "stale command/tool-name literals must not remain:\n{rendered}"
        );
    }

    fn make_input(command: &str) -> BashInput {
        BashInput {
            command: command.to_string(),
            timeout: None,
            workdir: None,
            description: "Test command".to_string(),
        }
    }

    // ─── Tests ───

    #[tokio::test]
    async fn foreground_command_success() {
        let resources = make_resources(MockTerminal::success("hello world\n", 0));
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("echo hello"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Bash(bash) => {
                assert_eq!(bash.exit_code, 0);
                assert_eq!(bash.command, "echo hello");
                assert!(!bash.timed_out);
                assert!(!bash.truncated);
                assert_eq!(String::from_utf8_lossy(&bash.output), "hello world\n");
                assert_eq!(bash.current_dir, "/tmp");
            }
            BashToolOutput::BackgroundTaskStarted(_) => panic!("Expected foreground output"),
        }
    }

    #[tokio::test]
    async fn foreground_command_timeout() {
        let resources = make_resources(MockTerminal::timed_out("partial output"));
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("sleep 999"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Bash(bash) => {
                assert!(bash.timed_out);
                assert_eq!(bash.exit_code, -1);
            }
            BashToolOutput::BackgroundTaskStarted(_) => panic!("Expected foreground output"),
        }
    }

    #[tokio::test]
    async fn foreground_command_error() {
        let resources = make_resources(MockTerminal::failing());
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("bad_cmd"),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command failed"));
    }

    #[tokio::test]
    async fn workdir_overrides_cwd() {
        // Verify resolve_cwd logic
        let session_cwd = std::path::Path::new("/home/user/project");

        // Absolute workdir
        let resolved = BashTool::resolve_cwd(session_cwd, Some("/tmp/other"));
        assert_eq!(resolved, PathBuf::from("/tmp/other"));

        // Relative workdir
        let resolved = BashTool::resolve_cwd(session_cwd, Some("subdir"));
        assert_eq!(resolved, PathBuf::from("/home/user/project/subdir"));

        // No workdir
        let resolved = BashTool::resolve_cwd(session_cwd, None);
        assert_eq!(resolved, PathBuf::from("/home/user/project"));
    }

    #[tokio::test]
    async fn timeout_clamped_to_max() {
        let timeout = BashTool::effective_timeout(Some(999_999));
        assert_eq!(timeout, Duration::from_millis(MAX_TIMEOUT_MS));
    }

    #[tokio::test]
    async fn timeout_uses_default_when_none() {
        let timeout = BashTool::effective_timeout(None);
        assert_eq!(timeout, DEFAULT_TIMEOUT);
    }

    #[tokio::test]
    async fn tool_metadata() {
        use crate::types::tool_metadata::ToolMetadata;
        let tool = BashTool;
        assert_eq!(echo_agent_tool_runtime::Tool::id(&tool).as_str(), "bash");
        assert!(matches!(tool.kind(), ToolKind::Execute));
        assert!(matches!(tool.tool_namespace(), ToolNamespace::OpenCode));
    }

    #[tokio::test]
    async fn errors_when_terminal_not_in_resources() {
        let mut resources = Resources::new();
        resources.insert(Cwd(PathBuf::from("/tmp")));
        resources.insert(SessionFolder(PathBuf::from("/tmp/s")));
        // No Terminal inserted
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("ls"),
        )
        .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing required resource")
        );
    }

    #[tokio::test]
    async fn truncated_output() {
        let mock = MockTerminal {
            foreground_result: Ok(TerminalRunResult {
                combined_output: "partial…".to_string(),
                exit_code: Some(0),
                truncated: true,
                signal: None,
                timed_out: false,
                output_file: PathBuf::from("/tmp/test.log"),
                total_bytes: 100_000,
                pid: None,
            }),
        };
        let resources = make_resources(mock);
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("cat bigfile"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Bash(bash) => {
                assert!(bash.truncated);
                assert_eq!(bash.total_bytes, 100_000);
            }
            BashToolOutput::BackgroundTaskStarted(_) => panic!("Expected foreground output"),
        }
    }

    #[tokio::test]
    async fn signal_field() {
        let mock = MockTerminal {
            foreground_result: Ok(TerminalRunResult {
                combined_output: String::new(),
                exit_code: None,
                truncated: false,
                signal: Some("SIGKILL".to_string()),
                timed_out: false,
                output_file: PathBuf::from("/tmp/test.log"),
                total_bytes: 0,
                pid: None,
            }),
        };
        let resources = make_resources(mock);
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("kill -9 $$"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Bash(bash) => {
                assert_eq!(bash.signal, Some("SIGKILL".to_string()));
                assert_eq!(bash.exit_code, -1); // None maps to -1
            }
            BashToolOutput::BackgroundTaskStarted(_) => panic!("Expected foreground output"),
        }
    }

    #[tokio::test]
    async fn output_file_path() {
        let mut resources = make_resources(MockTerminal::success("ok", 0));
        // Override with specific IDs so we can assert the path.
        resources.insert(SessionFolder(PathBuf::from("/sessions/abc")));

        let tool = BashTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            rt_ctx_with_call_id(resources, "my-call-42"),
            make_input("echo ok"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Bash(bash) => {
                assert_eq!(bash.output_file, "/sessions/abc/terminal/my-call-42.log");
            }
            BashToolOutput::BackgroundTaskStarted(_) => panic!("Expected foreground output"),
        }
    }

    #[tokio::test]
    async fn description_preserved() {
        let resources = make_resources(MockTerminal::success("ok", 0));
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("echo ok"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Bash(bash) => {
                assert_eq!(bash.description, Some("Test command".to_string()));
            }
            BashToolOutput::BackgroundTaskStarted(_) => panic!("Expected foreground output"),
        }
    }

    #[tokio::test]
    async fn custom_timeout_within_range() {
        let timeout = BashTool::effective_timeout(Some(5000));
        assert_eq!(timeout, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn timeout_zero_accepted() {
        // Regression guard: timeout=0 should produce Duration::ZERO, not panic.
        let timeout = BashTool::effective_timeout(Some(0));
        assert_eq!(timeout, Duration::from_millis(0));
    }

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "command": "ls -la",
            "timeout": 30000,
            "workdir": "/home/user",
            "description": "List files"
        });

        let input: BashInput = serde_json::from_value(json).unwrap();
        assert_eq!(input.command, "ls -la");
        assert_eq!(input.timeout, Some(30000));
        assert_eq!(input.workdir, Some("/home/user".to_string()));
        assert_eq!(input.description, "List files");

        // Minimal (only required fields)
        let minimal = serde_json::json!({
            "command": "pwd",
            "description": "Print directory"
        });
        let input2: BashInput = serde_json::from_value(minimal).unwrap();
        assert_eq!(input2.command, "pwd");
        assert!(input2.timeout.is_none());
        assert!(input2.workdir.is_none());
    }

    #[test]
    fn tool_input_from_bash_input_is_bash_variant() {
        let input = BashInput {
            command: "echo hi".into(),
            timeout: Some(1_000),
            workdir: Some("/tmp".into()),
            description: "say hi".into(),
        };
        match ToolInput::from(input) {
            ToolInput::Bash(b) => {
                assert_eq!(b.command, "echo hi");
                assert_eq!(b.timeout, Some(1_000));
                assert!(!b.is_background);
            }
            other => panic!("expected ToolInput::Bash, got {other:?}"),
        }
    }

    #[test]
    fn from_bash_tool_output() {
        use crate::types::output::{BackgroundTaskStarted, ToolOutput};

        // Bash variant
        let bash = BashToolOutput::Bash(BashOutput {
            output: b"hello".to_vec(),
            output_for_prompt: "hello".to_string(),
            exit_code: 0,
            command: "echo hello".to_string(),
            truncated: false,
            signal: None,
            timed_out: false,
            description: None,
            current_dir: "/tmp".to_string(),
            output_file: "/tmp/out.log".to_string(),
            total_bytes: 5,
            output_delta: None,
            was_bare_echo: false,
        });
        let tool_output: ToolOutput = bash.into();
        assert!(matches!(tool_output, ToolOutput::Bash(_)));

        // BackgroundTaskStarted variant
        let bg = BashToolOutput::BackgroundTaskStarted(BackgroundTaskStarted {
            task_id: "task-1".to_string(),
            task_type: "bash".to_string(),
            output_file: "/tmp/bg.log".to_string(),
            status: "running".to_string(),
            command: "sleep 100".to_string(),
            summary: "Running sleep 100".to_string(),
            retrieval_hint: String::new(),
            pre_formatted: None,
            pid: None,
        });
        let tool_output: ToolOutput = bg.into();
        assert!(matches!(tool_output, ToolOutput::BackgroundTaskStarted(_)));
    }

    // ─── Additional tests ───

    #[tokio::test]
    async fn output_for_prompt_populated() {
        let resources = make_resources(MockTerminal::success("hello world\n", 0));
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("echo hello"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Bash(bash) => {
                assert!(!bash.output_for_prompt.is_empty());
                assert_eq!(
                    bash.output_for_prompt,
                    BashOutput::make_output_for_prompt("hello world\n")
                );
            }
            BashToolOutput::BackgroundTaskStarted(_) => panic!("Expected foreground output"),
        }
    }

    #[tokio::test]
    async fn workdir_end_to_end() {
        let mock = MockTerminal::success("ok\n", 0);
        let mut resources = make_resources(mock);
        // Override Cwd to a different path so we can verify workdir takes precedence.
        resources.insert(Cwd(PathBuf::from("/home/user")));

        let tool = BashTool;
        let input = BashInput {
            command: "pwd".to_string(),
            timeout: None,
            workdir: Some("/tmp/test".to_string()),
            description: "Check workdir".to_string(),
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            BashToolOutput::Bash(bash) => {
                // current_dir should be the resolved workdir, not the session Cwd.
                assert_eq!(bash.current_dir, "/tmp/test");
            }
            BashToolOutput::BackgroundTaskStarted(_) => panic!("Expected foreground output"),
        }
    }

    #[tokio::test]
    async fn truncation_cfg_override() {
        use crate::types::context::TruncationConfig;
        use std::sync::Mutex;

        // A mock that captures the request so we can inspect output_byte_limit.
        struct CapturingMockTerminal {
            captured_request: Mutex<Option<TerminalRunRequest>>,
        }

        #[async_trait::async_trait]
        impl TerminalBackend for CapturingMockTerminal {
            async fn run(
                &self,
                request: TerminalRunRequest,
            ) -> Result<TerminalRunResult, ComputerError> {
                *self.captured_request.lock().unwrap() = Some(request);
                Ok(TerminalRunResult {
                    combined_output: "ok".to_string(),
                    exit_code: Some(0),
                    truncated: false,
                    signal: None,
                    timed_out: false,
                    output_file: PathBuf::from("/tmp/test.log"),
                    total_bytes: 2,
                    pid: None,
                })
            }

            async fn run_background(
                &self,
                _request: TerminalRunRequest,
            ) -> Result<BackgroundHandle, ComputerError> {
                Err(ComputerError::io("not supported"))
            }

            async fn get_task(&self, _task_id: &str) -> Option<TaskSnapshot> {
                None
            }

            async fn kill_task(&self, _task_id: &str) -> KillOutcome {
                KillOutcome::NotFound
            }

            async fn wait_for_completion(
                &self,
                _task_id: &str,
                _timeout: Option<Duration>,
            ) -> Option<TaskSnapshot> {
                None
            }

            async fn list_tasks(&self) -> Vec<TaskSnapshot> {
                vec![]
            }
        }

        let capturing = Arc::new(CapturingMockTerminal {
            captured_request: Mutex::new(None),
        });

        let mut resources = Resources::new();
        let backend: Arc<dyn TerminalBackend> = capturing.clone();
        resources.insert(Terminal(backend));
        resources.insert(Cwd(PathBuf::from("/tmp")));
        resources.insert(SessionFolder(PathBuf::from("/tmp/session")));
        resources.insert(SessionEnv(Arc::new(HashMap::new())));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));

        // Insert TruncationCfg with a custom per-tool limit for "bash".
        let mut cfg = TruncationConfig::default();
        cfg.per_tool_max_output_bytes
            .insert("bash".to_string(), 1000);
        resources.insert(TruncationCfg(cfg));

        let tool = BashTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("echo ok"),
        )
        .await;
        assert!(result.is_ok());

        // Verify the captured request used the overridden limit.
        let captured = capturing.captured_request.lock().unwrap();
        let req = captured
            .as_ref()
            .expect("request should have been captured");
        assert_eq!(req.output_byte_limit, 1000);
    }

    #[tokio::test]
    async fn missing_session_env() {
        let mock = MockTerminal::success("ok", 0);
        let backend: Arc<dyn TerminalBackend> = Arc::new(mock);

        let mut resources = Resources::new();
        resources.insert(Terminal(backend));
        resources.insert(Cwd(PathBuf::from("/tmp")));
        resources.insert(SessionFolder(PathBuf::from("/tmp/session")));
        // Deliberately omit SessionEnv.
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));

        let tool = BashTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("ls"),
        )
        .await;

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing required resource"),
            "Expected error about missing required resource"
        );
    }

    // ─── Description template shell-awareness parity tests ───
    //
    // Same shape as echo_agent_build/bash: the opencode tool inherits the same
    // Unix-utility guidance and must branch on PowerShell/cmd.

    mod description_shell_branches {
        use super::*;
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;
        use std::collections::HashMap;

        fn full_renderer() -> TemplateRenderer {
            TemplateRenderer::new(
                HashMap::from([
                    (ToolKind::List, "list_dir".to_string()),
                    (ToolKind::Search, "grep".to_string()),
                    (ToolKind::Read, "read_file".to_string()),
                    (ToolKind::Edit, "search_replace".to_string()),
                    (ToolKind::Write, "write".to_string()),
                    (ToolKind::Execute, "bash".to_string()),
                ]),
                HashMap::from([(
                    ToolKind::Execute,
                    HashMap::from([("timeout".to_string(), "timeout".to_string())]),
                )]),
            )
        }

        /// Render with a forced `has_unix_utilities` override so we can
        /// exercise both branches without changing the host OS.
        fn render(has_unix_utilities: bool) -> String {
            let renderer = full_renderer();
            let extras = serde_json::json!({
                "has_unix_utilities": has_unix_utilities,
            });
            renderer.render_with_extra(DESCRIPTION, &extras).unwrap()
        }

        #[test]
        fn unix_shell_emits_legacy_avoid_paragraph() {
            let out = render(true);
            assert!(out.contains("Avoid using this tool with the `find`, `grep`, `cat`, `head`"));
            assert!(!out.contains("are NOT available in this shell"));
        }

        #[test]
        fn powershell_emits_explicit_unavailable_warning() {
            let out = render(false);
            assert!(out.contains(
                "Unix utilities `grep`, `head`, `tail`, `sed`, `awk`, and `find` are NOT available in this shell"
            ));
            assert!(out.contains("'<name>' is not recognized"));
            assert!(!out.contains("Avoid using this tool with the `find`, `grep`, `cat`, `head`"));
        }
    }
}
