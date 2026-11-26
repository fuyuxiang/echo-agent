//! The unified `Tool` trait, the streaming primitives it produces, and the
//! helper constructors tool authors use to build well-formed streams.
//!
//! `Tool::execute` is the canonical streaming entry point; the runtime
//! always calls it. The default impl wraps `Tool::run` (the simpler
//! convenience hook) into a single-item terminal stream so blocking tools
//! don't have to think about streaming. A tool that overrides neither gets
//! a `NotImplemented` terminal at runtime.
//!
//! `ToolStream<T>` is a type alias for an opaque pinned stream; the helper
//! free functions [`terminal_only`] and [`with_progress`] are the supported
//! ways to build one. Stream invariant: at most arbitrarily many `Progress`
//! items, ending in exactly one `Terminal`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use echo_agent_tool_protocol::{ToolCapabilities, ToolId};
use echo_agent_tool_types::ToolDescription;

use crate::context::{ListToolsContext, ToolCallContext};
use crate::error::ToolError;
use crate::render::{ToolChatCompletionResponse, ToolOutput};

/// The unified tool trait used by every tool source.
///
/// Implement either `run` (blocking) or `execute` (streaming). The
/// runtime only ever invokes `execute`.
pub trait Tool: Send + Sync {
    /// Typed input. Must be deserialisable from JSON for wire dispatch.
    type Args: for<'de> Deserialize<'de> + JsonSchema + Send + 'static;

    /// Typed output. Must implement [`ToolOutput`] which provides
    /// model-facing content blocks and optional chat-completion
    /// responses. All methods have defaults, so most output types
    /// just need:
    /// ```rust,ignore
    /// impl ToolOutput for MyOutput {}
    /// ```
    type Output: Serialize + ToolOutput + Send + 'static;

    /// Stable identity used by the runtime to route to this tool.
    fn id(&self) -> ToolId;

    /// Model-facing description and argument schema.
    ///
    /// Receives the per-turn [`ListToolsContext`] (viewer context,
    /// attachments, etc.) — the same context [`Tool::should_list`] consumes
    /// — so descriptions can be context-aware. Most tools ignore `_ctx` and
    /// return a static description. Callers outside a listing turn pass
    /// [`ListToolsContext::default`].
    fn description(&self, _ctx: &ListToolsContext) -> ToolDescription;

    /// Per-tool capability flags (concurrency, scope, frame caps, ...).
    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities::default()
    }

    /// Whether [`Tool::description`] varies with the per-turn
    /// [`ListToolsContext`]. `false` for the common case of a static
    /// description.
    fn has_dynamic_description(&self) -> bool {
        false
    }

    /// Per-turn listing predicate. Return `false` to exclude this tool
    /// from the model-facing manifest for a given turn.
    fn should_list(&self, _ctx: &ListToolsContext) -> bool {
        true
    }

    /// Streaming entry point. Default impl wraps `run` into a single-item
    /// stream so blocking tools just override `run`.
    ///
    /// Uses a native `async fn` in trait (RPITIT) with an explicit `Send`
    /// bound rather than `#[async_trait]`, so the returned future is not
    /// boxed. The `Tool` trait is only ever consumed generically (type
    /// erasure goes through [`ToolDyn`]), so it does not need to be
    /// dyn-compatible.
    fn execute(
        &self,
        ctx: ToolCallContext,
        args: Self::Args,
    ) -> impl Future<Output = ToolStream<Self::Output>> + Send {
        async move {
            let result = self.run(ctx, args).await;
            terminal_only(result)
        }
    }

    /// Blocking convenience entry point. Default returns
    /// `Err(ToolError::not_implemented(...))` so a tool that overrides
    /// neither method fails loudly at the first call.
    fn run(
        &self,
        _ctx: ToolCallContext,
        _args: Self::Args,
    ) -> impl Future<Output = Result<Self::Output, ToolError>> + Send {
        async move {
            Err(ToolError::not_implemented(
                "Tool must implement either `run` or `execute`",
            ))
        }
    }
}

/// Stream of items a tool produces during a single call. Shape:
/// `[Progress(_)*, Terminal(Result<T, ToolError>)]`.
pub type ToolStream<T> = Pin<Box<dyn Stream<Item = ToolStreamItem<T>> + Send>>;

/// One item in a [`ToolStream`].
#[derive(Debug)]
pub enum ToolStreamItem<T> {
    /// Intermediate progress. Zero or more per stream.
    Progress(ToolProgress),
    /// Terminal result. Exactly one per stream, always last.
    Terminal(Result<T, ToolError>),
}

impl<T> ToolStreamItem<T> {
    /// `true` for the `Terminal` variant. Stream consumers use this to
    /// short-circuit once the final item has been seen.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal(_))
    }
}

/// Open-ended progress payload. The `Custom` arm is the escape hatch for
/// tool-specific shapes that don't map onto `Text` or `Content`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolProgress {
    /// Free-form text chunk (terminal stdout, log line, partial response).
    Text { text: String },
    /// Rich content blocks.
    Content { blocks: Vec<ContentBlock> },
    /// Tool-defined progress payload. `subkind` is a stable snake-case
    /// discriminator owned by the tool. The outer `"kind"` serde tag is
    /// always `"custom"` for this variant; `subkind` is the producer's
    /// own discriminator and lives one level deeper to avoid colliding
    /// with the tag.
    Custom {
        subkind: String,
        payload: serde_json::Value,
    },
}

/// Rich content block for `ToolProgress::Content`. Mirrors the wire-side
/// `McpBlock` shape on the protocol crate so adapters can move blocks
/// across the wire boundary without re-encoding.
///
/// The `Image` variant carries optional metadata fields (`media_id`,
/// `filename`, `path`, `metadata`) that the EchoAgent SLOP converter uses to
/// build `Media` objects with the right identifiers and file paths. Tool
/// authors populate these in their `ToolOutput::model_output()` impls so
/// downstream consumers don't need to know the concrete tool type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text. Equivalent to `ToolProgress::Text` but allowed inside a
    /// content list so a tool can interleave images and text.
    Text { text: String },
    /// Image. `mime_type` is e.g. `"image/png"`; `data` is base64.
    ///
    /// Optional metadata fields are used by the EchoAgent SLOP converter to
    /// produce `Media` objects with the correct identifiers:
    /// - `media_id`: unique image ID for referencing in subsequent tool calls
    /// - `filename`: human-readable filename (e.g. `"xK29f.png"`)
    /// - `path`: file path on the EchoAgent Computer filesystem
    /// - `metadata`: arbitrary key-value pairs (e.g. `title`, `webpage_url`)
    Image {
        #[serde(alias = "mimeType")]
        mime_type: String,
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        metadata: std::collections::HashMap<String, String>,
    },
    /// Resource pointer. `uri` is required; `mime_type` and `text` are
    /// optional preview data.
    Resource {
        uri: String,
        #[serde(default, skip_serializing_if = "Option::is_none", alias = "mimeType")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

/// Build a single-item stream containing only the terminal result.
///
/// The most common shape — a blocking tool's `Tool::run` is wrapped this
/// way by the default `execute` impl.
pub fn terminal_only<T: Send + 'static>(result: Result<T, ToolError>) -> ToolStream<T> {
    Box::pin(stream::iter(std::iter::once(ToolStreamItem::Terminal(
        result,
    ))))
}

/// Build a stream that emits each progress item from `progress` then
/// resolves `terminal` and emits its result as the final `Terminal` item.
///
/// `terminal` is awaited only after `progress` has fully drained, so a
/// progress producer that pulls from the same upstream can drive the
/// terminal value without conflicts.
pub fn with_progress<T, P, F>(progress: P, terminal: F) -> ToolStream<T>
where
    T: Send + 'static,
    P: Stream<Item = ToolProgress> + Send + 'static,
    F: Future<Output = Result<T, ToolError>> + Send + 'static,
{
    let progress = progress.map(ToolStreamItem::Progress);
    let tail = stream::once(async move { ToolStreamItem::Terminal(terminal.await) });
    Box::pin(progress.chain(tail))
}

/// Type-erased tool output that bundles the serialised JSON value with
/// model-facing content blocks extracted at serialisation time.
///
/// When the `ToolDyn` blanket impl serialises a typed `Tool::Output` to
/// JSON it also calls [`ToolOutput::model_output`] and
/// [`ToolOutput::chat_completion_output`], capturing both
/// here so downstream consumers never need to deserialise the `Value`
/// back into the concrete type.
///
/// # MCP invariant
///
/// `model_output` is **always non-empty**. The [`ToolOutput`]
/// default serialises the typed output as a JSON text block, so even
/// tools that never override `model_output()` produce MCP-compliant
/// content.  Tools that override the method are expected to return at
/// least one block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedToolOutput {
    /// Identity of the tool that produced this output.
    pub tool_id: ToolId,
    /// Serialised JSON representation of the tool output.
    pub value: Value,
    /// Model-facing content blocks. Always contains at least one block
    /// (MCP compliance — see struct-level docs).
    pub model_output: Vec<ContentBlock>,
    /// Optional chat-completion response frame, extracted from the typed
    /// output via [`ToolOutput`] at type-erasure time.
    ///
    /// `None` for the vast majority of tools. Present when a tool
    /// produces a frontend-ready chat response (render cards, progress
    /// reports, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_completion_output: Option<ToolChatCompletionResponse>,
}

impl TypedToolOutput {
    /// Convenience constructor for building a `TypedToolOutput` from an
    /// already-serialised `Value` (e.g. at wire decode boundaries).
    ///
    /// `model_output` is derived from the value via
    /// [`extract_content_blocks`](crate::render::extract_content_blocks);
    /// `chat_completion_output` is always `None` since the wire format
    /// does not carry it.
    pub fn from_value(tool_id: ToolId, value: Value) -> Self {
        let model_output = crate::render::extract_content_blocks(&value);
        Self {
            tool_id,
            value,
            model_output,
            chat_completion_output: None,
        }
    }

    /// Override the `chat_completion_output` that [`Self::from_value`]
    /// leaves `None` — used by wire-decode boundaries that recover the
    /// frame the plain `from_value` path cannot carry.
    pub fn with_chat_completion_output(
        mut self,
        chat_completion_output: Option<ToolChatCompletionResponse>,
    ) -> Self {
        self.chat_completion_output = chat_completion_output;
        self
    }
}

impl ToolOutput for TypedToolOutput {
    fn model_output(&self) -> Vec<ContentBlock> {
        self.model_output.clone()
    }

    fn chat_completion_output(&self) -> Option<ToolChatCompletionResponse> {
        self.chat_completion_output.clone()
    }
}

/// Type erased tool trait. Auto-generated for every typed Tool implementation.
#[async_trait]
pub trait ToolDyn: Send + Sync {
    /// Stable identity. Same value as [`Tool::id`].
    fn id(&self) -> ToolId;

    /// Model-facing description. Same value as [`Tool::description`].
    fn description(&self, ctx: &ListToolsContext) -> ToolDescription;

    /// Per-tool capability flags. Same value as [`Tool::capabilities`].
    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities::default()
    }

    /// Same value as [`Tool::has_dynamic_description`].
    fn has_dynamic_description(&self) -> bool {
        false
    }

    fn should_list(&self, _ctx: &ListToolsContext) -> bool {
        true
    }

    /// JSON-typed streaming entry point. The returned stream MUST honour
    /// the same `[Progress*, Terminal]` invariant as [`ToolStream`].
    ///
    /// Terminal items carry [`TypedToolOutput`] which bundles both the
    /// serialised JSON `Value` and the model-facing content blocks
    /// extracted from the typed output.
    async fn execute(&self, ctx: ToolCallContext, args: Value) -> ToolStream<TypedToolOutput>;
}

#[async_trait]
impl<T: Tool> ToolDyn for T {
    fn id(&self) -> ToolId {
        Tool::id(self)
    }

    fn description(&self, ctx: &ListToolsContext) -> ToolDescription {
        Tool::description(self, ctx)
    }

    fn capabilities(&self) -> ToolCapabilities {
        Tool::capabilities(self)
    }

    fn has_dynamic_description(&self) -> bool {
        Tool::has_dynamic_description(self)
    }

    fn should_list(&self, ctx: &ListToolsContext) -> bool {
        Tool::should_list(self, ctx)
    }

    async fn execute(&self, ctx: ToolCallContext, args: Value) -> ToolStream<TypedToolOutput> {
        let typed_args: T::Args = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return terminal_only(Err(ToolError::invalid_arguments(e.to_string()))),
        };

        let tool_id = Tool::id(self);
        let typed_stream = Tool::execute(self, ctx, typed_args).await;

        Box::pin(typed_stream.map(move |item| {
            match item {
                ToolStreamItem::Progress(p) => ToolStreamItem::Progress(p),
                ToolStreamItem::Terminal(Ok(out)) => {
                    // TypedToolOutput::value and the model_output
                    // fallback.
                    match serde_json::to_value(&out) {
                        Ok(value) => {
                            let custom = out.model_output();
                            let model_output = if custom.is_empty() {
                                // Default path: extract blocks from the
                                // already-serialised Value.
                                crate::render::extract_content_blocks(&value)
                            } else {
                                custom
                            };
                            let chat_completion_output = out.chat_completion_output();
                            ToolStreamItem::Terminal(Ok(TypedToolOutput {
                                tool_id: tool_id.clone(),
                                value,
                                model_output,
                                chat_completion_output,
                            }))
                        }
                        Err(e) => ToolStreamItem::Terminal(Err(ToolError::execution(
                            tool_id.clone(),
                            format!("serializing tool output to JSON: {e}"),
                        )
                        .with_source(e))),
                    }
                }
                ToolStreamItem::Terminal(Err(e)) => ToolStreamItem::Terminal(Err(e)),
            }
        }))
    }
}

/// Convenience alias for the most common [`ToolDyn`] handle shape.
pub type ArcTool = Arc<dyn ToolDyn>;

/// Variant identifier for tools that ship multiple implementations under
/// one stable [`ToolId`].
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum ToolVariant {
    /// The implicit fallback variant.
    Default,
    /// A named variant. The string is treated opaquely by the registry.
    Variant(String),
}

/// Group of related tools that share one [`ToolId`] but route to different
/// implementations chosen by a [`ToolVariant`].
pub trait ToolFamily: Send + Sync {
    /// Identity shared by every variant in this family.
    fn id(&self) -> ToolId;

    /// Resolve a `variant` to its concrete tool. Returns `None` when the
    /// family does not expose the requested variant.
    fn get_tool(&self, variant: &ToolVariant) -> Option<ArcTool>;

    /// Every variant the family exposes. Registries iterate this once at
    /// startup and cache the results, so allocating here is fine.
    fn variants(&self) -> Vec<ToolVariant>;

    /// Variant name the default falls back to when the family's `Default`
    /// arm is itself a named variant. Returns `None` when the default is
    /// the [`ToolVariant::Default`] sentinel.
    fn default_variant_name(&self) -> Option<&'static str> {
        None
    }
}

/// Convenience alias for the most common [`ToolFamily`] handle shape.
pub type ArcToolFamily = Arc<dyn ToolFamily>;
