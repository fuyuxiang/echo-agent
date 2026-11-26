//! Default `Tool::execute` wrapping a blocking `Tool::run`.

use futures::StreamExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use echo_agent_tool_protocol::ToolId;
use echo_agent_tool_runtime::{
    Tool, ToolCallContext, ToolError, ToolErrorKind, ToolOutput, ToolStreamItem,
};
use echo_agent_tool_types::ToolDescription;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct EchoArgs {
    text: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct EchoOutput {
    text: String,
}

impl ToolOutput for EchoOutput {}
struct BlockingOk;

impl Tool for BlockingOk {
    type Args = EchoArgs;
    type Output = EchoOutput;

    fn id(&self) -> ToolId {
        ToolId::new("blocking_ok").unwrap()
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("blocking_ok", "ok")
    }

    async fn run(
        &self,
        _ctx: ToolCallContext,
        args: Self::Args,
    ) -> Result<Self::Output, ToolError> {
        Ok(EchoOutput { text: args.text })
    }
}

struct BlockingErr;

impl Tool for BlockingErr {
    type Args = EchoArgs;
    type Output = EchoOutput;

    fn id(&self) -> ToolId {
        ToolId::new("blocking_err").unwrap()
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("blocking_err", "err")
    }

    async fn run(
        &self,
        _ctx: ToolCallContext,
        args: Self::Args,
    ) -> Result<Self::Output, ToolError> {
        Err(ToolError::invalid_arguments(format!(
            "rejected: {}",
            args.text
        )))
    }
}

struct UnimplementedTool;

impl Tool for UnimplementedTool {
    type Args = EchoArgs;
    type Output = EchoOutput;

    fn id(&self) -> ToolId {
        ToolId::new("unimplemented_tool").unwrap()
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("unimplemented_tool", "neither")
    }
}

#[tokio::test]
async fn blocking_ok_wraps_into_single_terminal() {
    let tool = BlockingOk;
    let mut stream = tool
        .execute(
            ToolCallContext::default(),
            EchoArgs {
                text: "hello".into(),
            },
        )
        .await;
    let first = stream.next().await.expect("expected one item");
    assert!(first.is_terminal());
    match first {
        ToolStreamItem::Terminal(Ok(EchoOutput { text })) => assert_eq!(text, "hello"),
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
    assert!(stream.next().await.is_none(), "stream should be exhausted");
}

#[tokio::test]
async fn blocking_err_wraps_into_single_terminal() {
    let tool = BlockingErr;
    let mut stream = tool
        .execute(
            ToolCallContext::default(),
            EchoArgs {
                text: "rejected".into(),
            },
        )
        .await;
    let first = stream.next().await.expect("expected one item");
    match first {
        ToolStreamItem::Terminal(Err(ref err)) if err.kind == ToolErrorKind::InvalidArguments => {
            assert_eq!(err.detail, "rejected: rejected");
        }
        other => panic!("expected Terminal(Err(InvalidArguments)), got {other:?}"),
    }
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn unimplemented_tool_returns_not_implemented_terminal() {
    let tool = UnimplementedTool;
    let item = tool
        .execute(ToolCallContext::default(), EchoArgs { text: "x".into() })
        .await
        .next()
        .await
        .unwrap();
    match item {
        ToolStreamItem::Terminal(Err(ref err))
            if err.kind == echo_agent_tool_runtime::error::ToolErrorKind::NotImplemented =>
        {
            assert!(
                err.detail.contains("run") && err.detail.contains("execute"),
                "detail should mention both methods, got: {}",
                err.detail
            );
        }
        other => panic!("expected Terminal(Err(NotImplemented)), got {other:?}"),
    }
}

#[tokio::test]
async fn run_takes_args_by_value() {
    // The trait `run` consumes args; this would not compile if the
    // signature accidentally borrowed.
    let tool = BlockingOk;
    let args = EchoArgs {
        text: "consumed".into(),
    };
    let result = tool.run(ToolCallContext::default(), args).await.unwrap();
    assert_eq!(result.text, "consumed");
}

#[tokio::test]
async fn execute_default_drains_in_one_pass() {
    // A stream from the default impl should always have exactly one item.
    let tool = BlockingOk;
    let count = tool
        .execute(ToolCallContext::default(), EchoArgs { text: "n".into() })
        .await
        .count()
        .await;
    assert_eq!(count, 1);
}
