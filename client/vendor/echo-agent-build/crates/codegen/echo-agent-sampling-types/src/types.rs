use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::num::NonZeroU64;

// ============================================================================
// TraceContext — cloneable, type-erased context for request tracing
// ============================================================================

/// Object-safe trait for opaque tracing context attached to requests.
///
/// `Clone` is not object-safe, so we use a `clone_box` method instead.
/// Any concrete type that is `Clone + Send + Sync + Debug + 'static` gets a
/// blanket impl, so callers just do:
///
/// ```ignore
/// request.trace = Some(Box::new(my_concrete_trace));
/// ```
pub trait TraceContext: std::any::Any + Send + Sync + std::fmt::Debug {
    /// Clone this trace context into a new `Box`.
    fn clone_box(&self) -> Box<dyn TraceContext>;

    /// Upcast to `&dyn Any` for downcasting back to the concrete type.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Blanket impl: any `T: Clone + Send + Sync + Debug + 'static` is a `TraceContext`.
impl<T> TraceContext for T
where
    T: Clone + Send + Sync + std::fmt::Debug + 'static,
{
    fn clone_box(&self) -> Box<dyn TraceContext> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl Clone for Box<dyn TraceContext> {
    fn clone(&self) -> Self {
        // Explicitly dereference to `&dyn TraceContext` so `clone_box()` dispatches
        // through the vtable to the concrete type's implementation.
        //
        // Without this, `self.clone_box()` resolves via auto-deref to
        // `<Box<dyn TraceContext> as TraceContext>::clone_box()` (from the blanket impl),
        // which calls `self.clone()` → `self.clone_box()` → infinite recursion.
        let inner: &dyn TraceContext = &**self;
        inner.clone_box()
    }
}

/// Deserialize a field that may be `null` as the default value.
/// This is useful for fields like `Vec<T>` where `null` should become `vec![]`.
fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(|opt| opt.unwrap_or_default())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatCompletionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub messages: Vec<ChatRequestMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Whether the provider may emit more than one tool call in a model turn.
    /// `None` preserves the provider default; the sampler can set `false` for
    /// one recovery attempt when a provider emits ambiguous parallel deltas.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_parameters: Option<SearchParameters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<crate::rs::ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,

    /// custom headers
    #[serde(skip)]
    pub x_echo_agent_conv_id: Option<String>,
    #[serde(skip)]
    pub x_echo_agent_req_id: Option<String>,
    #[serde(skip)]
    pub x_echo_agent_session_id: Option<String>,
    #[serde(skip)]
    pub x_echo_agent_turn_idx: Option<String>,
    #[serde(skip)]
    pub x_echo_agent_agent_id: Option<String>,
    #[serde(skip)]
    pub x_echo_agent_deployment_id: Option<String>,
    #[serde(skip)]
    pub x_echo_agent_user_id: Option<String>,

    /// Optional opaque tracing context (e.g., where to persist the finalized request payload).
    /// This is intentionally not serialized or deserialized.
    /// Consumers downcast via `trace.as_ref().unwrap().as_any().downcast_ref::<T>()`.
    #[serde(skip)]
    pub trace: Option<Box<dyn TraceContext>>,
}

impl ChatCompletionRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatRequestMessage>) -> Self {
        Self {
            model: Some(model.into()),
            messages,
            temperature: None,
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            search_parameters: None,
            response_format: None,
            reasoning_effort: None,
            x_echo_agent_conv_id: None,
            x_echo_agent_req_id: None,
            x_echo_agent_session_id: None,
            x_echo_agent_turn_idx: None,
            x_echo_agent_agent_id: None,
            x_echo_agent_deployment_id: None,
            x_echo_agent_user_id: None,
            trace: None,
        }
    }

    pub fn from_messages(messages: Vec<ChatRequestMessage>) -> Self {
        Self {
            model: None,
            messages,
            temperature: None,
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            search_parameters: None,
            response_format: None,
            reasoning_effort: None,
            x_echo_agent_conv_id: None,
            x_echo_agent_req_id: None,
            x_echo_agent_session_id: None,
            x_echo_agent_turn_idx: None,
            x_echo_agent_agent_id: None,
            x_echo_agent_deployment_id: None,
            x_echo_agent_user_id: None,
            trace: None,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn with_tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.tool_choice = Some(tool_choice);
        self
    }

    pub fn set_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    pub fn with_top_p(mut self, top_p: f32) -> Self {
        self.top_p = Some(top_p);
        self
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum ChatContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ChatContentBlock>),
}

impl MessageContent {
    pub fn is_empty(&self) -> bool {
        match self {
            MessageContent::Blocks(blocks) => blocks.is_empty(),
            MessageContent::Text(text) => text.is_empty(),
        }
    }

    pub fn blocks(&self) -> Vec<ChatContentBlock> {
        match self {
            MessageContent::Blocks(blocks) => blocks.clone(),
            MessageContent::Text(text) => vec![ChatContentBlock::Text { text: text.clone() }],
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatRequestMessage {
    pub role: Role,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// The model used for this message (typically set on assistant responses)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// The reasoning/thinking content from the model (for models that support extended thinking)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatRequestMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: MessageContent::Text(content.into()),
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            model_id: None,
            reasoning_content: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Text(content.into()),
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            model_id: None,
            reasoning_content: None,
        }
    }

    pub fn assistant(
        content: impl Into<String>,
        model_id: impl Into<String>,
        reasoning_content: Option<String>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text(content.into()),
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            model_id: Some(model_id.into()),
            reasoning_content,
        }
    }

    pub fn assistant_tool_call(tool_call: ToolCallRequest) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text("".into()),
            name: None,
            tool_calls: vec![tool_call],
            tool_call_id: None,
            model_id: None,
            reasoning_content: None,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: MessageContent::Text(content.into()),
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            model_id: None,
            reasoning_content: None,
        }
    }

    pub fn is_system_message(&self) -> bool {
        self.role == Role::System
    }

    /// Extract text content from the message content blocks
    pub fn text_content(&self) -> String {
        self.content
            .blocks()
            .iter()
            .filter_map(|block| match block {
                ChatContentBlock::Text { text } => Some(text.clone()),
                ChatContentBlock::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Set text content, replacing all existing content blocks
    pub fn set_text_content(&mut self, text: impl Into<String>) {
        self.content = MessageContent::Text(text.into());
    }

    /// Append text content to existing content
    pub fn append_text_content(&mut self, text: impl Into<String>) {
        if self.content.is_empty() {
            self.set_text_content(text);
            return;
        }

        let new_content = match &self.content {
            MessageContent::Text(prev) => MessageContent::Text(format!("{}{}", prev, text.into())),
            MessageContent::Blocks(blocks) => {
                let mut blocks = blocks.clone();
                blocks.push(ChatContentBlock::Text { text: text.into() });
                MessageContent::Blocks(blocks)
            }
        };

        self.content = new_content;
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Calculate how many chat messages to keep for a given target prompt index (0-based, inclusive).
pub fn chat_truncate_for_prompt(
    chat_history: &[ChatRequestMessage],
    target_prompt_index: usize,
) -> usize {
    let mut user_count = 0;
    let mut keep_count = 0;

    for (i, msg) in chat_history.iter().enumerate() {
        if matches!(msg.role, Role::User) {
            user_count += 1;
            // If we've seen more user messages than target + 1, stop here
            if user_count > target_prompt_index + 1 {
                keep_count = i;
                break;
            }
        }
        keep_count = i + 1;
    }

    keep_count
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ToolType {
    Function,
}

// Re-export ToolDefinition and FunctionTool from echo-agent-tools.
// The canonical definitions now live there; this re-export keeps
// all existing `crate::sampling::types::ToolDefinition` imports working.
pub use echo_agent_tools::types::definition::{FunctionTool, ToolDefinition};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum ToolChoice {
    Preset(String),
    Function {
        #[serde(rename = "type")]
        kind: ToolType,
        function: ToolChoiceFunction,
    },
}

impl ToolChoice {
    pub fn auto() -> Self {
        Self::Preset("auto".to_string())
    }

    pub fn none() -> Self {
        Self::Preset("none".to_string())
    }

    pub fn required() -> Self {
        Self::Preset("required".to_string())
    }

    pub fn function(name: impl Into<String>) -> Self {
        Self::Function {
            kind: ToolType::Function,
            function: ToolChoiceFunction { name: name.into() },
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolChoiceFunction {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCallRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: ToolType,
    pub function: ToolCallFunction,
}

impl ToolCallRequest {
    pub fn function(name: impl Into<String>, arguments: impl Into<String>) -> Self {
        Self {
            id: None,
            kind: ToolType::Function,
            function: ToolCallFunction::new(name, arguments),
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatResponseMessage,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_finish_reason"
    )]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    FunctionCall,
}

/// Some OpenAI-compatible providers emit an empty string in intermediate
/// chunks instead of `null`/omitting `finish_reason`. Treat that as unfinished
/// while preserving strict validation for every non-empty value.
fn deserialize_optional_finish_reason<'de, D>(
    deserializer: D,
) -> Result<Option<FinishReason>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    let Some(value) = value.map(|value| value.trim().to_owned()) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }

    serde_json::from_value(Value::String(value))
        .map(Some)
        .map_err(serde::de::Error::custom)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatResponseMessage {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ToolCallResponse {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub function: ToolCallFunction,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ToolCallFunction {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

impl ToolCallFunction {
    pub fn new(name: impl Into<String>, arguments: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    pub fn from_json(name: impl Into<String>, arguments: &Value) -> Self {
        Self {
            name: name.into(),
            arguments: arguments.to_string(),
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
    /// EchoAgent extension: request price in USD ticks (1 USD = 1e10 ticks).
    /// The REST mapper backfills `0` for unbilled requests; capture sites
    /// normalize `0` to "unreported" (see `stream/chat_completions.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_in_usd_ticks: Option<i64>,
}

impl<'de> Deserialize<'de> for Usage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Some OpenAI-compatible gateways emit both naming conventions in the
        // same usage object (for example `prompt_tokens` and `input_tokens`).
        // A serde alias treats that as a duplicate field and rejects the whole
        // streamed response. Deserialize through Value so duplicate literal
        // keys use serde_json's normal last-value behavior, while canonical
        // OpenAI names take precedence over Responses-style aliases.
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("usage must be a JSON object"))?;

        fn token_field<E: serde::de::Error>(
            object: &serde_json::Map<String, Value>,
            canonical: &str,
            alias: Option<&str>,
        ) -> Result<u32, E> {
            let value = object
                .get(canonical)
                .or_else(|| alias.and_then(|name| object.get(name)));
            match value {
                Some(Value::Null) | None => Ok(0),
                Some(value) => serde_json::from_value(value.clone()).map_err(E::custom),
            }
        }

        fn optional_field<T, E>(
            object: &serde_json::Map<String, Value>,
            name: &str,
        ) -> Result<Option<T>, E>
        where
            T: serde::de::DeserializeOwned,
            E: serde::de::Error,
        {
            match object.get(name) {
                Some(Value::Null) | None => Ok(None),
                Some(value) => serde_json::from_value(value.clone())
                    .map(Some)
                    .map_err(E::custom),
            }
        }

        Ok(Self {
            prompt_tokens: token_field(object, "prompt_tokens", Some("input_tokens"))?,
            completion_tokens: token_field(object, "completion_tokens", Some("output_tokens"))?,
            total_tokens: token_field(object, "total_tokens", None)?,
            prompt_tokens_details: optional_field(object, "prompt_tokens_details")?,
            completion_tokens_details: optional_field(object, "completion_tokens_details")?,
            cost_in_usd_ticks: optional_field(object, "cost_in_usd_ticks")?,
        })
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: u32,
    #[serde(default)]
    pub audio_tokens: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: u32,
    #[serde(default)]
    pub audio_tokens: u32,
    #[serde(default)]
    pub accepted_prediction_tokens: u32,
    #[serde(default)]
    pub rejected_prediction_tokens: u32,
}
// ============ Streaming types ============

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChunkChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_helpers::empty_string_as_none"
    )]
    pub system_fingerprint: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatChunkChoice {
    pub index: u32,
    pub delta: ChatChunkDelta,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_finish_reason"
    )]
    pub finish_reason: Option<FinishReason>,
}

/// Streaming delta for a tool call.
///
/// In OpenAI-compatible streaming, tool calls arrive across multiple chunks:
/// - The first chunk carries `id`, `type`, `index`, and the `function.name` + start of `arguments`.
/// - Subsequent chunks only carry `index` and a `function.arguments` fragment (no `id`, no `name`).
///
/// Providers are expected to send `index`, but several OpenAI-compatible
/// implementations omit it or incorrectly reuse zero for parallel calls. Keep
/// it optional so the stream assembler can recover by call ID or fail safely
/// instead of merging two calls.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ToolCallDelta {
    /// The positional index of the tool call being streamed.
    /// Used to correlate delta chunks belonging to the same tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// Only present in the first chunk for this tool call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Only present in the first chunk (usually "function").
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The function name and/or argument fragment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<ToolCallFunctionDelta>,
}

/// Streaming delta for function name/arguments within a tool call.
///
/// `name` is only present in the first chunk; `arguments` may arrive across many chunks.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ToolCallFunctionDelta {
    /// Only present in the first chunk for this tool call.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_helpers::empty_string_as_none"
    )]
    pub name: Option<String>,
    /// Argument fragment (may be empty or partial JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ChatChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    /// Tool call deltas. Handles `null` in JSON as empty vec.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_null_default"
    )]
    pub tool_calls: Vec<ToolCallDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Parameters to control realtime data.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SearchParameters {
    /// Choose the mode to query realtime data:
    /// * `off`: no search performed and no external will be considered.
    /// * `on` (default): the model will search in every source for relevant data.
    /// * `auto`: the model chooses whether to search data or not and where to search the data.
    pub mode: Option<String>,
    /// List of sources to search in. If no sources are specified, the model will look over the web and X by default.
    pub sources: Option<Vec<SearchSource>>,
    /// Date from which to consider the results in ISO-8601 YYYY-MM-DD.
    pub from_date: Option<String>,
    /// Date up to which to consider the results in ISO-8601 YYYY-MM-DD.
    pub to_date: Option<String>,
    /// Whether to return citations in the response or not.
    pub return_citations: Option<bool>,
    /// Maximum number of search results to use.
    pub max_search_results: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type")]
pub enum SearchSource {
    #[serde(rename = "x")]
    X {
        /// X Handles of the users from whom to consider the posts.
        included_x_handles: Option<Vec<String>>,
        /// DEPRECATED in favor of `included_x_handles`. Use `included_x_handles` instead.
        x_handles: Option<Vec<String>>,
        /// List of X handles to exclude from the search results.
        excluded_x_handles: Option<Vec<String>>,
        /// The minimum favorite count of the X posts to consider.
        post_favorite_count: Option<i32>,
        /// The minimum view count of the X posts to consider.
        post_view_count: Option<i32>,
    },
    #[serde(rename = "web")]
    Web {
        /// List of website to exclude from the search results.
        excluded_websites: Option<Vec<String>>,
        /// List of website to allow in the search results.
        allowed_websites: Option<Vec<String>>,
        /// ISO alpha-2 code of the country.
        country: Option<String>,
        /// If set to true, mature content won't be considered during the search.
        safe_search: Option<bool>,
    },
    #[serde(rename = "news")]
    News {
        /// List of website to exclude from the search results.
        excluded_websites: Option<Vec<String>>,
        /// ISO alpha-2 code of the country.
        country: Option<String>,
        /// If set to true, mature content won't be considered during the search.
        safe_search: Option<bool>,
    },
    #[serde(rename = "rss")]
    Rss {
        /// Links of the RSS feeds.
        links: Vec<String>,
    },
}

/// Per-model config for the `x-compaction-at` request header (a token count).
///
/// Deserialized from a polymorphic remote-config value: `true` enables the
/// header with a value computed as `context_window * auto_compact_threshold_percent / 100`;
/// `false` (or absent) disables it; an integer `N` sends the constant `N`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum CompactionAtTokens {
    Enabled(bool),
    Fixed(u64),
}

impl CompactionAtTokens {
    /// Resolve the absolute token count to send, or `None` when disabled.
    pub fn resolve(self, context_window: u64, threshold_percent: u8) -> Option<u64> {
        match self {
            CompactionAtTokens::Enabled(false) => None,
            CompactionAtTokens::Enabled(true) => {
                Some(context_window * u64::from(threshold_percent) / 100)
            }
            CompactionAtTokens::Fixed(n) => Some(n),
        }
    }
}

/// Per-model config for the `x-compactions-remaining` request header.
///
/// `true` sends the dynamic value (1 on the uncompacted prefix, 0 once the session compacts);
/// `false`/absent disables the header; an integer `N` sends the constant `N`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum CompactionsRemaining {
    Dynamic(bool),
    Fixed(u8),
}

impl CompactionsRemaining {
    /// Resolve the header value to send, or `None` when disabled.
    pub fn resolve(self, has_compaction_summary: bool) -> Option<u8> {
        match self {
            CompactionsRemaining::Dynamic(false) => None,
            CompactionsRemaining::Dynamic(true) => Some(u8::from(!has_compaction_summary)),
            CompactionsRemaining::Fixed(n) => Some(n),
        }
    }
}

/// Reasoning effort level. `None`/`Minimal` are omitted on the Anthropic Messages API.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    Xhigh,
    Max,
}

impl ReasoningEffort {
    pub fn to_responses_api(self) -> crate::rs::ReasoningEffort {
        match self {
            Self::None => crate::rs::ReasoningEffort::None,
            Self::Minimal => crate::rs::ReasoningEffort::Minimal,
            Self::Low => crate::rs::ReasoningEffort::Low,
            Self::Medium => crate::rs::ReasoningEffort::Medium,
            Self::High => crate::rs::ReasoningEffort::High,
            Self::Xhigh => crate::rs::ReasoningEffort::Xhigh,
            Self::Max => crate::rs::ReasoningEffort::Max,
        }
    }

    /// Inverse of [`to_responses_api`](Self::to_responses_api): the effort the
    /// Responses API echoes back on `response.reasoning.effort`.
    pub fn from_responses_api(effort: crate::rs::ReasoningEffort) -> Self {
        match effort {
            crate::rs::ReasoningEffort::None => Self::None,
            crate::rs::ReasoningEffort::Minimal => Self::Minimal,
            crate::rs::ReasoningEffort::Low => Self::Low,
            crate::rs::ReasoningEffort::Medium => Self::Medium,
            crate::rs::ReasoningEffort::High => Self::High,
            crate::rs::ReasoningEffort::Xhigh => Self::Xhigh,
            crate::rs::ReasoningEffort::Max => Self::Max,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn to_messages_api(self) -> Option<&'static str> {
        match self {
            Self::None | Self::Minimal => None,
            _ => Some(self.as_str()),
        }
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ReasoningEffort {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "none" => Ok(Self::None),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::Xhigh),
            "max" => Ok(Self::Max),
            _ => Err(format!(
                "invalid reasoning effort: {s:?} (expected one of: none, minimal, low, medium, high, xhigh, max)"
            )),
        }
    }
}

pub fn parse_canonical_effort_token(token: &str) -> Option<ReasoningEffort> {
    token.parse().ok()
}

pub const REASONING_EFFORT_META_KEY: &str = "reasoningEffort";
pub const SUPPORTS_REASONING_EFFORT_META_KEY: &str = "supportsReasoningEffort";

pub fn supports_reasoning_effort_meta(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    meta.and_then(|m| m.get(SUPPORTS_REASONING_EFFORT_META_KEY))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Returns `None` on type-mismatch or unknown variant (logs a warn so we don't
/// overwrite the user's persisted pref on the next save).
pub fn parse_reasoning_effort_meta(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<ReasoningEffort> {
    let raw = meta?.get(REASONING_EFFORT_META_KEY)?;
    let s = match raw.as_str() {
        Some(s) => s,
        None => {
            tracing::warn!(value = %raw, "meta.reasoningEffort: expected string, ignoring");
            return None;
        }
    };
    match s.parse() {
        Ok(eff) => Some(eff),
        Err(err) => {
            tracing::warn!(value = %s, error = %err, "meta.reasoningEffort: parse failed, ignoring");
            None
        }
    }
}

pub fn reasoning_effort_meta_value(effort: ReasoningEffort) -> serde_json::Value {
    serde_json::Value::String(effort.as_str().to_string())
}

pub const REASONING_EFFORTS_META_KEY: &str = "reasoningEfforts";

/// A single selectable reasoning-effort option for a model. `id`/`label` are
/// presentation and input; `value` is the canonical value sent on the wire.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ReasoningEffortOption {
    pub id: String,
    pub value: ReasoningEffort,
    pub label: String,
    pub description: Option<String>,
    pub default: bool,
}

/// Deserialization shape accepting either a bare canonical value string
/// (`"xhigh"`) or a table with `value` required and everything else optional.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum RawReasoningEffortOption {
    Bare(String),
    Full {
        value: ReasoningEffort,
        id: Option<String>,
        label: Option<String>,
        description: Option<String>,
        #[serde(default)]
        default: bool,
    },
}

/// Uppercase the first character of an id for a default label; `"xhigh"` becomes
/// `"Xhigh"`, `"deep"` becomes `"Deep"`.
fn humanize_effort_id(id: &str) -> String {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

impl<'de> serde::Deserialize<'de> for ReasoningEffortOption {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match RawReasoningEffortOption::deserialize(deserializer)? {
            RawReasoningEffortOption::Bare(s) => {
                let value = s
                    .parse::<ReasoningEffort>()
                    .map_err(serde::de::Error::custom)?;
                let id = value.as_str().to_string();
                let label = humanize_effort_id(&id);
                ReasoningEffortOption {
                    id,
                    value,
                    label,
                    description: None,
                    default: false,
                }
            }
            RawReasoningEffortOption::Full {
                value,
                id,
                label,
                description,
                default,
            } => {
                let id = id.unwrap_or_else(|| value.as_str().to_string());
                let label = label.unwrap_or_else(|| humanize_effort_id(&id));
                ReasoningEffortOption {
                    id,
                    value,
                    label,
                    description,
                    default,
                }
            }
        })
    }
}

/// Parse a JSON array of reasoning-effort options element-by-element, skipping
/// (and warning on) any entry whose `value` fails to parse (forward-compat for
/// tiers a newer server introduces). The single home for the skip-invalid rule,
/// shared by the meta reader and the remote `/models` parser.
pub fn parse_reasoning_effort_options(arr: &[serde_json::Value]) -> Vec<ReasoningEffortOption> {
    arr.iter()
        .filter_map(
            |el| match serde_json::from_value::<ReasoningEffortOption>(el.clone()) {
                Ok(opt) => Some(opt),
                Err(err) => {
                    tracing::warn!(value = %el, error = %err, "reasoningEfforts: skipping invalid entry");
                    None
                }
            },
        )
        .collect()
}

/// Parse the per-model reasoning-effort menu from a model's ACP `meta`. Returns
/// `None` when the key is absent, is not an array, or yields no usable options
/// after skip-invalid — so "absent" and "present-but-unusable" collapse to the
/// same fallback path in every consumer.
pub fn parse_reasoning_efforts_meta(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<Vec<ReasoningEffortOption>> {
    let raw = meta?.get(REASONING_EFFORTS_META_KEY)?;
    let arr = match raw.as_array() {
        Some(arr) => arr,
        None => {
            tracing::warn!(value = %raw, "meta.reasoningEfforts: expected array, ignoring");
            return None;
        }
    };
    let options = parse_reasoning_effort_options(arr);
    (!options.is_empty()).then_some(options)
}

pub fn reasoning_efforts_meta_value(opts: &[ReasoningEffortOption]) -> serde_json::Value {
    serde_json::to_value(opts).unwrap_or_else(|_| serde_json::Value::Array(Vec::new()))
}

/// Which API backend to use for model inference.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiBackend {
    /// Use the Chat Completions API (/v1/chat/completions)
    #[default]
    ChatCompletions,
    /// Use the Responses API (/v1/responses)
    Responses,
    /// Use the Anthropic Messages API (/v1/messages)
    Messages,
}

impl ApiBackend {
    /// Whether the backend enforces a response JSON schema natively alongside
    /// tool calls. The Messages API does not (a schema there blocks tool use),
    /// so structured output there goes through the StructuredOutput tool.
    pub fn supports_native_schema(&self) -> bool {
        matches!(self, Self::ChatCompletions | Self::Responses)
    }

    /// Whether [`ConversationRequest::prompt_cache_key`] reaches the wire. Only the Responses mapping sends it, so a key set elsewhere is inert.
    ///
    /// [`ConversationRequest::prompt_cache_key`]: crate::conversation::ConversationRequest::prompt_cache_key
    pub fn forwards_prompt_cache_key(&self) -> bool {
        matches!(self, Self::Responses)
    }
}

/// Sampling client configuration (API key excluded — that stays in the client).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SamplingConfig {
    pub base_url: String,
    pub model: String,
    pub max_completion_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    /// Which API backend to use for this model
    #[serde(default)]
    pub api_backend: ApiBackend,
    /// Extra headers to send with requests (e.g., for BYOK scenarios).
    #[serde(default, skip_serializing_if = "indexmap::IndexMap::is_empty")]
    pub extra_headers: indexmap::IndexMap<String, String>,
    /// Query parameters folded into every request URL (percent-encoded).
    #[serde(default, skip_serializing_if = "indexmap::IndexMap::is_empty")]
    pub query_params: indexmap::IndexMap<String, String>,
    /// Header name to environment variable; only the mapping persists, not the
    /// resolved secret.
    #[serde(default, skip_serializing_if = "indexmap::IndexMap::is_empty")]
    pub env_http_headers: indexmap::IndexMap<String, String>,
    /// Total context window size in tokens. Used for auto-compact thresholds.
    pub context_window: NonZeroU64,
    /// Reasoning effort level for reasoning models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// When true, inject `stream_tool_calls: true` into the Responses
    /// API request body so the upstream emits per-chunk argument deltas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_calls: Option<bool>,
}

// ============ Responses API wrapper ============

/// Wrapper around `async_openai::types::responses::CreateResponse` that adds
/// custom header fields for EchoAgent request tracking, similar to
/// `ChatCompletionRequest`.
#[derive(Debug, Clone, Default)]
pub struct CreateResponseWrapper {
    /// The inner Responses API request.
    pub inner: crate::rs::CreateResponse,

    /// Custom header: conversation ID for tracking.
    pub x_echo_agent_conv_id: Option<String>,

    /// Custom header: request ID for tracking.
    pub x_echo_agent_req_id: Option<String>,

    pub x_echo_agent_session_id: Option<String>,
    pub x_echo_agent_turn_idx: Option<String>,
    pub x_echo_agent_agent_id: Option<String>,
    pub x_echo_agent_deployment_id: Option<String>,
    pub x_echo_agent_user_id: Option<String>,

    /// Optional tracing context (e.g., where to persist the finalized request payload).
    pub trace: Option<Box<dyn TraceContext>>,

    /// EchoAgent-specific tool definitions that can't be expressed via
    /// `async_openai`'s `rs::Tool` enum (e.g., `x_search`). Injected
    /// as raw JSON into the serialized request body's `tools` array.
    pub extra_tool_entries: Vec<serde_json::Value>,
}

impl CreateResponseWrapper {
    /// Create a new wrapper from an existing `CreateResponse`.
    pub fn new(inner: crate::rs::CreateResponse) -> Self {
        Self {
            inner,
            x_echo_agent_conv_id: None,
            x_echo_agent_req_id: None,
            x_echo_agent_session_id: None,
            x_echo_agent_turn_idx: None,
            x_echo_agent_agent_id: None,
            x_echo_agent_deployment_id: None,
            x_echo_agent_user_id: None,
            trace: None,
            extra_tool_entries: vec![],
        }
    }

    /// Set the conversation ID header.
    pub fn with_conv_id(mut self, conv_id: impl Into<String>) -> Self {
        self.x_echo_agent_conv_id = Some(conv_id.into());
        self
    }

    /// Set the request ID header.
    pub fn with_req_id(mut self, req_id: impl Into<String>) -> Self {
        self.x_echo_agent_req_id = Some(req_id.into());
        self
    }

    /// Set the trace context for request logging.
    pub fn with_trace(mut self, trace: impl TraceContext + 'static) -> Self {
        self.trace = Some(Box::new(trace));
        self
    }
}

impl From<crate::rs::CreateResponse> for CreateResponseWrapper {
    fn from(inner: crate::rs::CreateResponse) -> Self {
        Self::new(inner)
    }
}

// ============ Messages API wrapper ============

/// Wrapper around `MessagesRequest` that adds custom header fields for EchoAgent
/// request tracking, analogous to `CreateResponseWrapper`.
#[derive(Debug, Clone, Default)]
pub struct MessagesRequestWrapper {
    /// The inner Messages API request.
    pub inner: crate::messages::MessagesRequest,

    /// Custom header: conversation ID for tracking.
    pub x_echo_agent_conv_id: Option<String>,

    /// Custom header: request ID for tracking.
    pub x_echo_agent_req_id: Option<String>,

    pub x_echo_agent_session_id: Option<String>,
    pub x_echo_agent_turn_idx: Option<String>,
    pub x_echo_agent_agent_id: Option<String>,
    pub x_echo_agent_deployment_id: Option<String>,
    pub x_echo_agent_user_id: Option<String>,

    /// Optional tracing context (e.g., where to persist the finalized request payload).
    pub trace: Option<Box<dyn TraceContext>>,
}

impl MessagesRequestWrapper {
    /// Create a new wrapper from an existing `MessagesRequest`.
    pub fn new(inner: crate::messages::MessagesRequest) -> Self {
        Self {
            inner,
            x_echo_agent_conv_id: None,
            x_echo_agent_req_id: None,
            x_echo_agent_session_id: None,
            x_echo_agent_turn_idx: None,
            x_echo_agent_agent_id: None,
            x_echo_agent_deployment_id: None,
            x_echo_agent_user_id: None,
            trace: None,
        }
    }

    /// Set the conversation ID header.
    pub fn with_conv_id(mut self, conv_id: impl Into<String>) -> Self {
        self.x_echo_agent_conv_id = Some(conv_id.into());
        self
    }

    /// Set the request ID header.
    pub fn with_req_id(mut self, req_id: impl Into<String>) -> Self {
        self.x_echo_agent_req_id = Some(req_id.into());
        self
    }

    /// Set the trace context for request logging.
    pub fn with_trace(mut self, trace: impl TraceContext + 'static) -> Self {
        self.trace = Some(Box::new(trace));
        self
    }
}

impl From<crate::messages::MessagesRequest> for MessagesRequestWrapper {
    fn from(inner: crate::messages::MessagesRequest) -> Self {
        Self::new(inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reasoning_effort_serde_lowercase_round_trip() {
        for v in [
            ReasoningEffort::None,
            ReasoningEffort::Minimal,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Xhigh,
            ReasoningEffort::Max,
        ] {
            let json = serde_json::to_string(&v).unwrap();
            assert_eq!(json, format!("\"{}\"", v.as_str()), "serialize {v:?}");
            let back: ReasoningEffort = serde_json::from_str(&json).unwrap();
            assert_eq!(back, v, "round-trip {v:?}");
        }
        assert!(serde_json::from_str::<ReasoningEffort>("\"BOGUS\"").is_err());
    }

    #[test]
    fn reasoning_effort_from_str_parses_max_and_xhigh_as_distinct_tiers() {
        assert_eq!(
            "max".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::Max
        );
        assert_eq!(
            "xhigh".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::Xhigh
        );
    }

    #[test]
    fn parse_canonical_effort_token_helper() {
        assert_eq!(
            parse_canonical_effort_token("max"),
            Some(ReasoningEffort::Max)
        );
        assert_eq!(
            parse_canonical_effort_token("high"),
            Some(ReasoningEffort::High)
        );
        assert!(parse_canonical_effort_token("deep").is_none());
        assert!(parse_canonical_effort_token("bogus").is_none());
    }

    #[test]
    fn reasoning_effort_option_deserializes_bare_string() {
        let opt: ReasoningEffortOption = serde_json::from_value(json!("xhigh")).unwrap();
        assert_eq!(
            opt,
            ReasoningEffortOption {
                id: "xhigh".to_string(),
                value: ReasoningEffort::Xhigh,
                label: "Xhigh".to_string(),
                description: None,
                default: false,
            }
        );
    }

    #[test]
    fn reasoning_effort_option_table_defaults_id_and_label_from_value() {
        let opt: ReasoningEffortOption =
            serde_json::from_value(json!({ "value": "high" })).unwrap();
        assert_eq!(opt.id, "high");
        assert_eq!(opt.label, "High");
        assert_eq!(opt.value, ReasoningEffort::High);
        assert!(!opt.default);
    }

    #[test]
    fn reasoning_effort_option_table_honors_explicit_fields() {
        let opt: ReasoningEffortOption = serde_json::from_value(json!({
            "id": "deep",
            "value": "xhigh",
            "label": "Deep",
            "description": "Maximum reasoning",
            "default": true,
        }))
        .unwrap();
        assert_eq!(opt.id, "deep");
        assert_eq!(opt.value, ReasoningEffort::Xhigh);
        assert_eq!(opt.label, "Deep");
        assert_eq!(opt.description.as_deref(), Some("Maximum reasoning"));
        assert!(opt.default);
    }

    #[test]
    fn parse_reasoning_efforts_meta_absent_is_none() {
        assert!(parse_reasoning_efforts_meta(None).is_none());
        assert!(
            parse_reasoning_efforts_meta(Some(
                json!({ "agentType": "echoagent" }).as_object().unwrap()
            ))
            .is_none()
        );
    }

    #[test]
    fn parse_reasoning_efforts_meta_skips_invalid_value() {
        let meta = json!({
            REASONING_EFFORTS_META_KEY: [
                { "value": "high" },
                { "value": "quantum" },
                "low",
            ]
        })
        .as_object()
        .cloned()
        .unwrap();
        let parsed = parse_reasoning_efforts_meta(Some(&meta)).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].value, ReasoningEffort::High);
        assert_eq!(parsed[1].value, ReasoningEffort::Low);
    }

    #[test]
    fn parse_reasoning_efforts_meta_present_but_unusable_is_none() {
        // Explicit empty, non-array, and all-entries-skip-invalidated all collapse
        // to `None` so consumers fall back exactly as they do for an absent key.
        for meta in [
            json!({ REASONING_EFFORTS_META_KEY: [] }),
            json!({ REASONING_EFFORTS_META_KEY: "nope" }),
            json!({ REASONING_EFFORTS_META_KEY: [{ "value": "quantum" }] }),
        ] {
            let meta = meta.as_object().cloned().unwrap();
            assert!(
                parse_reasoning_efforts_meta(Some(&meta)).is_none(),
                "expected None for {meta:?}"
            );
        }
    }

    #[test]
    fn reasoning_efforts_meta_value_round_trips() {
        let opts = vec![
            ReasoningEffortOption {
                id: "deep".to_string(),
                value: ReasoningEffort::Xhigh,
                label: "Deep".to_string(),
                description: Some("Maximum reasoning".to_string()),
                default: true,
            },
            ReasoningEffortOption {
                id: "balanced".to_string(),
                value: ReasoningEffort::Medium,
                label: "Balanced".to_string(),
                description: None,
                default: false,
            },
        ];
        let meta = json!({ REASONING_EFFORTS_META_KEY: reasoning_efforts_meta_value(&opts) })
            .as_object()
            .cloned()
            .unwrap();
        assert_eq!(parse_reasoning_efforts_meta(Some(&meta)).unwrap(), opts);
    }

    #[test]
    fn compactions_remaining_resolve_covers_all_variants() {
        assert_eq!(CompactionsRemaining::Dynamic(false).resolve(false), None);
        assert_eq!(CompactionsRemaining::Dynamic(false).resolve(true), None);
        assert_eq!(CompactionsRemaining::Dynamic(true).resolve(false), Some(1));
        assert_eq!(CompactionsRemaining::Dynamic(true).resolve(true), Some(0));
        assert_eq!(CompactionsRemaining::Fixed(1).resolve(false), Some(1));
        assert_eq!(CompactionsRemaining::Fixed(1).resolve(true), Some(1));
    }

    #[test]
    fn compactions_remaining_untagged_serde_prefers_dynamic_for_bools() {
        assert_eq!(
            serde_json::from_str::<CompactionsRemaining>("true").unwrap(),
            CompactionsRemaining::Dynamic(true)
        );
        assert_eq!(
            serde_json::from_str::<CompactionsRemaining>("false").unwrap(),
            CompactionsRemaining::Dynamic(false)
        );
        assert_eq!(
            serde_json::from_str::<CompactionsRemaining>("1").unwrap(),
            CompactionsRemaining::Fixed(1)
        );
    }

    #[test]
    fn parse_reasoning_effort_meta_handles_all_inputs() {
        let as_map = |v: serde_json::Value| v.as_object().cloned().unwrap();
        assert_eq!(parse_reasoning_effort_meta(None), None);
        let empty = as_map(serde_json::json!({}));
        assert_eq!(parse_reasoning_effort_meta(Some(&empty)), None);
        let ok = as_map(serde_json::json!({"reasoningEffort": "xhigh"}));
        assert_eq!(
            parse_reasoning_effort_meta(Some(&ok)),
            Some(ReasoningEffort::Xhigh)
        );
        let bad_type = as_map(serde_json::json!({"reasoningEffort": 3}));
        assert_eq!(parse_reasoning_effort_meta(Some(&bad_type)), None);
        let unknown = as_map(serde_json::json!({"reasoningEffort": "ULTRA"}));
        assert_eq!(parse_reasoning_effort_meta(Some(&unknown)), None);
    }

    #[test]
    fn test_chat_text_content_serialization() {
        let test = vec![ChatContentBlock::Text {
            text: "Hello World!".to_string(),
        }];

        let json = serde_json::to_string(&test).unwrap();
        assert_eq!(json, r#"[{"type":"text","text":"Hello World!"}]"#);
    }

    #[test]
    fn test_chat_all_content_serialization() {
        let test = vec![
            ChatContentBlock::ImageUrl {
                image_url: ImageUrl {
                    url: "https://www.test.com".to_string(),
                },
            },
            ChatContentBlock::Text {
                text: "Hello".to_string(),
            },
        ];

        let json = serde_json::to_string(&test).unwrap();
        assert_eq!(
            json,
            r#"[{"type":"image_url","image_url":{"url":"https://www.test.com"}},{"type":"text","text":"Hello"}]"#
        );
    }

    #[test]
    fn test_content_string_deserialization() {
        let expected_contents = vec!["", "Hello world!"];

        for expected_content in expected_contents {
            let json = format!(r#"{{"content":"{}","role":"assistant"}}"#, expected_content);

            let msg: ChatRequestMessage = serde_json::from_str(&json)
                .unwrap_or_else(|_| panic!("Should deserialize {}", expected_content));

            let blocks = msg.content.blocks();
            assert_eq!(blocks.len(), 1);
            match &blocks[0] {
                ChatContentBlock::Text { text } => assert_eq!(text, expected_content),
                _ => panic!("Expected empty Text block"),
            }
        }
    }

    #[test]
    fn test_chat_chunk_delta_deserialize_with_null_tool_calls() {
        let delta_json = r#"{
            "reasoning": null,
            "reasoning_details": [],
            "content": "",
            "function_call": null,
            "refusal": null,
            "role": "assistant",
            "tool_calls": null
        }"#;

        let result = serde_json::from_str::<ChatChunkDelta>(delta_json);
        assert!(result.is_ok(), "Failed to deserialize: {:?}", result.err());

        let delta = result.unwrap();
        assert_eq!(delta.role, Some(Role::Assistant));
        assert_eq!(delta.content, Some("".to_string()));
        assert!(delta.tool_calls.is_empty());
    }

    #[test]
    fn usage_accepts_partial_and_responses_style_token_fields() {
        let partial: Usage = serde_json::from_str(r#"{"total_tokens":42}"#).unwrap();
        assert_eq!(partial.prompt_tokens, 0);
        assert_eq!(partial.completion_tokens, 0);
        assert_eq!(partial.total_tokens, 42);

        let aliased: Usage =
            serde_json::from_str(r#"{"input_tokens":30,"output_tokens":12,"total_tokens":42}"#)
                .unwrap();
        assert_eq!(aliased.prompt_tokens, 30);
        assert_eq!(aliased.completion_tokens, 12);
        assert_eq!(aliased.total_tokens, 42);
    }

    #[test]
    fn usage_accepts_gateways_that_emit_both_token_naming_conventions() {
        let usage: Usage = serde_json::from_str(
            r#"{
                "prompt_tokens":30,
                "input_tokens":999,
                "completion_tokens":12,
                "output_tokens":888,
                "total_tokens":42
            }"#,
        )
        .unwrap();

        assert_eq!(usage.prompt_tokens, 30);
        assert_eq!(usage.completion_tokens, 12);
        assert_eq!(usage.total_tokens, 42);

        let duplicate_literal: Usage = serde_json::from_str(
            r#"{"prompt_tokens":10,"prompt_tokens":11,"completion_tokens":2}"#,
        )
        .unwrap();
        assert_eq!(duplicate_literal.prompt_tokens, 11);
        assert_eq!(duplicate_literal.completion_tokens, 2);
    }

    #[test]
    fn tool_call_response_accepts_missing_provider_identifiers() {
        let call: ToolCallResponse = serde_json::from_str(
            r#"{"type":"function","function":{"arguments":"{\"query\":\"news\"}"}}"#,
        )
        .unwrap();
        assert!(call.id.is_empty());
        assert!(call.function.name.is_empty());
        assert_eq!(call.function.arguments, r#"{"query":"news"}"#);
    }

    #[test]
    fn streaming_tool_call_treats_empty_function_name_as_omitted() {
        let delta: ToolCallFunctionDelta =
            serde_json::from_str(r#"{"name":"","arguments":"{\"target_file\":\"README.md\"}"}"#)
                .unwrap();

        assert_eq!(delta.name, None);
        assert_eq!(
            delta.arguments.as_deref(),
            Some(r#"{"target_file":"README.md"}"#)
        );
    }

    #[test]
    fn chat_choices_treat_empty_finish_reason_as_unfinished() {
        let empty: ChatChunkChoice =
            serde_json::from_str(r#"{"index":0,"delta":{},"finish_reason":""}"#).unwrap();
        assert_eq!(empty.finish_reason, None);

        let whitespace: ChatChunkChoice =
            serde_json::from_str(r#"{"index":0,"delta":{},"finish_reason":"  "}"#).unwrap();
        assert_eq!(whitespace.finish_reason, None);

        let completed: ChatChunkChoice =
            serde_json::from_str(r#"{"index":0,"delta":{},"finish_reason":"tool_calls"}"#).unwrap();
        assert_eq!(completed.finish_reason, Some(FinishReason::ToolCalls));

        let invalid = serde_json::from_str::<ChatChunkChoice>(
            r#"{"index":0,"delta":{},"finish_reason":"unexpected"}"#,
        );
        assert!(invalid.is_err());

        let non_streaming: ChatChoice = serde_json::from_str(
            r#"{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":""}"#,
        )
        .unwrap();
        assert_eq!(non_streaming.finish_reason, None);
    }

    /// Regression test: cloning `Box<dyn TraceContext>` must not infinitely recurse.
    ///
    /// The blanket `impl<T: Clone + ...> TraceContext for T` applies to
    /// `Box<dyn TraceContext>` itself. Without the explicit dereference in
    /// `Clone for Box<dyn TraceContext>`, `self.clone_box()` resolves to the
    /// blanket impl's method (via auto-deref) instead of dispatching through
    /// the vtable, causing `clone()` → `clone_box()` → `clone()` → stack overflow.
    #[test]
    fn clone_box_dyn_trace_context_does_not_recurse() {
        #[derive(Debug, Clone)]
        struct TestTrace(String);

        let trace: Box<dyn TraceContext> = Box::new(TestTrace("hello".into()));
        let cloned = trace.clone();

        // Verify the clone produced a valid TraceContext with the same data.
        // Note: `as_any()` must be called through `&dyn TraceContext` (not on the Box
        // directly) to use vtable dispatch rather than the blanket impl.
        let inner: &dyn TraceContext = &*trace;
        let original = inner.as_any().downcast_ref::<TestTrace>().unwrap();

        let cloned_inner_ref: &dyn TraceContext = &*cloned;
        let cloned_inner = cloned_inner_ref
            .as_any()
            .downcast_ref::<TestTrace>()
            .unwrap();
        assert_eq!(original.0, cloned_inner.0);
    }

    /// Verify that cloning a `ChatCompletionRequest` with a trace does not recurse.
    #[test]
    fn clone_chat_completion_request_with_trace() {
        #[derive(Debug, Clone)]
        struct TestTrace(String);

        let mut request = ChatCompletionRequest::new("test-model", vec![]);
        request.trace = Some(Box::new(TestTrace("trace-data".into())));

        let cloned = request.clone();
        assert!(cloned.trace.is_some());

        let cloned_trace = cloned.trace.unwrap();
        let inner: &dyn TraceContext = &*cloned_trace;
        let downcast = inner.as_any().downcast_ref::<TestTrace>().unwrap();
        assert_eq!(downcast.0, "trace-data");
    }
}
