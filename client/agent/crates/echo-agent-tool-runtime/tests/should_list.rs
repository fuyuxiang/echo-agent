//! `Tool::should_list` predicate + `ToolDyn` blanket forwarding.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use echo_agent_tool_protocol::ToolId;
use echo_agent_tool_runtime::{
    ArcTool, Cwd, ListToolsContext, Tool, ToolCallContext, ToolDyn, ToolError, ToolOutput,
};
use echo_agent_tool_types::ToolDescription;

#[derive(Deserialize, JsonSchema)]
struct NoArgs {}

#[derive(Serialize)]
struct Unit;

impl ToolOutput for Unit {}
struct AlwaysTool;

impl Tool for AlwaysTool {
    type Args = NoArgs;
    type Output = Unit;
    fn id(&self) -> ToolId {
        ToolId::new("always").unwrap()
    }
    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("always", "a")
    }
    async fn run(&self, _: ToolCallContext, _: NoArgs) -> Result<Unit, ToolError> {
        Ok(Unit)
    }
}

struct NeedsCwdTool;

impl Tool for NeedsCwdTool {
    type Args = NoArgs;
    type Output = Unit;
    fn id(&self) -> ToolId {
        ToolId::new("needs_cwd").unwrap()
    }
    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("needs_cwd", "a")
    }
    fn should_list(&self, ctx: &ListToolsContext) -> bool {
        ctx.extensions.contains::<Cwd>()
    }
    async fn run(&self, _: ToolCallContext, _: NoArgs) -> Result<Unit, ToolError> {
        Ok(Unit)
    }
}

#[derive(Clone, Debug)]
struct AttachmentCount(usize);

struct NeedsAttachmentTool;

impl Tool for NeedsAttachmentTool {
    type Args = NoArgs;
    type Output = Unit;
    fn id(&self) -> ToolId {
        ToolId::new("needs_attachment").unwrap()
    }
    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("needs_attachment", "a")
    }
    fn should_list(&self, ctx: &ListToolsContext) -> bool {
        ctx.extensions
            .get::<AttachmentCount>()
            .is_some_and(|c| c.0 > 0)
    }
    async fn run(&self, _: ToolCallContext, _: NoArgs) -> Result<Unit, ToolError> {
        Ok(Unit)
    }
}

// Tool::should_list (typed)

#[test]
fn default_returns_true() {
    assert!(Tool::should_list(&AlwaysTool, &ListToolsContext::default()));
}

#[test]
fn reads_extensions() {
    let tool = NeedsCwdTool;
    assert!(!Tool::should_list(&tool, &ListToolsContext::default()));

    let mut ctx = ListToolsContext::default();
    ctx.extensions
        .insert(Cwd(std::path::PathBuf::from("/work")));
    assert!(Tool::should_list(&tool, &ctx));
}

#[test]
fn reads_custom_extension() {
    let tool = NeedsAttachmentTool;
    assert!(!Tool::should_list(&tool, &ListToolsContext::default()));

    let mut zero = ListToolsContext::default();
    zero.extensions.insert(AttachmentCount(0));
    assert!(!Tool::should_list(&tool, &zero));

    let mut some = ListToolsContext::default();
    some.extensions.insert(AttachmentCount(3));
    assert!(Tool::should_list(&tool, &some));
}

// ToolDyn blanket forwarding

#[test]
fn dyn_forwards_default() {
    let tool: ArcTool = Arc::new(AlwaysTool);
    assert!(tool.should_list(&ListToolsContext::default()));
}

#[test]
fn dyn_forwards_custom() {
    let tool: ArcTool = Arc::new(NeedsCwdTool);
    assert!(!tool.should_list(&ListToolsContext::default()));

    let mut ctx = ListToolsContext::default();
    ctx.extensions
        .insert(Cwd(std::path::PathBuf::from("/home")));
    assert!(tool.should_list(&ctx));
}

#[test]
fn arc_dyn_callable() {
    let tool: Arc<dyn ToolDyn> = Arc::new(NeedsAttachmentTool);
    let mut ctx = ListToolsContext::default();
    ctx.extensions.insert(AttachmentCount(1));
    assert!(tool.should_list(&ctx));
}

// ListToolsContext

#[test]
fn list_ctx_default_is_empty() {
    let ctx = ListToolsContext::default();
    assert!(ctx.extensions.is_empty());
}

#[test]
fn list_ctx_insert_and_get() {
    let mut ctx = ListToolsContext::default();
    ctx.extensions.insert(Cwd(std::path::PathBuf::from("/a")));
    assert_eq!(
        ctx.extensions.get::<Cwd>().unwrap().0,
        std::path::PathBuf::from("/a")
    );
}

#[test]
fn list_ctx_clone_is_independent() {
    let mut ctx = ListToolsContext::default();
    ctx.extensions.insert(AttachmentCount(5));
    let mut copy = ctx.clone();
    copy.extensions.remove::<AttachmentCount>();
    assert!(ctx.extensions.contains::<AttachmentCount>());
    assert!(!copy.extensions.contains::<AttachmentCount>());
}

// TypedExtensions standalone

#[test]
fn typed_extensions_insert_get_remove() {
    let mut ext = echo_agent_tool_runtime::TypedExtensions::new();
    assert!(ext.is_empty());
    ext.insert(42_u32);
    assert_eq!(*ext.get::<u32>().unwrap(), 42);
    ext.remove::<u32>();
    assert!(ext.get::<u32>().is_none());
}
