//! Streaming `Tool::execute` overrides — interleaving Progress with a
//! single Terminal item.

use std::pin::Pin;

use futures::stream::{self, Stream, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use echo_agent_tool_protocol::{StreamingSpec, ToolCapabilities, ToolId};
use echo_agent_tool_runtime::{
    ContentBlock, Tool, ToolCallContext, ToolError, ToolErrorKind, ToolOutput, ToolProgress,
    ToolStream, ToolStreamItem, with_progress,
};
use echo_agent_tool_types::ToolDescription;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct EmptyArgs {}

#[derive(Debug, Serialize, PartialEq)]
struct UnitOutput {
    pub value: u32,
}

impl ToolOutput for UnitOutput {}
struct StreamingOk;

impl Tool for StreamingOk {
    type Args = EmptyArgs;
    type Output = UnitOutput;

    fn id(&self) -> ToolId {
        ToolId::new("streaming_ok").unwrap()
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("streaming_ok", "ok")
    }

    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities {
            streaming: Some(StreamingSpec {
                subkind: "streaming_ok_chunk".to_owned(),
                max_delta_bytes: None,
            }),
            ..Default::default()
        }
    }

    async fn execute(&self, _ctx: ToolCallContext, _args: Self::Args) -> ToolStream<UnitOutput> {
        let progress: Pin<Box<dyn Stream<Item = ToolProgress> + Send>> =
            Box::pin(stream::iter(vec![
                ToolProgress::Text {
                    text: "first".into(),
                },
                ToolProgress::Text {
                    text: "second".into(),
                },
                ToolProgress::Content {
                    blocks: vec![ContentBlock::Text {
                        text: "third block".into(),
                    }],
                },
            ]));
        with_progress(progress, async move { Ok(UnitOutput { value: 7 }) })
    }
}

struct StreamingErr;

impl Tool for StreamingErr {
    type Args = EmptyArgs;
    type Output = UnitOutput;

    fn id(&self) -> ToolId {
        ToolId::new("streaming_err").unwrap()
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("streaming_err", "err")
    }

    async fn execute(&self, _ctx: ToolCallContext, _args: Self::Args) -> ToolStream<UnitOutput> {
        let progress = stream::iter(vec![ToolProgress::Text {
            text: "before terminal".into(),
        }]);
        with_progress(progress, async move {
            Err(ToolError::custom("intentional_failure", "boom"))
        })
    }
}

#[tokio::test]
async fn streaming_ok_emits_progress_then_terminal() {
    let tool = StreamingOk;
    let ctx = ToolCallContext::default();
    let mut items: Vec<_> = tool.execute(ctx, EmptyArgs {}).await.collect().await;
    assert_eq!(items.len(), 4, "expected 3 progress + 1 terminal");

    let terminal = items.pop().unwrap();
    assert!(terminal.is_terminal(), "last item must be Terminal");
    let earlier_terminals = items.iter().filter(|i| i.is_terminal()).count();
    assert_eq!(earlier_terminals, 0, "Terminal must only appear last");

    match terminal {
        ToolStreamItem::Terminal(Ok(out)) => assert_eq!(out, UnitOutput { value: 7 }),
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }

    let progress_kinds: Vec<_> = items
        .into_iter()
        .map(|item| match item {
            ToolStreamItem::Progress(p) => p,
            ToolStreamItem::Terminal(_) => unreachable!(),
        })
        .collect();
    assert_eq!(progress_kinds.len(), 3);
    assert!(matches!(progress_kinds[0], ToolProgress::Text { .. }));
    assert!(matches!(progress_kinds[2], ToolProgress::Content { .. }));
}

#[tokio::test]
async fn streaming_err_propagates_through_terminal() {
    let tool = StreamingErr;
    let ctx = ToolCallContext::default();
    let items: Vec<_> = tool.execute(ctx, EmptyArgs {}).await.collect().await;
    assert_eq!(items.len(), 2);

    match &items[0] {
        ToolStreamItem::Progress(ToolProgress::Text { text }) => {
            assert_eq!(text, "before terminal");
        }
        other => panic!("expected Progress(Text), got {other:?}"),
    }

    match &items[1] {
        ToolStreamItem::Terminal(Err(err)) if err.kind == ToolErrorKind::Custom => {
            let code = err
                .details
                .as_ref()
                .and_then(|d| d.get("code"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert_eq!(code, "intentional_failure");
            assert_eq!(err.detail, "boom");
        }
        other => panic!("expected Terminal(Err(Custom)), got {other:?}"),
    }
}

#[tokio::test]
async fn streaming_progress_count_is_independent_of_args() {
    // Distinct invocations on the same tool produce the same shape.
    let tool = StreamingOk;
    for _ in 0..3 {
        let count = tool
            .execute(ToolCallContext::default(), EmptyArgs {})
            .await
            .count()
            .await;
        assert_eq!(count, 4);
    }
}

#[tokio::test]
async fn empty_progress_still_yields_terminal() {
    // Building `with_progress` on an empty stream still produces exactly
    // one terminal item — the same shape `terminal_only` produces.
    let progress = stream::iter(Vec::<ToolProgress>::new());
    let mut stream = with_progress(progress, async move { Ok::<u32, ToolError>(99) });
    let item = stream.next().await.unwrap();
    match item {
        ToolStreamItem::Terminal(Ok(v)) => assert_eq!(v, 99),
        other => panic!("expected Terminal(Ok(99)), got {other:?}"),
    }
    assert!(stream.next().await.is_none());
}
