//! New tool I/O types for the spec architecture.
//!
//! These types exist alongside the old `tool_input::ToolInput` and
//! `output::ToolOutput`. They will replace the old types once all tool
//! implementations are migrated to the new `Tool` trait.
//!
//! ## Design
//!
//! - `ToolInput` — one variant per built-in tool + `Dynamic(Value)`.
//!   `TryInto` derive generates `TryFrom<ToolInput>` for each inner type.
//! - `ToolOutput` — one variant per built-in tool + `Dynamic(Value)`.
//!   `From` derive generates `From<TypedOutput>` for each inner type.
use crate::implementations::BashToolInput;
use crate::implementations::codex::apply_patch::tool::ApplyPatchInput;
use crate::implementations::codex::grep_files::tool::CodexGrepFilesInput;
use crate::implementations::codex::list_dir::tool::CodexListDirInput;
use crate::implementations::codex::read_file::tool::CodexReadFileInput;
use crate::implementations::echo_agent_build::ask_user_question::AskUserQuestionInput;
use crate::implementations::echo_agent_build::enter_plan_mode::EnterPlanModeInput;
use crate::implementations::echo_agent_build::exit_plan_mode::ExitPlanModeInput;
use crate::implementations::echo_agent_build::grep::GrepSearchInput;
use crate::implementations::echo_agent_build::image_edit::ImageEditInput;
use crate::implementations::echo_agent_build::image_gen::ImageGenInput;
use crate::implementations::echo_agent_build::list_dir::ListDirInput;
use crate::implementations::echo_agent_build::read_file::ReadFileInput;
use crate::implementations::echo_agent_build::search_replace::SearchReplaceInput;
use crate::implementations::echo_agent_build::todo::TodoWriteInput;
use crate::implementations::echo_agent_build::update_goal::UpdateGoalInput;
use crate::implementations::echo_agent_build::video_gen::{
    ImageToVideoInput, ReferenceToVideoInput,
};
use crate::implementations::echo_agent_build::web_fetch::WebFetchInput;
use crate::implementations::echo_agent_build::web_search::WebSearchInput;
use crate::implementations::lsp::LspToolInput;
use crate::implementations::memory::types::{MemoryGetInput, MemorySearchInput};
use crate::implementations::opencode::write::WriteInput;
use crate::implementations::search_tool::SearchToolInput;
use crate::implementations::skills::skill::SkillInput;
use crate::implementations::use_tool::UseToolInput;
use echo_agent_tool_types::KillTaskToolInput;
use echo_agent_tool_types::TaskOutputToolInput;
use echo_agent_tool_types::TaskToolInput;
use echo_agent_tool_types::WaitTasksToolInput;
use serde::{Deserialize, Serialize};
/// Raw input for an MCP (Model Context Protocol) tool call.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MCPToolInput {
    pub tool_name: String,
    pub tool_input: serde_json::Value,
}
/// Typed tool input — one variant per built-in tool, plus `Dynamic` for
/// MCP/runtime-registered tools.
///
/// Each variant wraps the tool's existing input struct. The new `Tool` trait
/// will use `TryFrom<ToolInput>` to extract the typed input.
///
/// `derive_more::TryInto` generates `TryFrom<ToolInput> for T` for each
/// inner type, so e.g. `ReadFileInput::try_from(input)` extracts the `ReadFileInput`
/// variant or returns an error.
#[derive(Debug, Clone, Serialize, Deserialize, derive_more::TryInto, derive_more::From)]
#[serde(tag = "variant")]
pub enum ToolInput {
    ReadFile(ReadFileInput),
    SearchReplace(SearchReplaceInput),
    Bash(BashToolInput),
    Grep(GrepSearchInput),
    ListDir(ListDirInput),
    TodoWrite(TodoWriteInput),
    Skill(SkillInput),
    MCPTool(MCPToolInput),
    TaskOutput(TaskOutputToolInput),
    WaitTasks(WaitTasksToolInput),
    KillTask(KillTaskToolInput),
    Task(TaskToolInput),
    WebSearch(WebSearchInput),
    ImageGen(ImageGenInput),
    ImageEdit(ImageEditInput),
    ImageToVideo(ImageToVideoInput),
    ReferenceToVideo(ReferenceToVideoInput),
    WebFetch(WebFetchInput),
    Write(WriteInput),
    ApplyPatch(ApplyPatchInput),
    HashlineEdit(crate::implementations::echo_agent_build_hashline::edit::types::HashlineEditInput),
    CodexListDir(CodexListDirInput),
    CodexGrepFiles(CodexGrepFilesInput),
    CodexReadFile(CodexReadFileInput),
    MemorySearch(MemorySearchInput),
    MemoryGet(MemoryGetInput),
    SearchTool(SearchToolInput),
    UseTool(UseToolInput),
    EnterPlanMode(EnterPlanModeInput),
    ExitPlanMode(ExitPlanModeInput),
    AskUserQuestion(AskUserQuestionInput),
    Lsp(LspToolInput),
    Monitor(crate::implementations::echo_agent_build::monitor::types::MonitorInput),
    SchedulerCreate(
        crate::implementations::echo_agent_build::scheduler::create::SchedulerCreateInput,
    ),
    SchedulerDelete(
        crate::implementations::echo_agent_build::scheduler::delete::SchedulerDeleteInput,
    ),
    SchedulerList(crate::implementations::echo_agent_build::scheduler::list::SchedulerListInput),
    UpdateGoal(UpdateGoalInput),
    Workflow(crate::implementations::echo_agent_build::workflow::WorkflowToolInput),
    /// Dynamic input for runtime-registered tools (MCP, etc.)
    Dynamic(serde_json::Value),
}
impl ToolInput {
    /// The real target tool for *meta-dispatch* tools whose wire `function.name`
    /// is only the wrapper (`use_tool`), or `None` for
    /// ordinary tools (already named by `function.name`). Single source of truth
    /// for hook matching / telemetry; callers fall back to `function.name` on
    /// `None`. Add any new dispatcher here.
    pub fn dispatch_target_name(&self) -> Option<String> {
        match self {
            ToolInput::UseTool(input) => Some(input.tool_name.clone()),
            _ => None,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn try_into_input_succeeds_for_matching_variant() {
        let input = ToolInput::ListDir(ListDirInput {
            target_directory: "/tmp".to_string(),
        });
        let result: Result<ListDirInput, _> = input.try_into();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().target_directory, "/tmp");
    }
    #[test]
    fn dispatch_target_name_resolves_meta_dispatch_tools() {
        let use_tool = ToolInput::UseTool(UseToolInput {
            tool_name: "linear__save_issue".to_string(),
            tool_input: serde_json::json!({}),
        });
        assert_eq!(
            use_tool.dispatch_target_name().as_deref(),
            Some("linear__save_issue")
        );
        let ordinary = ToolInput::ListDir(ListDirInput {
            target_directory: "/tmp".to_string(),
        });
        assert_eq!(ordinary.dispatch_target_name(), None);
    }
    #[test]
    fn try_into_input_fails_for_mismatched_variant() {
        let input = ToolInput::ListDir(ListDirInput {
            target_directory: "/tmp".to_string(),
        });
        let result: Result<ReadFileInput, _> = input.try_into();
        assert!(result.is_err());
    }
    #[test]
    fn try_into_all_input_variants() {
        let rf: Result<ReadFileInput, _> = ToolInput::ReadFile(ReadFileInput {
            document: None,
            path: "x".into(),
            offset: None,
            limit: None,
            pages: None,
            format: None,
        })
        .try_into();
        assert_eq!(rf.unwrap().path, "x");
        let bash: Result<BashToolInput, _> = ToolInput::Bash(BashToolInput {
            command: "ls".into(),
            timeout: None,
            description: "list files".into(),
            is_background: false,
        })
        .try_into();
        assert_eq!(bash.unwrap().command, "ls");
        let grep: Result<GrepSearchInput, _> = ToolInput::Grep(GrepSearchInput {
            pattern: "test".into(),
            path: None,
            glob: None,
            output_mode: None,
            before_context: None,
            after_context: None,
            context: None,
            case_insensitive: false,
            head_limit: None,
            multiline: false,
            r#type: None,
        })
        .try_into();
        assert_eq!(grep.unwrap().pattern, "test");
        let kill: Result<KillTaskToolInput, _> = ToolInput::KillTask(KillTaskToolInput {
            task_id: "t1".into(),
        })
        .try_into();
        assert_eq!(kill.unwrap().task_id, "t1");
        let ws: Result<WebSearchInput, _> = ToolInput::WebSearch(WebSearchInput {
            query: "q".into(),
            allowed_domains: None,
        })
        .try_into();
        assert_eq!(ws.unwrap().query, "q");
    }
    #[test]
    fn dynamic_input_holds_arbitrary_json() {
        let input = ToolInput::Dynamic(serde_json::json!({"custom": "data"}));
        match input {
            ToolInput::Dynamic(v) => {
                assert_eq!(v["custom"], "data");
            }
            _ => panic!("Expected Dynamic variant"),
        }
    }
}
