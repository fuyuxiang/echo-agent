//! `ToolDyn` blanket impl + `ToolFamily` lookup.
//!
//! Covers the JSON-erased object-safe surface (`ToolDyn`) and the
//! variant-keyed family lookup (`ToolFamily`). Mirrors the toolbox
//! `EchoAgentToolDyn` / `EchoAgentToolFamily` tests but against the runtime's
//! typed `Tool` trait.

use std::sync::Arc;

use futures::StreamExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use echo_agent_tool_protocol::{ToolCapabilities, ToolId};
use echo_agent_tool_runtime::{
    ArcTool, ContentBlock, Tool, ToolCallContext, ToolDyn, ToolError, ToolErrorKind, ToolFamily,
    ToolOutput, ToolProgress, ToolStream, ToolStreamItem, ToolVariant, with_progress,
};
use echo_agent_tool_types::ToolDescription;

fn tid(s: &str) -> ToolId {
    ToolId::new(s).expect("test tool ids are well-formed")
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct EchoArgs {
    text: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct EchoOutput {
    text: String,
}

impl ToolOutput for EchoOutput {}
/// Blocking tool — exercises the default `Tool::execute` wrap-with-`run`
/// path through the `ToolDyn` blanket.
struct BlockingEcho;

impl Tool for BlockingEcho {
    type Args = EchoArgs;
    type Output = EchoOutput;

    fn id(&self) -> ToolId {
        tid("blocking_echo")
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("blocking_echo", "echo the input text")
    }

    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities {
            is_read_only: true,
            ..Default::default()
        }
    }

    async fn run(
        &self,
        _ctx: ToolCallContext,
        args: Self::Args,
    ) -> Result<Self::Output, ToolError> {
        Ok(EchoOutput { text: args.text })
    }
}

/// Streaming tool — exercises progress propagation through the blanket.
struct StreamingEcho;

impl Tool for StreamingEcho {
    type Args = EchoArgs;
    type Output = EchoOutput;

    fn id(&self) -> ToolId {
        tid("streaming_echo")
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("streaming_echo", "stream then echo")
    }

    async fn execute(&self, _ctx: ToolCallContext, args: Self::Args) -> ToolStream<Self::Output> {
        let progress = futures::stream::iter(vec![
            ToolProgress::Text {
                text: "tick".into(),
            },
            ToolProgress::Content {
                blocks: vec![ContentBlock::Text {
                    text: "tock".into(),
                }],
            },
        ]);
        with_progress(progress, async move { Ok(EchoOutput { text: args.text }) })
    }
}

/// Output type that always fails to serialize. Drives the `Tool::Output ->
/// Value` re-encoding error path through the `ToolDyn` blanket.
struct Unencodable;

impl ToolOutput for Unencodable {}
impl Serialize for Unencodable {
    fn serialize<S: serde::Serializer>(&self, _ser: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("intentionally unencodable"))
    }
}

struct UnencodableTool;

impl Tool for UnencodableTool {
    type Args = EchoArgs;
    type Output = Unencodable;

    fn id(&self) -> ToolId {
        tid("unencodable")
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("unencodable", "always returns a non-serializable output")
    }

    async fn run(
        &self,
        _ctx: ToolCallContext,
        _args: Self::Args,
    ) -> Result<Self::Output, ToolError> {
        Ok(Unencodable)
    }
}

// ── Tool with custom ToolOutput (non-empty) ──────────────────

/// Output that provides its own model-facing content blocks. The blanket
/// impl must forward these as-is rather than filling in the JSON fallback.
#[derive(Debug, Serialize)]
struct RichOutput {
    value: u32,
    blocks: Vec<ContentBlock>,
}

impl ToolOutput for RichOutput {
    fn model_output(&self) -> Vec<ContentBlock> {
        self.blocks.clone()
    }
}

struct RichTool;

impl Tool for RichTool {
    type Args = EchoArgs;
    type Output = RichOutput;

    fn id(&self) -> ToolId {
        tid("rich")
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("rich", "returns custom model output")
    }

    async fn run(
        &self,
        _ctx: ToolCallContext,
        _args: Self::Args,
    ) -> Result<Self::Output, ToolError> {
        Ok(RichOutput {
            value: 42,
            blocks: vec![
                ContentBlock::Text {
                    text: "summary".into(),
                },
                ContentBlock::Image {
                    mime_type: "image/png".into(),
                    data: "base64data".into(),
                    media_id: None,
                    filename: None,
                    path: None,
                    metadata: Default::default(),
                },
            ],
        })
    }
}

#[tokio::test]
async fn tool_dyn_preserves_custom_model_output() {
    let tool: ArcTool = Arc::new(RichTool);
    let mut stream = tool
        .execute(ToolCallContext::default(), json!({"text": "ignored"}))
        .await;
    let item = stream.next().await.expect("expected one item");
    match item {
        ToolStreamItem::Terminal(Ok(typed)) => {
            assert_eq!(typed.tool_id, tid("rich"));
            assert_eq!(
                typed.value,
                json!({"value": 42, "blocks": [
                    {"type": "text", "text": "summary"},
                    {"type": "image", "mime_type": "image/png", "data": "base64data"},
                ]})
            );
            // Custom model output preserved verbatim — no JSON fallback.
            assert_eq!(typed.model_output.len(), 2);
            assert_eq!(
                typed.model_output[0],
                ContentBlock::Text {
                    text: "summary".into(),
                },
            );
            assert!(matches!(typed.model_output[1], ContentBlock::Image { .. }));
        }
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
}

#[tokio::test]
async fn tool_dyn_delegates_id_description_capabilities() {
    let tool: ArcTool = Arc::new(BlockingEcho);
    assert_eq!(tool.id(), tid("blocking_echo"));
    assert_eq!(
        tool.description(&echo_agent_tool_runtime::ListToolsContext::default())
            .name,
        "blocking_echo"
    );
    assert!(tool.capabilities().is_read_only);
}

#[tokio::test]
async fn tool_dyn_blanket_encodes_terminal_output() {
    let tool: ArcTool = Arc::new(BlockingEcho);
    let mut stream = tool
        .execute(ToolCallContext::default(), json!({"text": "hi"}))
        .await;
    let item = stream.next().await.expect("expected one item");
    match item {
        ToolStreamItem::Terminal(Ok(typed)) => {
            assert_eq!(typed.tool_id, tid("blocking_echo"));
            assert_eq!(typed.value, json!({"text": "hi"}));
            // EchoOutput uses the default ToolOutput which
            // serialises self to a JSON text block (MCP-compliant).
            assert_eq!(typed.model_output.len(), 1);
            assert_eq!(
                typed.model_output[0],
                ContentBlock::Text {
                    text: r#"{"text":"hi"}"#.into(),
                },
            );
        }
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn tool_dyn_blanket_passes_progress_through() {
    let tool: ArcTool = Arc::new(StreamingEcho);
    let items: Vec<_> = tool
        .execute(ToolCallContext::default(), json!({"text": "done"}))
        .await
        .collect()
        .await;
    assert_eq!(items.len(), 3, "expected 2 progress + 1 terminal");

    assert!(matches!(
        items[0],
        ToolStreamItem::Progress(ToolProgress::Text { .. }),
    ));
    assert!(matches!(
        items[1],
        ToolStreamItem::Progress(ToolProgress::Content { .. }),
    ));
    match &items[2] {
        ToolStreamItem::Terminal(Ok(typed)) => {
            assert_eq!(typed.tool_id, tid("streaming_echo"));
            assert_eq!(typed.value, json!({"text": "done"}));
        }
        other => panic!("expected Terminal(Ok) at end, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_dyn_invalid_args_become_invalid_arguments_terminal() {
    let tool: ArcTool = Arc::new(BlockingEcho);
    // `text` is required and must be a string — `null` fails serde.
    let mut stream = tool
        .execute(ToolCallContext::default(), json!({"text": null}))
        .await;
    match stream.next().await.unwrap() {
        ToolStreamItem::Terminal(Err(ref err)) if err.kind == ToolErrorKind::InvalidArguments => {
            assert!(!err.detail.is_empty(), "detail should describe the failure");
        }
        other => panic!("expected Terminal(Err(InvalidArguments)), got {other:?}"),
    }
    assert!(stream.next().await.is_none(), "stream should be exhausted");
}

#[tokio::test]
async fn tool_dyn_unencodable_output_becomes_execution_terminal() {
    let tool: ArcTool = Arc::new(UnencodableTool);
    let mut stream = tool
        .execute(ToolCallContext::default(), json!({"text": "anything"}))
        .await;
    match stream.next().await.unwrap() {
        ToolStreamItem::Terminal(Err(ref err)) if err.kind == ToolErrorKind::Execution => {
            assert!(
                err.detail.contains("unencodable"),
                "detail should mention the tool id, got: {}",
                err.detail
            );
        }
        other => panic!("expected Terminal(Err(Execution)), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// ToolFamily
// ---------------------------------------------------------------------------

/// Backend-flavoured echo. Two variants share the `echo` tool id and only
/// differ in the prefix attached to the output text — enough to assert
/// the family routes lookups to distinct implementations.
struct PrefixedEcho {
    prefix: &'static str,
}

impl Tool for PrefixedEcho {
    type Args = EchoArgs;
    type Output = EchoOutput;

    fn id(&self) -> ToolId {
        tid("echo")
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("echo", "prefixed echo")
    }

    async fn run(
        &self,
        _ctx: ToolCallContext,
        args: Self::Args,
    ) -> Result<Self::Output, ToolError> {
        Ok(EchoOutput {
            text: format!("{}{}", self.prefix, args.text),
        })
    }
}

const ALT_VARIANT: &str = "alt";

struct EchoFamily;

impl ToolFamily for EchoFamily {
    fn id(&self) -> ToolId {
        tid("echo")
    }

    fn get_tool(&self, variant: &ToolVariant) -> Option<ArcTool> {
        match variant {
            ToolVariant::Default => Some(Arc::new(PrefixedEcho { prefix: "default:" })),
            ToolVariant::Variant(v) if v == ALT_VARIANT => {
                Some(Arc::new(PrefixedEcho { prefix: "alt:" }))
            }
            ToolVariant::Variant(_) => None,
        }
    }

    fn variants(&self) -> Vec<ToolVariant> {
        vec![
            ToolVariant::Default,
            ToolVariant::Variant(ALT_VARIANT.into()),
        ]
    }
}

#[tokio::test]
async fn tool_family_default_variant_routes_to_default_impl() {
    let family = EchoFamily;
    assert_eq!(family.id(), tid("echo"));
    let tool = family
        .get_tool(&ToolVariant::Default)
        .expect("default variant must exist");
    let mut stream = tool
        .execute(ToolCallContext::default(), json!({"text": "x"}))
        .await;
    match stream.next().await.unwrap() {
        ToolStreamItem::Terminal(Ok(typed)) => {
            assert_eq!(typed.tool_id, tid("echo"));
            assert_eq!(typed.value, json!({"text": "default:x"}));
        }
        other => panic!("expected default impl output, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_family_named_variant_routes_to_named_impl() {
    let family = EchoFamily;
    let tool = family
        .get_tool(&ToolVariant::Variant(ALT_VARIANT.into()))
        .expect("alt variant must exist");
    let mut stream = tool
        .execute(ToolCallContext::default(), json!({"text": "x"}))
        .await;
    match stream.next().await.unwrap() {
        ToolStreamItem::Terminal(Ok(typed)) => {
            assert_eq!(typed.tool_id, tid("echo"));
            assert_eq!(typed.value, json!({"text": "alt:x"}));
        }
        other => panic!("expected alt impl output, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_family_unknown_variant_returns_none() {
    let family = EchoFamily;
    assert!(
        family
            .get_tool(&ToolVariant::Variant("nonexistent".into()))
            .is_none()
    );
}

#[tokio::test]
async fn tool_family_variants_lists_every_exposed_variant() {
    let family = EchoFamily;
    let variants = family.variants();
    assert_eq!(variants.len(), 2);
    assert!(variants.contains(&ToolVariant::Default));
    assert!(variants.contains(&ToolVariant::Variant(ALT_VARIANT.into())));
}

#[tokio::test]
async fn tool_family_default_variant_name_defaults_to_none() {
    let family = EchoFamily;
    assert!(family.default_variant_name().is_none());
}

// ---------------------------------------------------------------------------
// Object safety / ergonomic checks
// ---------------------------------------------------------------------------

#[test]
fn tool_dyn_is_object_safe_in_arc_and_box() {
    let _arc: Arc<dyn ToolDyn> = Arc::new(BlockingEcho);
    let _boxed: Box<dyn ToolDyn> = Box::new(StreamingEcho);
}

#[test]
fn tool_family_is_object_safe_in_arc_and_box() {
    let _arc: Arc<dyn ToolFamily> = Arc::new(EchoFamily);
    let _boxed: Box<dyn ToolFamily> = Box::new(EchoFamily);
}

#[test]
fn arc_tool_alias_holds_heterogeneous_tools() {
    // The whole point of `ArcTool` — many typed `Tool` impls collapse
    // into one container shape via the blanket impl.
    let tools: Vec<ArcTool> = vec![Arc::new(BlockingEcho), Arc::new(StreamingEcho)];
    assert_eq!(tools.len(), 2);
    let ids: Vec<_> = tools.iter().map(|t| t.id()).collect();
    assert!(ids.contains(&tid("blocking_echo")));
    assert!(ids.contains(&tid("streaming_echo")));
}

#[test]
fn tool_variant_round_trips_through_clone_and_eq() {
    let a = ToolVariant::Variant("es".into());
    let b = a.clone();
    assert_eq!(a, b);
    assert_ne!(ToolVariant::Default, ToolVariant::Variant("default".into()));
}

/// The blanket impl is what makes `ToolDyn` ergonomic. This compile-time
/// check makes sure a fresh `Tool` impl can be passed where `&dyn ToolDyn`
/// is expected without an explicit upcast.
fn _accepts_dyn(_: &dyn ToolDyn) {}

#[allow(dead_code)]
fn _compile_time_blanket_check() {
    let tool = BlockingEcho;
    _accepts_dyn(&tool);
    let tool = StreamingEcho;
    _accepts_dyn(&tool);

    // The trait objects themselves must be `Send + Sync` so they can be
    // shared across tasks without further bounds at the call site.
    fn _is_send_sync<T: Send + Sync + ?Sized>() {}
    _is_send_sync::<dyn ToolDyn>();
    _is_send_sync::<dyn ToolFamily>();
}
