use crate::{
    computer::types::{AsyncFileSystem, TerminalBackend},
    implementations::{
        codex, echo_agent_build, echo_agent_build_concise, echo_agent_build_hashline, opencode,
        skills::types::SkillInfo,
    },
    notification::ToolNotificationHandle,
    persistence::ResourcesPersistence,
    reminders::SkillDiscoveryReminder,
    types::{
        ToolInput,
        definition::ToolDefinition,
        output::{ToolOutput, ToolRunResult},
        params_validation::{ParamValidationError, validate_params_json},
        requirements::{EvalContext, Expr, ProposedTool, ToolRequirement},
        resources::{InnerDispatch, Resources, SharedResources},
        template_renderer::TemplateRenderer,
        tool::{Reminder, ToolKind, ToolNamespace},
        tool_metadata::ToolMetadata,
    },
    util::remap::remap_json_keys,
};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
/// Process-global registry of external "tool packs" — functions that
/// contribute additional tool registrations into every
/// [`ToolRegistryBuilder::new`].
///
/// This inverts the dependency for harness code that must live outside
/// this crate: instead of `echo-agent-tools` referencing an out-of-tree tool
/// pack, the pack calls
/// [`register_tool_pack`] at startup and registers itself here.
///
/// # Ordering contract
/// [`register_tool_pack`] MUST run before the FIRST `ToolRegistryBuilder::new()`
/// in the process. Packs registered after a builder
/// has been constructed do not retroactively apply to that builder.
/// A tool pack: a function that contributes registrations to a builder.
pub type ToolPack = fn(&mut ToolRegistryBuilder);
static TOOL_PACKS: OnceLock<Mutex<Vec<ToolPack>>> = OnceLock::new();
fn tool_packs() -> &'static Mutex<Vec<ToolPack>> {
    TOOL_PACKS.get_or_init(|| Mutex::new(Vec::new()))
}
/// Register an out-of-tree tool pack. See [`TOOL_PACKS`] for the ordering
/// contract. Idempotency is the caller's responsibility (a pack registered
/// twice registers its tools twice).
pub fn register_tool_pack(pack: ToolPack) {
    tool_packs().lock().push(pack);
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolConfig {
    pub id: String,
    /// tool params, keyed by fully qualified name.
    pub params: Option<serde_json::Map<String, serde_json::Value>>,
    /// tool_id → client-facing name.
    pub name_override: Option<String>,
    /// { canonical param → client-facing param }.
    pub params_name_overrides: Option<HashMap<String, String>>,
    /// When `Some`, replaces the tool's `description_template()` entirely.
    ///
    /// Use this when the same built-in tool needs a context-specific
    /// description — e.g. `run_terminal_cmd` in a container environment
    /// vs. the default host-shell description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_override: Option<String>,
    /// Per-tool behavior version override. Wins over `ToolServerConfig::behavior_preset`.
    /// Only valid for version-managed tools (see `versions::MANAGED_TOOLS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_version: Option<String>,
    /// The tool's capability category. Populated automatically by
    /// `for_tool::<T>()` / `From<&T: Tool>` and used by capability-mode
    /// enforcement to filter tools without a hardcoded ID mapping.
    ///
    /// `None` means the tool's kind is unknown (e.g. MCP/custom tools
    /// created via `ToolConfig::from_id()`). Capability-mode filtering
    /// preserves tools with `kind: None` — this is intentional to avoid
    /// breaking extensibility.
    ///
    /// `ToolKind` is `#[serde(other)]`, so an unknown deserialized `kind` becomes
    /// `Some(Other)` (dropped by restrictive modes) rather than an error — not a
    /// live path today since `kind` is auto-populated and `from_id` leaves it
    /// `None`. [`deserialize_config_kind`] warns on that sink so a config typo
    /// doesn't silently demote the tool.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_config_kind"
    )]
    pub kind: Option<ToolKind>,
}
/// Deserialize `ToolConfig::kind`, warning when an unknown string sinks into
/// `ToolKind::Other` via `#[serde(other)]` — otherwise a config typo silently
/// demotes the tool in restrictive capability modes.
fn deserialize_config_kind<'de, D>(deserializer: D) -> Result<Option<ToolKind>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let Some(raw) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let kind = ToolKind::deserialize(serde::de::value::StrDeserializer::<D::Error>::new(&raw))?;
    if kind == ToolKind::Other && raw != "other" {
        tracing::warn!(kind = %raw, "unknown tool kind in config; treating as \"other\"");
    }
    Ok(Some(kind))
}
impl ToolConfig {
    /// Build a `ToolConfig` from a tool TYPE.
    ///
    /// The fully-qualified id (`"<namespace>:<id>"`) and `kind` are derived
    /// from the type via `ToolMetadata::tool_namespace()` and
    /// `echo_agent_tool_runtime::Tool::id()`. Use this for built-in tools known
    /// at compile time — it gives compile-time checking of the tool name
    /// and auto-populates `kind` so capability-mode filtering works.
    ///
    /// Requires `T: Default` because deriving the namespace/id needs an
    /// instance. All built-in tools satisfy this (it is also a bound on
    /// `ToolRegistryBuilder::register`).
    pub fn for_tool<T>() -> Self
    where
        T: crate::types::tool_metadata::ToolMetadata + echo_agent_tool_runtime::Tool + Default,
    {
        Self::from(&T::default())
    }
    /// Build a `ToolConfig` from a string id (no associated Rust type).
    ///
    /// Use this for MCP/custom tools or anywhere the id is only known at
    /// runtime. `kind` is left as `None`; capability-mode filtering then
    /// preserves the tool unconditionally.
    pub fn from_id(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            params: None,
            name_override: None,
            params_name_overrides: None,
            description_override: None,
            behavior_version: None,
            kind: None,
        }
    }
    /// Set the client-facing tool name (overrides the default id).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name_override = Some(name.into());
        self
    }
    /// Replace the tool's `description_template()` with a custom description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description_override = Some(desc.into());
        self
    }
    /// Set a tool configuration parameter (stored in `params` JSON map).
    pub fn with_param(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.params
            .get_or_insert_with(serde_json::Map::new)
            .insert(key.into(), value.into());
        self
    }
    /// Add a single parameter name remapping (canonical -> client-facing).
    pub fn with_param_rename(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.params_name_overrides
            .get_or_insert_with(HashMap::new)
            .insert(from.into(), to.into());
        self
    }
    /// Resolve the client-facing tool name.
    ///
    /// Returns `name_override` when set, otherwise falls back to `default_id`
    /// (typically `ToolEntry::id` — the unqualified tool name such as
    /// `"read_file"`).
    pub fn resolve_client_name(&self, default_id: &str) -> String {
        self.name_override
            .clone()
            .unwrap_or_else(|| default_id.to_owned())
    }
}
impl<T: crate::types::tool_metadata::ToolMetadata + echo_agent_tool_runtime::Tool> From<&T>
    for ToolConfig
{
    fn from(tool: &T) -> Self {
        Self {
            id: format!(
                "{}:{}",
                tool.tool_namespace(),
                echo_agent_tool_runtime::Tool::id(tool).as_str()
            ),
            params: None,
            name_override: None,
            params_name_overrides: None,
            description_override: None,
            behavior_version: None,
            kind: Some(tool.kind()),
        }
    }
}
/// What the client sends: which tools to enable, their params, and
/// how to rename tools/params for the client-facing API.
/// TODO: This whole thing is a map from the tool_id to the per tool config
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ToolServerConfig {
    pub tools: Vec<ToolConfig>,
    /// Behavior preset name (e.g. `"current"`, `"legacy-0.4.10"`).
    /// Applied to all version-managed tools. Defaults to `"current"` when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_preset: Option<String>,
}

/// Built-in tool id used by Desktop configurations saved before its removal.
#[cfg(feature = "desktop")]
pub const RETIRED_LSP_TOOL_ID: &str = "EchoAgentBuild:lsp";

#[derive(Clone)]
pub struct SubagentSessionResources {
    pub backend: crate::implementations::echo_agent_build::task::backend::SubagentBackendResource,
    /// Same channel as [`Self::backend`]; required so `TaskCompletionReminder`
    /// can drain completions onto the next tool result (shell parity).
    pub event_sender: crate::implementations::echo_agent_build::task::types::SubagentEventSender,
    pub depth: crate::implementations::echo_agent_build::task::types::SubagentDepthCounter,
    pub session_id: crate::implementations::echo_agent_build::task::types::SessionIdResource,
}
/// Everything a session provides at finalization time.
///
/// This is the **public API boundary** — callers pass concrete, strongly-typed
/// values. The builder converts these into type-erased `Resources` entries
/// internally, so callers never touch `Resources` directly.
pub struct SessionContext {
    /// Terminal backend for shell execution (bash, kill_task, task_output).
    pub backend: Arc<dyn TerminalBackend>,
    /// Async file system abstraction (read_file, search_replace).
    pub fs: Arc<dyn AsyncFileSystem>,
    /// Working directory for the session.
    pub cwd: PathBuf,
    /// Session-scoped folder for logs, output files, etc.
    pub session_folder: PathBuf,
    /// Environment variables inherited by shell commands (.envrc, etc.).
    pub session_env: Arc<HashMap<String, String>>,
    /// Handle for streaming tool output notifications.
    pub notification_handle: ToolNotificationHandle,
    /// Session ID that owns processes spawned by this session's tools.
    /// Used to scope kill operations on a shared terminal backend.
    pub owner_session_id: Option<String>,
    /// Complete subagent capability for this session.
    pub subagent: Option<SubagentSessionResources>,
    /// Parent's scheduler handle. When `Some`, the session reuses the parent's
    /// scheduler actor instead of spawning its own, so scheduled tasks survive
    /// subagent exit.
    pub parent_scheduler_handle:
        Option<crate::implementations::echo_agent_build::scheduler::types::SchedulerHandle>,
    /// Available skills for the Skill tool and description templates.
    pub skills: Vec<SkillInfo>,
    /// File path for persisting Resources state across restarts.
    ///
    /// The toolset loads existing state on construction and auto-saves
    /// after every tool execution. The file stores serialized `State<T>`
    /// values (e.g., `TodoState`).
    ///
    /// Empty means this registry gives the session a handle that reads and writes nothing. `echo-agent-core` reads
    /// the same empty value as "use the temp directory" for `session_folder`; unifying the two is a follow-up.
    pub state_path: PathBuf,
    /// Optional memory backend for cross-session knowledge retrieval.
    /// When `Some`, injected into `Resources` so `memory_search` / `memory_get`
    /// tools can access it. When `None`, the tools return "not enabled".
    pub memory_backend: Option<Arc<dyn crate::types::memory_backend::MemoryBackend>>,
    /// Optional web search configuration. When `Enabled`, a `WebSearchClient`
    /// is created and injected into `Resources` so the `web_search` tool can
    /// call the Responses API. When `Disabled` (default), the tool returns a
    /// graceful error if invoked.
    pub web_search_config: crate::implementations::web_search::WebSearchConfig,
    /// Optional web fetch configuration. When `Enabled`, a `WebFetchClient`
    /// is created and injected into `Resources` so the `web_fetch` tool can
    /// fetch URLs. When `Disabled` (default), the tool is not registered.
    pub web_fetch_config: crate::implementations::echo_agent_build::web_fetch::WebFetchConfig,
    /// Optional shared LSP handle — created once by the caller (shell),
    /// passed to every session. Same pattern as `fs` and `backend`.
    /// When `Some`, inserted into `Resources` so `LspTool` can use it.
    pub lsp: Option<std::sync::Arc<dyn crate::implementations::lsp::LspBackend>>,
    /// Optional image generation configuration. When `Enabled`, an `ImageGenClient`
    /// is created and injected into `Resources` so the `image_gen` tool can
    /// call the EchoAgent Imagine API. When `Disabled` (default), the tool is not
    /// registered and image generation is unavailable.
    pub image_gen_config: crate::implementations::echo_agent_build::image_gen::ImageGenConfig,
    /// Optional video generation configuration. When `Enabled`, a `VideoGenClient`
    /// is created and injected into `Resources` so the `video_gen` tool can
    /// call the EchoAgent Video Generation API. When `Disabled` (default), the tool is not
    /// registered and video generation is unavailable.
    pub video_gen_config: crate::implementations::echo_agent_build::video_gen::VideoGenConfig,
    /// Optional deploy service configuration. When enabled, the
    /// `deploy_app` tool connects to the service at call time using the shared
    /// API key provider.
    pub app_builder_deployer_config:
        crate::implementations::echo_agent_build::app_builder::AppBuilderDeployerConfig,
    /// Dynamic API key provider for tool HTTP clients.
    /// When set, clients resolve the API key per-request from this provider
    /// instead of using the key baked into their config at construction time.
    /// Prevents 401 failures when a session outlives the initial token lifetime.
    pub api_key_provider: Option<crate::types::SharedApiKeyProvider>,
    /// Auth provider which returns a echo_agent_computer_hub_sdk::AuthCredential. Can be used by
    /// tools that need to authenticate with services.
    ///
    /// Not to be confused with the api_key_provider, which is a legacy
    /// provider used by the shell's auth manager.
    pub auth_provider: Option<echo_agent_computer_hub_sdk::SharedAuthProvider>,
    /// Optional 401-attribution callback for tool HTTP clients. When
    /// set, a 401 from `image_gen` / `video_gen` / `web_search`
    /// emits an `auth_401_attribution` event via this hook. Hosts can
    /// wire this to the same attribution sink used for inference-side
    /// 401s so tool and chat auth failures share one telemetry path.
    pub attribution_callback: Option<crate::SharedAttributionCallback>,
    /// Tag name for `<system-reminder>` wrappers in tool result text.
    /// Defaults to [`crate::reminders::DEFAULT_REMINDER_TAG`] (hyphen).
    /// Hosts that expect a different tag name may override this.
    pub system_reminder_tag: &'static str,
}
/// Default metadata for dynamically registered tools (e.g., MCP tools)
/// that don't implement `ToolMetadata`.
struct DefaultToolMetadata {
    kind: ToolKind,
    description: String,
}
#[async_trait::async_trait]
impl ToolMetadata for DefaultToolMetadata {
    fn kind(&self) -> ToolKind {
        self.kind
    }
    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::MCP
    }
    fn description_template(&self) -> &str {
        &self.description
    }
}
/// Drain a `ToolStream<TypedToolOutput>` to the terminal result's `value`.
/// Progress items are discarded.
pub async fn drain_value_stream(
    mut stream: echo_agent_tool_runtime::ToolStream<echo_agent_tool_runtime::TypedToolOutput>,
) -> Result<serde_json::Value, echo_agent_tool_runtime::ToolError> {
    use futures::StreamExt;
    while let Some(item) = stream.next().await {
        match item {
            echo_agent_tool_runtime::ToolStreamItem::Progress(_) => continue,
            echo_agent_tool_runtime::ToolStreamItem::Terminal(result) => {
                return result.map(|typed| typed.value);
            }
        }
    }
    Err(stream_no_terminal_error())
}
/// The error yielded when a dispatch stream ends without a terminal item.
/// Centralized so the code/message can't drift across call sites.
fn stream_no_terminal_error() -> echo_agent_tool_runtime::ToolError {
    echo_agent_tool_runtime::ToolError::custom(
        "stream_no_terminal",
        "dispatch stream ended without a terminal item",
    )
}
/// Converts a dispatch's `serde_json::Value` back into a `ToolOutput`.
type OutputConverter =
    Arc<dyn Fn(serde_json::Value) -> Result<ToolOutput, serde_json::Error> + Send + Sync>;
/// Everything captured during pre-dispatch setup that the dispatch and the
/// post-dispatch tail need. Produced by [`FinalizedToolset::prepare_dispatch`]
/// after the tools read guard has been dropped, so none of this is held across
/// `.await`.
struct DispatchParts {
    /// Resolved `LocalRegistry` handle to dispatch through.
    lr_handle: Arc<dyn echo_agent_computer_hub_core::ToolHandle>,
    /// Runtime context built for the call (resources, renderer, cwd,
    /// behavior version, inner-dispatch).
    ctx: echo_agent_tool_runtime::ToolCallContext,
    /// Canonical (reverse-remapped) params to pass to dispatch.
    canonical_params: serde_json::Value,
    /// Converts the dispatch's `serde_json::Value` back to `ToolOutput`.
    output_converter: OutputConverter,
    /// `use_tool` target tool name, surfaced in the final `ToolRunResult`.
    effective_tool_name: Option<String>,
}
/// Per-tool metadata + instance stored in the builder.
///
/// Stores a type-erased dispatch handle (`ToolDispatchHandle`) and
/// metadata (`ToolMetadata`) for each registered tool. Params-related
/// closures capture the concrete `P` type at registration time.
#[allow(clippy::type_complexity)]
struct ToolEntry {
    namespace: String,
    id: String,
    kind: ToolKind,
    requires: Expr<ToolRequirement>,
    default_params: serde_json::Value,
    input_schema: serde_json::Value,
    /// Tool metadata — kind, description, doom-loop, definitions, reminders.
    metadata: Box<dyn ToolMetadata>,
    /// Converts `serde_json::Value` (from dispatch) back to `ToolOutput`.
    /// Captured at registration time with knowledge of `T::Output`.
    output_converter:
        Box<dyn Fn(serde_json::Value) -> Result<ToolOutput, serde_json::Error> + Send + Sync>,
    /// Validates client JSON against `T::Params`.
    validate_params:
        Box<dyn Fn(&serde_json::Value) -> Result<(), ParamValidationError> + Send + Sync>,
    /// Applies validated JSON params into Resources as `Params<T>`.
    apply_params: Box<dyn Fn(&serde_json::Value, &mut Resources) + Send + Sync>,
    /// Registers `Params<T::Params>` for serialization in Resources.
    /// Noop when `T::Params = ()`.
    register_params: Box<dyn Fn(&mut Resources) + Send + Sync>,
    parse_input: Box<
        dyn Fn(serde_json::Value) -> Result<ToolInput, echo_agent_tool_runtime::ToolError>
            + Send
            + Sync,
    >,
    /// Registers this tool into a `LocalRegistry` using the concrete type.
    /// Captured at `register::<T>()` time when T is known.
    register_in_local: Box<dyn Fn(&echo_agent_computer_hub_sdk::LocalRegistry) + Send + Sync>,
}
/// Per-reminder metadata stored in the builder.
struct ReminderEntry {
    requires: Expr<ToolRequirement>,
    reminder: Box<dyn Reminder + Send + Sync>,
}
/// A tool ready for dispatch, with pre-built client-facing definition.
struct FinalizedTool {
    namespace: String,
    id: String,
    /// The key under which this tool is stored in the `LocalRegistry`.
    /// For built-in tools this equals `id`; for dynamically-registered
    /// (MCP) tools it is `Tool::id().as_str()` which may differ from
    /// the client-facing `id` / `client_name`.
    registry_id: String,
    client_name: String,
    /// Tool metadata — kind, fingerprinting, doom-loop, reminders.
    metadata: Arc<dyn ToolMetadata>,
    /// Converts `serde_json::Value` (from dispatch) back to `ToolOutput`.
    output_converter:
        Arc<dyn Fn(serde_json::Value) -> Result<ToolOutput, serde_json::Error> + Send + Sync>,
    definition: ToolDefinition,
    /// Effective params (defaults merged with client overrides).
    /// Kept for building `ProposedTool` during reminder evaluation and for
    /// params-aware finalized definition construction.
    effective_params: serde_json::Value,
    /// Canonical input schema (JSON Schema) derived from the Rust input type.
    /// This remains the internal schema used for `InputParam` requirement checks
    /// and TemplateRenderer param-name exposure, even if the exported tool
    /// definition schema is specialized per effective params.
    input_schema: serde_json::Value,
    /// Client-facing param → canonical param, for reverse-remapping at dispatch.
    reverse_params: HashMap<String, String>,
    /// useful for parsing input to specific type
    parse_input: Arc<
        dyn Fn(serde_json::Value) -> Result<ToolInput, echo_agent_tool_runtime::ToolError>
            + Send
            + Sync,
    >,
    /// Resolved behavior contract version for this tool (e.g. `"current"`,
    /// `"legacy-0.4.10"`). `None` for unmanaged tools and dynamically
    /// registered (MCP) tools.
    contract_version: Option<String>,
}
/// Toolset produced by `ToolRegistryBuilder::finalize()`.
///
/// The tools vector is wrapped in `parking_lot::RwLock` to allow concurrent
/// read access (tool dispatch) with rare write access (MCP tool registration).
/// The read guard is held only for microsecond lookups — never across `.await`.
pub struct FinalizedToolset {
    tools: parking_lot::RwLock<Vec<FinalizedTool>>,
    reminders: Vec<Box<dyn Reminder + Send + Sync>>,
    pub resources: SharedResources,
    resources_persistence: Arc<ResourcesPersistence>,
    scheduler_cancel: Option<tokio_util::sync::CancellationToken>,
    /// Shared local registry for in-process dispatch.
    /// Contains only config-enabled tools. Can be shared with ToolHarness.
    local_registry: echo_agent_computer_hub_sdk::LocalRegistry,
    /// Lock-free access to the template renderer for tool name/param resolution.
    /// Cloned into `ToolCallContext::extensions` on each `call()` so tools
    /// can resolve names without acquiring the `resources` mutex.
    renderer: Arc<TemplateRenderer>,
    /// Tag name for system-reminder wrappers in tool result text.
    system_reminder_tag: &'static str,
    /// Per-user feature-flag bag stamped on every dispatch ctx by
    /// `prepare_dispatch`. `None` outside a workspace bind.
    workspace_viewer_ctx: Option<echo_agent_tool_runtime::WorkspaceViewerContext>,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RequirementError {
    pub tool: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bad_value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}
impl RequirementError {
    pub fn new(tool: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            message: message.into(),
            field_path: None,
            expected: None,
            bad_value: None,
            category: None,
        }
    }
    pub fn with_field_path(mut self, field_path: impl Into<String>) -> Self {
        self.field_path = Some(field_path.into());
        self
    }
    pub fn with_expected(mut self, expected: impl Into<String>) -> Self {
        self.expected = Some(expected.into());
        self
    }
    pub fn with_bad_value(mut self, bad_value: serde_json::Value) -> Self {
        self.bad_value = Some(bad_value);
        self
    }
    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }
    pub fn summary(&self) -> String {
        let mut parts = vec![];
        if let Some(path) = &self.field_path {
            parts.push(path.clone());
        }
        parts.push(self.message.clone());
        if let Some(expected) = &self.expected {
            parts.push(format!("expected {expected}"));
        }
        if let Some(value) = &self.bad_value {
            parts.push(format!("got {}", value));
        }
        format!("{}: {}", self.tool, parts.join("; "))
    }
}
pub struct ToolRegistryBuilder {
    tools: HashMap<String, ToolEntry>,
    reminders: Vec<ReminderEntry>,
    shared_local_registry: Option<echo_agent_computer_hub_sdk::LocalRegistry>,
    /// Whether the client delivers system reminders (completion
    /// notifications for backgrounded commands/subagents) to the model.
    /// Exposed to description templates as `system_reminders_enabled` so
    /// "you are notified on completion" promises are only rendered when
    /// the client actually delivers them. Defaults to `true` (prod CLI
    /// behavior); the tools server sets it from
    /// `FinalizeToolServerConfigRequest.system_reminders_enabled`.
    system_reminders_enabled: bool,
}
impl Default for ToolRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}
impl ToolRegistryBuilder {
    /// Register a built-in tool with no configuration params.
    ///
    /// For tools with typed params, use [`register_with_params`] instead.
    ///
    /// `pub` so out-of-tree tool packs registered via
    /// [`register_tool_pack`] can contribute tool registrations.
    pub fn register<T>(&mut self)
    where
        T: echo_agent_tool_runtime::Tool
            + ToolMetadata
            + std::fmt::Debug
            + Default
            + Send
            + Sync
            + 'static,
        T::Args: serde::de::DeserializeOwned + schemars::JsonSchema + Into<ToolInput>,
        T::Output: serde::Serialize + serde::de::DeserializeOwned + Into<ToolOutput>,
    {
        self.register_with_params::<T, ()>();
    }
    /// Register a built-in tool with typed configuration params.
    ///
    /// `P` is the tool's configuration type (e.g. `BashParams`), stored
    /// as `Params<P>` in Resources. For tools with no config, use
    /// [`register`] which defaults `P = ()`.
    ///
    /// `pub` so out-of-tree tool packs registered via
    /// [`register_tool_pack`] can contribute tool registrations.
    pub fn register_with_params<T, P>(&mut self)
    where
        T: echo_agent_tool_runtime::Tool
            + ToolMetadata
            + std::fmt::Debug
            + Default
            + Send
            + Sync
            + 'static,
        T::Args: serde::de::DeserializeOwned + schemars::JsonSchema + Into<ToolInput>,
        T::Output: serde::Serialize + serde::de::DeserializeOwned + Into<ToolOutput>,
        P: crate::types::resources::ResourceType
            + serde::Serialize
            + serde::de::DeserializeOwned
            + Default
            + Send
            + Sync
            + 'static,
    {
        let tool = T::default();
        let name = format!(
            "{}:{}",
            tool.tool_namespace(),
            echo_agent_tool_runtime::Tool::id(&tool).as_str()
        );
        let namespace = tool.tool_namespace().to_string();
        let id = echo_agent_tool_runtime::Tool::id(&tool)
            .as_str()
            .to_string();
        let kind = tool.kind();
        let requires = tool.requires_expr();
        self.tools.insert(
            name,
            ToolEntry {
                namespace,
                id,
                kind,
                requires,
                default_params: serde_json::to_value(P::default()).unwrap_or_default(),
                input_schema: generate_schema_cached::<T::Args>(),
                metadata: Box::new(tool),
                output_converter: Box::new(|value| {
                    let typed: T::Output = serde_json::from_value(value)?;
                    Ok(typed.into())
                }),
                validate_params: Box::new(validate_params_json::<P>),
                apply_params: Box::new(|json, resources| {
                    if let Ok(typed) = serde_json::from_value::<P>(json.clone()) {
                        resources.insert(crate::types::resources::Params(typed));
                    }
                }),
                register_params: Box::new(|resources| {
                    if !P::ID.is_empty() {
                        resources.register_params::<P>();
                    }
                }),
                parse_input: Box::new(|json| {
                    let typed = serde_json::from_value::<T::Args>(json)?;
                    Ok(typed.into())
                }),
                register_in_local: Box::new(|lr: &echo_agent_computer_hub_sdk::LocalRegistry| {
                    lr.register(T::default());
                }),
            },
        );
    }
    /// Whether this registry knows the fully-qualified tool id
    /// (`"EchoAgentBuild:read_file"`).
    pub fn has_tool_id(&self, id: &str) -> bool {
        self.tools.contains_key(id)
    }
    pub fn known_tool_ids(&self) -> std::collections::HashSet<String> {
        self.tools.keys().cloned().collect()
    }
    /// Fully-qualified tool id (`"EchoAgentBuild:read_file"`) → declared
    /// [`ToolKind`], for every registered tool. Lets consumers that receive
    /// kind-less tool configs (e.g. hub `session.bind` wire entries) backfill
    /// the kind from the binary's own registry before capability filtering.
    pub fn known_tool_kinds(&self) -> HashMap<String, ToolKind> {
        self.tools
            .iter()
            .map(|(name, entry)| (name.clone(), entry.kind))
            .collect()
    }
    /// Register a cross-cutting reminder.
    ///
    /// Cross-cutting reminders fire after every tool call. They inspect
    /// `ToolOutput` and `Resources` to decide whether to emit reminder text.
    ///
    /// Reminders that need tool/param names use `TemplateRenderer` from
    /// Resources at runtime — no per-reminder configuration needed.
    fn register_reminder<R>(&mut self, reminder: R)
    where
        R: Reminder + Send + Sync + 'static,
    {
        let requires = reminder.requires_expr();
        self.reminders.push(ReminderEntry {
            requires,
            reminder: Box::new(reminder),
        });
    }
    pub fn new() -> Self {
        let mut b = Self {
            tools: HashMap::new(),
            reminders: Vec::new(),
            shared_local_registry: None,
            system_reminders_enabled: true,
        };
        b.register_with_params::<echo_agent_build::BashTool, echo_agent_build::bash::BashParams>();
        b.register_with_params::<echo_agent_build::ReadFileTool, echo_agent_build::read_file::ReadFileParams>();
        b.register_with_params::<
                echo_agent_build::SearchReplaceTool,
                echo_agent_build::search_replace::SearchReplaceParams,
            >();
        b.register_with_params::<echo_agent_build::ListDirTool, echo_agent_build::list_dir::ListDirParams>();
        b.register_with_params::<echo_agent_build::GrepTool, echo_agent_build::grep::GrepParams>();
        b.register::<echo_agent_build::KillTaskTool>();
        b.register::<echo_agent_build::KillTerminalCommandTool>();
        b.register::<echo_agent_build::TodoWriteTool>();
        b.register::<echo_agent_build::UpdateGoalTool>();
        b.register::<echo_agent_build::WorkflowTool>();
        b.register::<echo_agent_build::TaskOutputTool>();
        b.register::<echo_agent_build::GetTerminalCommandOutputTool>();
        b.register::<echo_agent_build::WaitTasksTool>();
        b.register::<echo_agent_build::TaskTool>();
        b.register::<echo_agent_build::WebSearchTool>();
        b.register_with_params::<echo_agent_build::WebFetchTool, echo_agent_build::web_fetch::WebFetchParams>();
        #[cfg(not(feature = "desktop"))]
        b.register::<echo_agent_build::LspTool>();
        b.register::<echo_agent_build::ImageGenTool>();
        b.register::<echo_agent_build::ImageEditTool>();
        b.register::<echo_agent_build::ImageToVideoTool>();
        b.register::<echo_agent_build::ReferenceToVideoTool>();
        b.register::<echo_agent_build::EnterPlanModeTool>();
        b.register::<echo_agent_build::ExitPlanModeTool>();
        b.register_with_params::<
                echo_agent_build::AskUserQuestionTool,
                echo_agent_build::ask_user_question::AskUserQuestionParams,
            >();
        b.register::<echo_agent_build::MonitorTool>();
        b.register::<echo_agent_build::SchedulerCreateTool>();
        b.register::<echo_agent_build::SchedulerDeleteTool>();
        b.register::<echo_agent_build::SchedulerListTool>();
        b.register::<codex::apply_patch::ApplyPatchTool>();
        b.register::<codex::list_dir::CodexListDirTool>();
        b.register::<codex::grep_files::CodexGrepFilesTool>();
        b.register::<codex::read_file::CodexReadFileTool>();
        b.register::<opencode::OpenCodeBashTool>();
        b.register::<opencode::OpenCodeReadTool>();
        b.register::<opencode::OpenCodeEditTool>();
        b.register::<opencode::OpenCodeWriteTool>();
        b.register::<opencode::OpenCodeGrepTool>();
        b.register::<opencode::OpenCodeGlobTool>();
        b.register::<opencode::OpenCodeTodoWriteTool>();
        b.register::<opencode::OpenCodeSkillTool>();
        b.register::<crate::implementations::memory::search_tool::MemorySearchImpl>();
        b.register::<crate::implementations::memory::get_tool::MemoryGetImpl>();
        b.register::<crate::implementations::search_tool::SearchTool>();
        b.register_with_params::<
                crate::implementations::use_tool::UseTool,
                crate::implementations::use_tool::UseToolParams,
            >();
        b.register_with_params::<
                echo_agent_build_concise::ReadFileConciseTool,
                echo_agent_build::read_file::ReadFileParams,
            >();
        b.register_with_params::<
                echo_agent_build_concise::SearchReplaceConciseTool,
                echo_agent_build::search_replace::SearchReplaceParams,
            >();
        b.register_with_params::<
                echo_agent_build_concise::BashConciseTool,
                echo_agent_build::bash::BashParams,
            >();
        b.register_with_params::<
                echo_agent_build_hashline::HashlineReadTool,
                echo_agent_build_hashline::config::HashlineSchemeParams,
            >();
        b.register_with_params::<
                echo_agent_build_hashline::HashlineEditTool,
                echo_agent_build_hashline::config::HashlineSchemeParams,
            >();
        b.register_with_params::<
                echo_agent_build_hashline::HashlineGrepTool,
                echo_agent_build_hashline::config::HashlineSchemeParams,
            >();
        #[cfg(not(feature = "desktop"))]
        b.register_reminder(crate::reminders::LspDiagnosticsReminder);
        b.register_reminder(crate::reminders::TaskCompletionReminder);
        b.register_reminder(SkillDiscoveryReminder);
        for pack in tool_packs().lock().iter() {
            pack(&mut b);
        }
        b
    }
    pub fn with_local_registry(
        mut self,
        registry: echo_agent_computer_hub_sdk::LocalRegistry,
    ) -> Self {
        self.shared_local_registry = Some(registry);
        self
    }
    /// Dump tools manifest as JSON for the client.
    pub fn get_tools_config_raw(&self) -> serde_json::Value {
        let out: HashMap<&str, serde_json::Value> = self
            .tools
            .iter()
            .map(|(name, e)| {
                (
                    name.as_str(),
                    serde_json::json!({
                        "namespace": e.namespace,
                        "id": e.id,
                        "kind": e.kind,
                        "default_params": e.default_params,
                        "input_schema": e.input_schema,
                        "requires": e.requires,
                    }),
                )
            })
            .collect();
        serde_json::to_value(&out).expect("tool_config_raw_to_not_fail")
    }
    /// Validate a client-proposed configuration. Returns errors (empty = valid).
    pub fn validate_config(&self, config: &ToolServerConfig) -> Vec<RequirementError> {
        let mut errors = vec![];
        let preset_name = config.behavior_preset.as_deref().unwrap_or("current");
        if crate::versions::lookup_preset(preset_name).is_none() {
            errors.push(
                RequirementError::new(
                    "(global)",
                    format!("unknown behavior_preset: \"{preset_name}\""),
                )
                .with_field_path("behavior_preset")
                .with_expected("one of the registered behavior presets")
                .with_bad_value(serde_json::Value::String(preset_name.to_owned()))
                .with_category("behavior_preset"),
            );
            return errors;
        }
        let mut resolved: Vec<(&ToolEntry, serde_json::Value)> = Vec::new();
        for tool_config in &config.tools {
            let Some(entry) = self.tools.get(tool_config.id.as_str()) else {
                tracing::warn!(
                    tool_id = %tool_config.id,
                    registered_keys = ?self.tools.keys().collect::<Vec<_>>(),
                    "validate_config: tool NOT FOUND in registry"
                );
                errors.push(
                    RequirementError::new(tool_config.id.clone(), "not found in registry")
                        .with_field_path("id")
                        .with_bad_value(serde_json::Value::String(tool_config.id.clone()))
                        .with_category("tool_not_found"),
                );
                continue;
            };
            if let Err(message) = crate::versions::resolve_version(
                preset_name,
                &tool_config.id,
                tool_config.behavior_version.as_deref(),
            ) {
                errors.push(
                    RequirementError::new(tool_config.id.clone(), message)
                        .with_field_path("behavior_version")
                        .with_bad_value(serde_json::Value::String(
                            tool_config.behavior_version.clone().unwrap_or_default(),
                        ))
                        .with_category("behavior_version"),
                );
                continue;
            }
            let effective = match compute_effective_params(entry, tool_config) {
                Ok(effective) => effective,
                Err(e) => {
                    errors.push(requirement_error_from_param_error(&tool_config.id, e));
                    continue;
                }
            };
            resolved.push((entry, effective));
        }
        if !errors.is_empty() {
            return errors;
        }
        {
            let mut seen_names: HashMap<String, String> = HashMap::new();
            for (tool_config, (entry, _)) in config.tools.iter().zip(resolved.iter()) {
                let client_name = tool_config.resolve_client_name(&entry.id);
                if let Some(prev_id) = seen_names.get(&client_name) {
                    errors.push(
                        RequirementError::new(
                            tool_config.id.clone(),
                            format!(
                                "duplicate client_name \"{client_name}\": \
                                 already used by {prev_id}. Use name_override \
                                 to give each tool a unique client-facing name."
                            ),
                        )
                        .with_field_path("name_override")
                        .with_expected("a unique client-facing tool name")
                        .with_bad_value(serde_json::Value::String(client_name.clone()))
                        .with_category("duplicate_client_name"),
                    );
                } else {
                    seen_names.insert(client_name, tool_config.id.clone());
                }
            }
        }
        if !errors.is_empty() {
            return errors;
        }
        {
            let standard_file_ids: &[&str] = &[
                "EchoAgentBuild:read_file",
                "EchoAgentBuild:search_replace",
                "EchoAgentBuild:grep",
            ];
            let hashline_file_ids: &[&str] = &[
                "EchoAgentBuildHashline:hashline_read",
                "EchoAgentBuildHashline:hashline_edit",
                "EchoAgentBuildHashline:hashline_grep",
            ];
            let has_standard = config
                .tools
                .iter()
                .any(|t| standard_file_ids.contains(&t.id.as_str()));
            let has_hashline = config
                .tools
                .iter()
                .any(|t| hashline_file_ids.contains(&t.id.as_str()));
            if has_standard && has_hashline {
                errors.push(
                    RequirementError::new(
                        "(file-toolset)",
                        "mixed standard and hashline file tools are not allowed. \
                         Use either the standard bundle (read_file, search_replace, grep) \
                         or the hashline bundle (hashline_read, hashline_edit, hashline_grep), \
                         not both.",
                    )
                    .with_field_path("tools")
                    .with_category("file_toolset_conflict"),
                );
            }
        }
        if !errors.is_empty() {
            return errors;
        }
        let proposed: Vec<ProposedTool> = resolved
            .iter()
            .map(|(e, params)| ProposedTool {
                namespace: &e.namespace,
                id: &e.id,
                kind: e.kind,
                params,
                input_schema: Some(&e.input_schema),
            })
            .collect();
        for (entry, params) in &resolved {
            let ctx = EvalContext {
                tools: &proposed,
                self_params: params,
            };
            if !entry.requires.eval(&|req| req.eval(&ctx)) {
                errors.push(explain_requirement_failure(entry, params, &proposed));
            }
        }
        errors
    }
    /// Validate and finalize into an immutable toolset.
    /// Consumes the builder — no further modifications possible.
    /// Set whether the client delivers system reminders to the model.
    /// Must be called before [`finalize`]; affects how description
    /// templates render notification promises (see
    /// `TemplateContext::system_reminders_enabled`).
    pub fn set_system_reminders_enabled(&mut self, enabled: bool) {
        self.system_reminders_enabled = enabled;
    }
    pub fn finalize(
        self,
        config: ToolServerConfig,
        ctx: SessionContext,
    ) -> Result<FinalizedToolset, Vec<RequirementError>> {
        self.finalize_with_trunc_config(
            config,
            ctx,
            crate::types::context::TruncationConfig::default(),
            None,
        )
    }
    /// Finalize with an explicit `TruncationConfig` so that per-tool
    /// `max_output_bytes` overrides are reflected in tool descriptions.
    /// `workspace_viewer_ctx` is `None` outside a workspace bind.
    pub fn finalize_with_trunc_config(
        mut self,
        config: ToolServerConfig,
        ctx: SessionContext,
        truncation_config: crate::types::context::TruncationConfig,
        workspace_viewer_ctx: Option<echo_agent_tool_runtime::WorkspaceViewerContext>,
    ) -> Result<FinalizedToolset, Vec<RequirementError>> {
        #[cfg(feature = "desktop")]
        let config = {
            let mut config = config;
            config.tools.retain(|tool| tool.id != RETIRED_LSP_TOOL_ID);
            config
        };
        let errors = self.validate_config(&config);
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut kind_to_name: HashMap<ToolKind, String> = HashMap::new();
        for tool_config in &config.tools {
            let entry = &self.tools[&tool_config.id];
            let client_name = tool_config.resolve_client_name(&entry.id);
            kind_to_name.entry(entry.kind).or_insert(client_name);
        }
        let mut kind_params: HashMap<ToolKind, HashMap<String, String>> = HashMap::new();
        for tool_config in &config.tools {
            let entry = &self.tools[&tool_config.id];
            let map = kind_params.entry(entry.kind).or_default();
            if let Some(props) = entry
                .input_schema
                .get("properties")
                .and_then(|p| p.as_object())
            {
                for key in props.keys() {
                    map.entry(key.clone()).or_insert_with(|| key.clone());
                }
            }
            if let Some(overrides) = &tool_config.params_name_overrides {
                map.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
        let renderer = TemplateRenderer::new(kind_to_name.clone(), kind_params.clone())
            .with_system_reminders_enabled(self.system_reminders_enabled);
        let mut tools = Vec::new();
        let mut resources = Resources::new();
        resources.insert(crate::types::resources::Terminal(ctx.backend));
        resources.insert(crate::types::resources::FileSystem(ctx.fs));
        let cwd = ctx.cwd;
        resources.insert(crate::types::resources::Cwd(cwd.clone()));
        resources.insert(crate::types::resources::SessionFolder(ctx.session_folder));
        resources.insert(crate::types::resources::SessionEnv(ctx.session_env));
        if let Some(owner_session_id) = ctx.owner_session_id.clone() {
            resources.insert(crate::types::resources::OwnerSessionId(owner_session_id));
        }
        if let Some(subagent) = ctx.subagent {
            resources.insert(subagent.backend);
            resources.insert(subagent.event_sender);
            resources.insert(subagent.depth);
            resources.insert(subagent.session_id);
        }
        let scheduler_notification_handle = ctx.notification_handle.clone();
        resources.insert(crate::types::resources::NotificationHandle(
            ctx.notification_handle,
        ));
        let startup_skills = ctx.skills;
        resources.insert(crate::types::resources::AvailableSkills(
            startup_skills.clone(),
        ));
        {
            let mut mgr = crate::types::skill_discovery_tracker::SkillManager::new();
            mgr.set_discovery_snapshot_names(
                startup_skills.iter().map(|s| s.name.clone()).collect(),
            );
            mgr.seed(Some(cwd.clone()), None, startup_skills, None, None, None);
            let _ = mgr.take_pending();
            resources.insert(mgr);
        }
        if let Some(memory_backend) = ctx.memory_backend {
            resources.insert(memory_backend);
        }
        if let Some(auth_provider) = ctx.auth_provider.clone() {
            resources.insert(auth_provider);
        }
        if let Ok(client) = crate::implementations::web_search::client::WebSearchClient::new(
            &ctx.web_search_config,
            ctx.api_key_provider.clone(),
        ) {
            let client = client.with_attribution_callback(ctx.attribution_callback.clone());
            resources.insert(client);
        }
        if let Some(lsp) = ctx.lsp {
            resources.insert(lsp);
        }
        let image_gen_config = ctx.image_gen_config;
        let video_gen_config = ctx.video_gen_config;
        if image_gen_config.has_credentials() {
            match crate::implementations::echo_agent_build::image_gen::ImageGenClient::new(
                &image_gen_config,
                ctx.api_key_provider.clone(),
            ) {
                Ok(client) => {
                    let mut client =
                        client.with_attribution_callback(ctx.attribution_callback.clone());
                    if let Some(session_id) = &ctx.owner_session_id {
                        client = client.with_session_id(session_id);
                    }
                    resources.insert(client);
                }
                Err(e) => {
                    tracing::warn!("Failed to create ImageGenClient: {e}");
                }
            }
        }
        if video_gen_config.is_enabled() {
            match crate::implementations::echo_agent_build::video_gen::VideoGenClient::new(
                &video_gen_config,
                ctx.api_key_provider.clone(),
            ) {
                Ok(client) => {
                    let mut client =
                        client.with_attribution_callback(ctx.attribution_callback.clone());
                    if let Some(session_id) = &ctx.owner_session_id {
                        client = client.with_session_id(session_id);
                    }
                    resources.insert(client);
                }
                Err(e) => {
                    tracing::warn!("Failed to create VideoGenClient: {e}");
                }
            }
        }
        if let crate::implementations::echo_agent_build::web_fetch::WebFetchConfig::Enabled {
            params,
        } = &ctx.web_fetch_config
        {
            match crate::implementations::echo_agent_build::web_fetch::WebFetchClient::new(params) {
                Ok(client) => {
                    resources.insert(client);
                }
                Err(e) => {
                    tracing::warn!("Failed to create WebFetchClient: {e}");
                }
            }
        }
        let concise_ns = crate::types::tool::ToolNamespace::EchoAgentBuildConcise.to_string();
        let has_concise_tools = config.tools.iter().any(|tc| {
            self.tools
                .get(&tc.id)
                .is_some_and(|e| e.namespace == concise_ns)
        });
        if has_concise_tools {
            resources.insert(crate::types::resources::SystemRemindersEnabled(false));
        }
        resources.register_state::<crate::reminders::task_completion::ReportedTaskCompletions>();
        resources.register_state::<crate::implementations::echo_agent_build::todo::TodoState>();
        resources.register_state::<crate::types::resources::WebCitationCounter>();
        resources
            .register_state::<
                crate::implementations::cursor_rules_on_read::CursorRulesOnReadTracker,
            >();
        resources
            .register_state::<crate::implementations::echo_agent_build::scheduler::types::SchedulerState>(
            );
        for entry in self.tools.values() {
            (entry.register_params)(&mut resources);
        }
        let persistence = Arc::new(if ctx.state_path.as_os_str().is_empty() {
            ResourcesPersistence::noop()
        } else {
            let dir = ctx.state_path.parent().unwrap_or(&ctx.state_path);
            ResourcesPersistence::new(dir.join("resources_state.json"))
        });
        persistence.load(&mut resources);
        let preset_name = config.behavior_preset.as_deref().unwrap_or("current");
        let local_registry = self.shared_local_registry.take().unwrap_or_default();
        for tool_config in &config.tools {
            let entry = self.tools.remove(&tool_config.id).unwrap();
            (entry.register_in_local)(&local_registry);
            let contract_version = crate::versions::resolve_version(
                preset_name,
                &tool_config.id,
                tool_config.behavior_version.as_deref(),
            )
            .map_err(|e| {
                vec![
                    RequirementError::new(tool_config.id.clone(), e)
                        .with_field_path("behavior_version")
                        .with_bad_value(serde_json::Value::String(
                            tool_config.behavior_version.clone().unwrap_or_default(),
                        ))
                        .with_category("behavior_version"),
                ]
            })?;
            let client_name = tool_config.resolve_client_name(&entry.id);
            let param_map = tool_config
                .params_name_overrides
                .clone()
                .unwrap_or_default();
            let reverse_params: HashMap<String, String> = param_map
                .iter()
                .map(|(canonical, client)| (client.clone(), canonical.clone()))
                .collect();
            let effective_params = compute_effective_params(&entry, tool_config)
                .map_err(|e| vec![requirement_error_from_param_error(&tool_config.id, e)])?;
            let mut definition = entry.metadata.versioned_definition(
                contract_version.as_deref(),
                &client_name,
                tool_config.description_override.as_deref(),
                &renderer,
                &param_map,
                &entry.input_schema,
                &effective_params,
            );
            if let Some(desc) = &definition.function.description {
                definition.function.description = Some(truncation_config.interpolate_description(
                    desc,
                    &client_name,
                    crate::DEFAULT_TOOL_OUTPUT_BYTES,
                    echo_agent_tool_types::max_wait_block_ms(),
                ));
            }
            renderer.render_schema_descriptions(&mut definition.function.parameters);
            truncation_config.apply_to_schema(
                &mut definition.function.parameters,
                &client_name,
                crate::DEFAULT_TOOL_OUTPUT_BYTES,
                echo_agent_tool_types::max_wait_block_ms(),
            );
            (entry.apply_params)(&effective_params, &mut resources);
            tools.push(FinalizedTool {
                namespace: entry.namespace,
                registry_id: entry.id.clone(),
                id: entry.id,
                client_name,
                metadata: Arc::from(entry.metadata),
                output_converter: Arc::from(entry.output_converter),
                definition,
                effective_params,
                input_schema: entry.input_schema,
                reverse_params,
                parse_input: Arc::from(entry.parse_input),
                contract_version,
            });
        }
        let native_tool_names: std::collections::HashSet<String> = tools
            .iter()
            .filter(|t| !t.client_name.contains("__"))
            .map(|t| t.client_name.clone())
            .collect();
        resources.insert(crate::types::resources::EnabledNativeToolNames(
            native_tool_names,
        ));
        let proposed: Vec<ProposedTool> = tools
            .iter()
            .map(|t| ProposedTool {
                namespace: &t.namespace,
                id: &t.id,
                kind: t.metadata.kind(),
                params: &t.effective_params,
                input_schema: Some(&t.input_schema),
            })
            .collect();
        let empty_params = serde_json::Value::Object(Default::default());
        let mut active_reminders: Vec<Box<dyn Reminder + Send + Sync>> = Vec::new();
        for entry in self.reminders {
            let ctx = EvalContext {
                tools: &proposed,
                self_params: &empty_params,
            };
            if entry.requires.eval(&|req| req.eval(&ctx)) {
                active_reminders.push(entry.reminder);
            }
        }
        let renderer_arc = Arc::new(renderer.clone());
        resources.insert(renderer);
        let (scheduler_cmd_rx, scheduler_cancel_token) =
            if let Some(parent_handle) = ctx.parent_scheduler_handle {
                resources.insert(parent_handle);
                (None, None)
            } else {
                let (scheduler_cmd_tx, scheduler_cmd_rx) = tokio::sync::mpsc::unbounded_channel();
                let cancel_token = tokio_util::sync::CancellationToken::new();
                resources.insert(
                    crate::implementations::echo_agent_build::scheduler::types::SchedulerHandle(
                        scheduler_cmd_tx,
                    ),
                );
                (Some(scheduler_cmd_rx), Some(cancel_token))
            };
        let shared_resources = resources.into_shared();
        if let (Some(cmd_rx), Some(cancel_token)) = (scheduler_cmd_rx, &scheduler_cancel_token) {
            let actor =
                crate::implementations::echo_agent_build::scheduler::actor::SchedulerActor {
                    resources: shared_resources.clone(),
                    resources_persistence: persistence.clone(),
                    notification_handle: scheduler_notification_handle,
                    cmd_rx,
                    cancel_token: cancel_token.clone(),
                    clock: Default::default(),
                    pending_removal: None,
                    blocked_expiries: Default::default(),
                };
            tokio::spawn(actor.run());
        }
        Ok(FinalizedToolset {
            tools: parking_lot::RwLock::new(tools),
            reminders: active_reminders,
            resources: shared_resources,
            resources_persistence: persistence,
            scheduler_cancel: scheduler_cancel_token,
            local_registry,
            renderer: renderer_arc,
            system_reminder_tag: ctx.system_reminder_tag,
            workspace_viewer_ctx,
        })
    }
}
impl Drop for FinalizedToolset {
    fn drop(&mut self) {
        if let Some(cancel) = self.scheduler_cancel.take() {
            cancel.cancel();
        }
    }
}
/// Calls `FinalizedToolset::call_raw()`, bypassing the outer `ToolBridge`
/// mutex to avoid deadlock when `use_tool` dispatches to a target MCP tool.
///
/// Stored in [`InnerDispatch`] inside `ToolCallContext::extensions` —
/// stack-bounded, dropped when `Tool::run()` returns.
///
/// Implements the canonical `echo_agent_tool_runtime::ToolDispatch` trait so the
/// dispatch contract is uniform across all boundaries. The impedance
/// mismatch (`ToolStream<Value>` vs `Result<ToolOutput>`) is bridged by
/// serializing `ToolOutput` to `Value` in the stream; callers use
/// `call_terminal()` and deserialize back.
struct InnerDispatchForToolset {
    toolset: Arc<FinalizedToolset>,
}
#[async_trait::async_trait]
impl echo_agent_tool_runtime::ToolDispatch for InnerDispatchForToolset {
    async fn call(
        &self,
        tool_id: echo_agent_tool_protocol::ToolId,
        args: serde_json::Value,
        ctx: echo_agent_tool_runtime::ToolCallContext,
    ) -> echo_agent_tool_runtime::ToolStream<echo_agent_tool_runtime::TypedToolOutput> {
        let result = self
            .toolset
            .call_raw(tool_id.as_str(), args, ctx)
            .await
            .and_then(|output| {
                let value = serde_json::to_value(&output).map_err(|e| {
                    echo_agent_tool_runtime::ToolError::custom("output_encoding", e.to_string())
                })?;
                Ok(echo_agent_tool_runtime::TypedToolOutput::from_value(
                    tool_id.clone(),
                    value,
                ))
            });
        echo_agent_tool_runtime::terminal_only(result)
    }
}
impl FinalizedToolset {
    /// Construct an empty toolset for tests. No tools, no background tasks.
    ///
    /// Safe to call from sync `#[test]` — does not require a tokio runtime.
    pub fn empty_for_test() -> Self {
        Self {
            tools: parking_lot::RwLock::new(vec![]),
            reminders: vec![],
            resources: Arc::new(tokio::sync::Mutex::new(
                crate::types::resources::Resources::default(),
            )),
            resources_persistence: Arc::new(ResourcesPersistence::noop()),
            scheduler_cancel: None,
            local_registry: echo_agent_computer_hub_sdk::LocalRegistry::new(),
            renderer: Arc::new(TemplateRenderer::new(
                std::collections::HashMap::new(),
                std::collections::HashMap::new(),
            )),
            system_reminder_tag: "system-reminder",
            workspace_viewer_ctx: None,
        }
    }
    pub fn local_registry(&self) -> &echo_agent_computer_hub_sdk::LocalRegistry {
        &self.local_registry
    }
    /// Whether the server must await this tool's in-process cancellation cleanup.
    pub fn cooperative_cancellation(&self, tool_name: &str) -> bool {
        {
            let _ = tool_name;
            false
        }
    }
    /// Get all tool definitions to send to the client.
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .read()
            .iter()
            .map(|t| t.definition.clone())
            .collect()
    }
    /// Client-facing name of the (first) enabled tool of `kind`, honoring
    /// `name_override` / preset renames — `None` if no tool of that kind is
    /// enabled. Mirrors `${{ tools.by_kind.<kind> }}` template resolution; used
    /// e.g. to label a background task with its real creator tool name.
    pub fn tool_name_for_kind(&self, kind: ToolKind) -> Option<String> {
        self.renderer.tool_for_kind(kind).map(str::to_owned)
    }
    /// Map of client-facing tool name → snake_case [`ToolKind`] key.
    pub fn tool_kinds(&self) -> HashMap<String, String> {
        self.tools
            .read()
            .iter()
            .map(|t| (t.client_name.clone(), t.metadata.kind().as_key().to_owned()))
            .collect()
    }
    /// Map of client-facing tool name → typed [`ToolKind`].
    ///
    /// Unlike the finalize-request `ToolConfig`s (whose `kind` is `None` when
    /// built from raw IDs over gRPC), the finalized tools always know their
    /// real kind from the registry metadata — use this for kind-derived
    /// metadata in server responses (e.g. capability-mode classification).
    pub fn tool_kind_map(&self) -> HashMap<String, ToolKind> {
        self.tools
            .read()
            .iter()
            .map(|t| (t.client_name.clone(), t.metadata.kind()))
            .collect()
    }
    /// Finalized canonical-to-client parameter names by tool kind.
    pub fn template_param_names(&self) -> HashMap<ToolKind, HashMap<String, String>> {
        self.renderer.param_names()
    }
    pub async fn update_resource<T: Send + Sync + 'static>(&self, resource: T) {
        self.resources.lock().await.insert(resource);
    }
    /// Seed many resources under one lock. The closure runs under the lock; keep it to plain inserts.
    pub async fn update_resources_with(
        &self,
        seed: impl FnOnce(&mut crate::types::resources::Resources),
    ) {
        seed(&mut *self.resources.lock().await);
    }
    /// Clone a typed resource out of this toolset, if present.
    ///
    /// Used to carry session-scoped backends (e.g. the browser service)
    /// across toolset rebuilds so live state survives a hot reload.
    pub async fn get_resource_cloned<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.resources.lock().await.get::<T>().cloned()
    }
    /// Get only built-in tool definitions (exclude MCP tools).
    pub fn tool_definitions_builtins_only(&self) -> Vec<ToolDefinition> {
        self.tools
            .read()
            .iter()
            .filter(|t| !t.client_name.contains("__"))
            .map(|t| t.definition.clone())
            .collect()
    }
    /// Get the resolved contract version for a tool by its client-facing name.
    ///
    /// Returns `None` if the tool is not found or is not version-managed.
    /// Returns an owned `String` because the internal `RwLock` read guard
    /// cannot outlive this call.
    pub fn get_contract_version(&self, tool_name: &str) -> Option<String> {
        self.tools
            .read()
            .iter()
            .find(|t| t.client_name == tool_name)
            .and_then(|t| t.contract_version.clone())
    }
    /// Look up a tool's metadata by its client-facing name. Returns `None` for unknown tools.
    pub fn get_tool_metadata(&self, tool_name: &str) -> Option<Arc<dyn ToolMetadata>> {
        self.tools
            .read()
            .iter()
            .find(|t| t.client_name == tool_name)
            .map(|t| t.metadata.clone())
    }
    /// Resolve canonical [`ToolIdentity`] (kind, namespace, presentation label)
    /// for a tool by its client-facing wire name. Drives the first-party
    /// `echo.agent/*` tool `_meta` contract (tool normalization). Returns `None` for
    /// unknown tools (e.g. uninitialized MCP, backend-only tools).
    pub fn tool_identity(&self, tool_name: &str) -> Option<crate::normalization::ToolIdentity> {
        self.tools
            .read()
            .iter()
            .find(|t| t.client_name == tool_name)
            .map(|t| crate::normalization::tool_identity_of(t.metadata.as_ref()))
    }
    fn tool_not_found_error(tool_name: &str) -> echo_agent_tool_runtime::ToolError {
        let tid = echo_agent_tool_protocol::ToolId::new(tool_name)
            .unwrap_or_else(|_| echo_agent_tool_protocol::ToolId::new("unknown").expect("valid"));
        echo_agent_tool_runtime::ToolError::not_found(tid, format!("Tool not found: {tool_name}"))
    }
    pub async fn try_parse(
        &self,
        tool_name: &str,
        tool_params: &serde_json::Value,
    ) -> Result<ToolInput, echo_agent_tool_runtime::ToolError> {
        let (reverse_params, parse_input) = {
            let tools = self.tools.read();
            let tool = tools
                .iter()
                .find(|t| t.client_name == tool_name)
                .ok_or_else(|| Self::tool_not_found_error(tool_name))?;
            (tool.reverse_params.clone(), tool.parse_input.clone())
        };
        let canonical_params = if reverse_params.is_empty() {
            tool_params.clone()
        } else {
            remap_json_keys(tool_params.clone(), &reverse_params)
        };
        (parse_input)(canonical_params)
    }
    /// Execute a tool, returning only its raw output.
    ///
    /// Unlike [`call()`], this skips reminders and persistence. Used by
    /// `InnerDispatchForToolset` so that `use_tool`'s
    /// dispatch to a target tool does not double-run post-processing: the
    /// outer `call("use_tool")` does one round of post-processing over the
    /// target's output.
    ///
    /// Inner dispatch is intentionally **not** populated in the forwarded
    /// context. MCP tools (the targets of `use_tool`) are passthrough
    /// implementations that never call `use_tool` themselves, so they do
    /// not need inner dispatch.
    ///
    /// The `parent_ctx` carries call-id, cwd, resources, etc. from the
    /// outer call. A fresh child context is built from it — stripping
    /// `InnerDispatch` to prevent recursion.
    async fn call_raw(
        &self,
        tool_name: &str,
        tool_args: serde_json::Value,
        parent_ctx: echo_agent_tool_runtime::ToolCallContext,
    ) -> Result<crate::types::output::ToolOutput, echo_agent_tool_runtime::ToolError> {
        let (registry_id, output_converter, reverse_params, mcp_input_schema) = {
            let tools = self.tools.read();
            let entry = tools
                .iter()
                .find(|t| t.client_name == tool_name)
                .ok_or_else(|| Self::tool_not_found_error(tool_name))?;
            (
                entry.registry_id.clone(),
                entry.output_converter.clone(),
                entry.reverse_params.clone(),
                (entry.namespace == ToolNamespace::MCP.to_string())
                    .then(|| entry.input_schema.clone()),
            )
        };
        if let Some(schema) = mcp_input_schema {
            crate::implementations::use_tool::validate_mcp_arguments(
                tool_name, &schema, &tool_args,
            )?;
        }
        let canonical_params = if reverse_params.is_empty() {
            tool_args
        } else {
            remap_json_keys(tool_args, &reverse_params)
        };
        let mut ctx = echo_agent_tool_runtime::ToolCallContext::new(parent_ctx.call_id.clone());
        ctx.extensions.insert(self.resources.clone());
        ctx.extensions.insert_arc(Arc::clone(&self.renderer));
        if let Some(cancellation) = parent_ctx.get::<echo_agent_tool_runtime::Cancellation>() {
            ctx.extensions.insert((*cancellation).clone());
        }
        ctx.extensions.insert(
            crate::types::resources::InvokingToolParamNames::from_reverse_params(&reverse_params),
        );
        if let Some(cwd) = parent_ctx.extensions.get::<echo_agent_tool_runtime::Cwd>() {
            ctx.extensions.insert((*cwd).clone());
        }
        let tool_id = echo_agent_tool_protocol::ToolId::new(&registry_id)
            .unwrap_or_else(|_| echo_agent_tool_protocol::ToolId::new("unknown").expect("valid"));
        let lr_handle = self.local_registry.find(&tool_id).ok_or_else(|| {
            echo_agent_tool_runtime::ToolError::not_found(
                tool_id,
                format!("Tool not found in LocalRegistry: {registry_id}"),
            )
        })?;
        let stream = lr_handle.execute(ctx, canonical_params).await;
        let value = drain_value_stream(stream).await?;
        (output_converter)(value).map_err(|e| {
            echo_agent_tool_runtime::ToolError::custom("output_decoding", e.to_string())
        })
    }
    /// Dispatch a tool call by client-facing name with client-facing params.
    ///
    /// `cwd_override` — optional per-call working directory. When `Some`, tools
    /// will use this instead of the session `Cwd` from Resources. This is
    /// stack-local (not shared state), so concurrent calls with different
    /// overrides don't race.
    pub async fn call(
        self: &Arc<Self>,
        tool_name: &str,
        tool_args: serde_json::Value,
        tool_call_id: &str,
        cwd_override: Option<std::path::PathBuf>,
    ) -> Result<ToolRunResult, echo_agent_tool_runtime::ToolError> {
        self.call_with_cancellation(tool_name, tool_args, tool_call_id, cwd_override, None)
            .await
    }
    /// Dispatch with cooperative cancellation exposed to the tool.
    pub async fn call_with_cancellation(
        self: &Arc<Self>,
        tool_name: &str,
        tool_args: serde_json::Value,
        tool_call_id: &str,
        cwd_override: Option<std::path::PathBuf>,
        cancellation: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<ToolRunResult, echo_agent_tool_runtime::ToolError> {
        use futures::StreamExt;
        let mut stream = self.call_streaming_with_cancellation(
            tool_name,
            tool_args,
            tool_call_id,
            cwd_override,
            cancellation,
        );
        while let Some(item) = stream.next().await {
            match item {
                echo_agent_tool_runtime::ToolStreamItem::Progress(_) => continue,
                echo_agent_tool_runtime::ToolStreamItem::Terminal(result) => return result,
            }
        }
        Err(stream_no_terminal_error())
    }
    /// Streaming sibling of [`call`].
    ///
    /// Forwards every inner [`ToolStreamItem::Progress`] unchanged, and when the
    /// inner dispatch stream reaches its terminal, runs the shared
    /// post-processing tail ([`finalize_output`]) on it and yields the resulting
    /// [`ToolRunResult`] as the single terminal of the outer stream.
    ///
    /// This is a non-`async` inherent method: it synchronously builds and
    /// returns a `'static` boxed stream. Because [`ToolStream`] is `'static`,
    /// all `.await` and `Arc::clone(self)` happen *inside* the stream block so
    /// nothing borrows `self` across the stream.
    ///
    /// [`ToolStream`]: echo_agent_tool_runtime::ToolStream
    /// [`ToolStreamItem::Progress`]: echo_agent_tool_runtime::ToolStreamItem::Progress
    pub fn call_streaming(
        self: &Arc<Self>,
        tool_name: &str,
        tool_args: serde_json::Value,
        tool_call_id: &str,
        cwd_override: Option<std::path::PathBuf>,
    ) -> echo_agent_tool_runtime::ToolStream<ToolRunResult> {
        self.call_streaming_with_cancellation(
            tool_name,
            tool_args,
            tool_call_id,
            cwd_override,
            None,
        )
    }
    /// Streaming dispatch with cooperative cancellation exposed to the tool.
    pub fn call_streaming_with_cancellation(
        self: &Arc<Self>,
        tool_name: &str,
        tool_args: serde_json::Value,
        tool_call_id: &str,
        cwd_override: Option<std::path::PathBuf>,
        cancellation: Option<tokio_util::sync::CancellationToken>,
    ) -> echo_agent_tool_runtime::ToolStream<ToolRunResult> {
        use futures::StreamExt;
        let this = Arc::clone(self);
        let tool_name = tool_name.to_owned();
        let tool_call_id = tool_call_id.to_owned();
        Box::pin(async_stream::stream! {
            let parts = match this.prepare_dispatch(
                &tool_name,
                tool_args,
                &tool_call_id,
                cwd_override,
                cancellation,
            ) {
                Ok(parts) => parts,
                Err(e) => {
                    yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Err(e));
                    return;
                }
            };
            let DispatchParts {
                lr_handle,
                ctx,
                canonical_params,
                output_converter,
                effective_tool_name,
            } = parts;

            let mut inner = lr_handle.execute(ctx, canonical_params).await;
            while let Some(item) = inner.next().await {
                match item {
                    echo_agent_tool_runtime::ToolStreamItem::Progress(p) => {
                        yield echo_agent_tool_runtime::ToolStreamItem::Progress(p);
                    }
                    echo_agent_tool_runtime::ToolStreamItem::Terminal(Err(e)) => {
                        yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Err(e));
                        return;
                    }
                    echo_agent_tool_runtime::ToolStreamItem::Terminal(Ok(typed)) => {
                        let run_result = this
                            .finalize_output(typed.value, &output_converter, effective_tool_name)
                            .await;
                        yield echo_agent_tool_runtime::ToolStreamItem::Terminal(run_result);
                        return;
                    }
                }
            }
            yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Err(stream_no_terminal_error()));
        })
    }
    /// Pre-dispatch setup shared by [`call`] / [`call_streaming`].
    ///
    /// Acquires the tools read lock, clones the per-tool metadata, remaps the
    /// params, builds the runtime context, and resolves the `LocalRegistry`
    /// handle. Everything needed across `.await` is captured here so the read
    /// guard is dropped before returning.
    fn prepare_dispatch(
        self: &Arc<Self>,
        tool_name: &str,
        tool_args: serde_json::Value,
        tool_call_id: &str,
        cwd_override: Option<std::path::PathBuf>,
        cancellation: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<DispatchParts, echo_agent_tool_runtime::ToolError> {
        let (registry_id, output_converter, reverse_params) = {
            let tools = self.tools.read();
            let entry = tools
                .iter()
                .find(|t| t.client_name == tool_name)
                .ok_or_else(|| Self::tool_not_found_error(tool_name))?;
            (
                entry.registry_id.clone(),
                entry.output_converter.clone(),
                entry.reverse_params.clone(),
            )
        };
        let canonical_params = if reverse_params.is_empty() {
            tool_args
        } else {
            remap_json_keys(tool_args, &reverse_params)
        };
        let effective_tool_name = if tool_name == "use_tool" {
            serde_json::from_value::<crate::implementations::use_tool::UseToolInput>(
                canonical_params.clone(),
            )
            .ok()
            .map(|input| input.tool_name)
        } else {
            None
        };
        let contract_version = self.get_contract_version(tool_name);
        let rt_call_id = echo_agent_tool_protocol::ToolCallId::new(tool_call_id)
            .unwrap_or_else(|_| echo_agent_tool_protocol::ToolCallId::new_v7());
        let mut ctx = echo_agent_tool_runtime::ToolCallContext::new(rt_call_id);
        ctx.extensions.insert(self.resources.clone());
        ctx.extensions.insert_arc(Arc::clone(&self.renderer));
        ctx.extensions.insert(
            crate::types::resources::InvokingToolParamNames::from_reverse_params(&reverse_params),
        );
        if let Some(cwd) = cwd_override {
            ctx.extensions.insert(echo_agent_tool_runtime::Cwd(cwd));
        }
        if let Some(cancellation) = cancellation {
            ctx.extensions
                .insert(echo_agent_tool_runtime::Cancellation(cancellation));
        }
        if let Some(ref version) = contract_version {
            ctx.extensions
                .insert(echo_agent_tool_runtime::BehaviorVersion(version.clone()));
        }
        ctx.extensions.insert(InnerDispatch(std::sync::Arc::new(
            InnerDispatchForToolset {
                toolset: Arc::clone(self),
            },
        )));
        if let Some(wvc) = self.workspace_viewer_ctx.as_ref() {
            ctx.extensions.insert(wvc.clone());
        }
        let tool_id = echo_agent_tool_protocol::ToolId::new(&registry_id)
            .unwrap_or_else(|_| echo_agent_tool_protocol::ToolId::new("unknown").expect("valid"));
        let lr_handle = self.local_registry.find(&tool_id).ok_or_else(|| {
            echo_agent_tool_runtime::ToolError::not_found(
                tool_id,
                format!("Tool not found in LocalRegistry: {registry_id}"),
            )
        })?;
        Ok(DispatchParts {
            lr_handle,
            ctx,
            canonical_params,
            output_converter,
            effective_tool_name,
        })
    }
    /// Post-dispatch tail shared by [`call`] / [`call_streaming`].
    ///
    /// Applies the `output_converter` to the terminal `value`, collects
    /// reminders, renders prompt text, persists resources, and builds the final
    /// [`ToolRunResult`]. This is the single source of truth for terminal-result
    /// construction so the streaming and non-streaming paths can never diverge.
    async fn finalize_output(
        &self,
        value: serde_json::Value,
        output_converter: &OutputConverter,
        effective_tool_name: Option<String>,
    ) -> Result<ToolRunResult, echo_agent_tool_runtime::ToolError> {
        let output = (output_converter)(value).map_err(|e| {
            echo_agent_tool_runtime::ToolError::custom("output_decoding", e.to_string())
        })?;
        let reminders_enabled;
        {
            reminders_enabled = self
                .resources
                .lock()
                .await
                .get::<crate::types::resources::SystemRemindersEnabled>()
                .is_none_or(|e| e.0);
        }
        let reminders = if reminders_enabled {
            let mut r = Vec::new();
            for reminder in &self.reminders {
                let extra = reminder
                    .collect_reminders(self.resources.clone(), &output)
                    .await;
                r.extend(extra);
            }
            r
        } else {
            Vec::new()
        };
        let prompt_text = output.to_prompt_format();
        let prompt_text = crate::reminders::format_with_reminders(
            prompt_text,
            reminders,
            self.system_reminder_tag,
        );
        {
            let res = self.resources.lock().await;
            self.resources_persistence.save(&res);
        }
        Ok(ToolRunResult {
            output,
            prompt_text,
            effective_tool_name,
        })
    }
    /// Reverse-remap client-facing param names to canonical names.
    pub fn remap_params(&self, tool_name: &str, tool_args: serde_json::Value) -> serde_json::Value {
        let reverse_params = {
            let tools = self.tools.read();
            tools
                .iter()
                .find(|t| t.client_name == tool_name)
                .map(|t| t.reverse_params.clone())
                .unwrap_or_default()
        };
        if reverse_params.is_empty() {
            tool_args
        } else {
            remap_json_keys(tool_args, &reverse_params)
        }
    }
    /// Persist resources state to disk.
    pub async fn persist_state(&self) {
        let res = self.resources.lock().await;
        self.resources_persistence.save(&res);
    }
    /// Register a tool at runtime (e.g., MCP tools).
    ///
    /// The tool must implement `echo_agent_tool_runtime::Tool + ToolMetadata`.
    /// MCP tools typically use:
    /// - `type Args = serde_json::Value` (untyped JSON passthrough)
    /// - `kind() -> ToolKind::Other`
    ///
    /// The `name` is used as both the canonical and client-facing name
    /// (no param remapping, no name overrides for dynamic tools).
    ///
    /// `input_schema_override` — when `Some`, this JSON Schema is used as
    /// the tool's input schema instead of deriving one from `T::Args` via
    /// `schemars`. This is required for MCP tools whose schemas come from
    /// the remote server at runtime and cannot be derived from a Rust type.
    ///
    /// Returns an error if a tool with the same name already exists.
    pub fn register_tool<T>(
        &self,
        name: String,
        tool: T,
        input_schema_override: Option<serde_json::Value>,
    ) -> Result<(), echo_agent_tool_runtime::ToolError>
    where
        T: echo_agent_tool_runtime::Tool + ToolMetadata + std::fmt::Debug + Send + Sync + 'static,
        T::Output: serde::Serialize,
    {
        let mut tools = self.tools.write();
        if tools.iter().any(|t| t.client_name == name) {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                format!("Tool already registered: {name}"),
            ));
        }
        let description = tool.description_template().to_string();
        let kind = tool.kind();
        let registry_id = echo_agent_tool_runtime::Tool::id(&tool).as_str().to_owned();
        let input_schema = input_schema_override.unwrap_or_else(generate_schema_cached::<T::Args>);
        let definition = ToolDefinition::function(&name, Some(&description), input_schema.clone());
        self.local_registry.register(tool);
        tools.push(FinalizedTool {
            namespace: ToolNamespace::MCP.to_string(),
            id: name.clone(),
            registry_id,
            client_name: name.clone(),
            metadata: Arc::new(DefaultToolMetadata {
                kind,
                description: description.clone(),
            }),
            output_converter: Arc::new(|value| {
                serde_json::from_value::<ToolOutput>(value.clone()).or_else(|_| match value {
                    serde_json::Value::String(s) => Ok(ToolOutput::Text(s.into())),
                    other => Ok(ToolOutput::Dynamic(other.into())),
                })
            }),
            definition,
            effective_params: serde_json::Value::Object(Default::default()),
            input_schema,
            reverse_params: HashMap::new(),
            parse_input: Arc::new(move |json| {
                Ok(ToolInput::MCPTool(crate::types::tool_io::MCPToolInput {
                    tool_name: name.clone(),
                    tool_input: json,
                }))
            }),
            contract_version: None,
        });
        Ok(())
    }
    pub fn unregister_tools_by_prefix(&self, prefix: &str) -> usize {
        let mut tools = self.tools.write();
        let before = tools.len();
        let to_remove: Vec<_> = tools
            .iter()
            .filter(|t| t.client_name.starts_with(prefix))
            .filter_map(|t| echo_agent_tool_protocol::ToolId::new(&t.registry_id).ok())
            .collect();
        tools.retain(|t| !t.client_name.starts_with(prefix));
        for tid in &to_remove {
            self.local_registry.unregister(tid);
        }
        before - tools.len()
    }
    pub fn unregister_tool_by_name(&self, name: &str) -> bool {
        let mut tools = self.tools.write();
        let tool_id = tools
            .iter()
            .find(|t| t.client_name == name)
            .and_then(|t| echo_agent_tool_protocol::ToolId::new(&t.registry_id).ok());
        let before = tools.len();
        tools.retain(|t| t.client_name != name);
        let removed = tools.len() < before;
        if removed && let Some(tid) = tool_id {
            self.local_registry.unregister(&tid);
        }
        removed
    }
    /// Flush any pending persistence writes. Call on graceful shutdown.
    pub async fn flush_persistence(&self) {
        self.resources_persistence.flush().await;
    }
    /// Serialize current in-memory state, write it to disk, and wait for the write to complete.
    /// Returns where it landed, or `None` for a session that persists nothing.
    ///
    /// Unlike `flush_persistence()`, which only flushes previously queued snapshots, this takes a fresh snapshot first.
    pub async fn save_and_flush_persistence(&self) -> Option<&std::path::Path> {
        {
            let res = self.resources.lock().await;
            self.resources_persistence.save(&res);
        }
        self.resources_persistence.flush().await;
        self.resources_persistence.state_path()
    }
}
/// Process-global memo of generated tool input schemas, keyed by the exact
/// [`std::any::TypeId`] of the schema type.
fn schema_cache() -> &'static RwLock<HashMap<std::any::TypeId, serde_json::Value>> {
    static CACHE: OnceLock<RwLock<HashMap<std::any::TypeId, serde_json::Value>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}
/// Memoized [`generate_schema`], keyed by `TypeId`. Sound as a process-wide cache
/// because the schema depends on `T` alone, not the agent or toolset; the per-boot
/// toolset rebuild would otherwise regenerate identical schemas across a fan-out.
pub(crate) fn generate_schema_cached<T: schemars::JsonSchema + 'static>() -> serde_json::Value {
    let key = std::any::TypeId::of::<T>();
    if let Some(cached) = schema_cache().read().get(&key) {
        return cached.clone();
    }
    #[cfg(test)]
    {
        *schema_uncached_counts().lock().entry(key).or_insert(0) += 1;
    }
    let value = generate_schema::<T>();
    schema_cache().write().insert(key, value.clone());
    value
}
#[cfg(test)]
fn schema_uncached_counts() -> &'static Mutex<HashMap<std::any::TypeId, u64>> {
    static COUNTS: OnceLock<Mutex<HashMap<std::any::TypeId, u64>>> = OnceLock::new();
    COUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}
#[cfg(test)]
fn schema_uncached_calls(key: std::any::TypeId) -> u64 {
    schema_uncached_counts()
        .lock()
        .get(&key)
        .copied()
        .unwrap_or(0)
}
/// JSON Schema for `T` with the root `title` and `description` stripped. Pure
/// and uncached; the per-boot hot path uses [`generate_schema_cached`].
pub fn generate_schema<T: schemars::JsonSchema>() -> serde_json::Value {
    let settings = schemars::generate::SchemaSettings::draft07().with(|s| {
        s.inline_subschemas = true;
    });
    let generator = settings.into_generator();
    let schema = generator.into_root_schema_for::<T>();
    let mut value = serde_json::to_value(&schema).unwrap_or_default();
    if let Some(obj) = value.as_object_mut() {
        obj.remove("title");
        obj.remove("description");
    }
    if let Some(obj) = value.as_object_mut()
        && obj.get("type").and_then(|v| v.as_str()) == Some("object")
    {
        obj.entry("properties")
            .or_insert_with(|| serde_json::Value::Object(Default::default()));
        obj.entry("required")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    }
    value
}
fn merge_json(base: &serde_json::Value, overlay: &serde_json::Value) -> serde_json::Value {
    match (base, overlay) {
        (serde_json::Value::Object(b), serde_json::Value::Object(o)) => {
            let mut m = b.clone();
            for (k, v) in o {
                m.insert(k.clone(), v.clone());
            }
            serde_json::Value::Object(m)
        }
        (_, o) => o.clone(),
    }
}
fn explain_requirement_failure(
    entry: &ToolEntry,
    params: &serde_json::Value,
    proposed: &[ProposedTool<'_>],
) -> RequirementError {
    let fq_tool_id = format!("{}:{}", entry.namespace, entry.id);
    match fq_tool_id.as_str() {
        "EchoAgentBuild:run_terminal_cmd" if params
            .get("enabled_background")
            .and_then(|value| value.as_bool())
            .unwrap_or(true) => {
            let mut missing = vec![];
            if !has_tool_kind(proposed, ToolKind::BackgroundTaskAction) {
                missing.push("EchoAgentBuild:get_task_output");
            }
            if !has_tool_kind(proposed, ToolKind::KillTaskAction) {
                missing.push("EchoAgentBuild:kill_task");
            }
            let message = if missing.is_empty() {
                "unsatisfied requirements".to_string()
            } else {
                format!(
                    "enabled_background=true requires {} so background bash tasks can be observed and cancelled",
                    missing.join(" and ")
                )
            };
            RequirementError::new(fq_tool_id, message)
                .with_field_path("params.enabled_background")
                .with_expected(
                    "set enabled_background=false or include get_task_output and kill_task",
                )
                .with_bad_value(serde_json::Value::Bool(true))
                .with_category("requirements")
        }
        "EchoAgentBuild:task" => {
            let mut missing = vec![];
            if !has_tool_kind(proposed, ToolKind::BackgroundTaskAction) {
                missing.push("EchoAgentBuild:get_task_output");
            }
            if !has_tool_kind(proposed, ToolKind::KillTaskAction) {
                missing.push("EchoAgentBuild:kill_task");
            }
            RequirementError::new(
                    fq_tool_id,
                    format!(
                    "task requires {} so spawned background subagents can be monitored and cancelled",
                    missing.join(" and ")
                ),
                )
                .with_field_path("tools")
                .with_expected("include get_task_output and kill_task")
                .with_category("requirements")
        }
        "EchoAgentBuild:get_task_output" => {
            let has_echo_agent_build_bash = has_tool_with_bool_param(
                proposed,
                "EchoAgentBuild",
                "run_terminal_cmd",
                "enabled_background",
                true,
            );
            let has_echo_agent_build_concise_bash = has_tool_with_bool_param(
                proposed,
                "EchoAgentBuildConcise",
                "run_terminal_cmd",
                "enabled_background",
                true,
            );
            let has_opencode_bash = has_tool(proposed, "OpenCode", "bash");
            let has_task = has_tool(proposed, "EchoAgentBuild", "task");
            let mut notes = vec![];
            if has_tool(proposed, "EchoAgentBuild", "run_terminal_cmd")
                && !has_echo_agent_build_bash
            {
                notes
                    .push(
                        "EchoAgentBuild:run_terminal_cmd is present but enabled_background=false",
                    );
            }
            if has_tool(proposed, "EchoAgentBuildConcise", "run_terminal_cmd")
                && !has_echo_agent_build_concise_bash
            {
                notes
                    .push(
                        "EchoAgentBuildConcise:run_terminal_cmd is present but enabled_background=false",
                    );
            }
            let mut message = "get_task_output requires a background-capable bash tool (EchoAgentBuild:run_terminal_cmd or EchoAgentBuildConcise:run_terminal_cmd with enabled_background=true), OpenCode:bash, or EchoAgentBuild:task"
                .to_string();
            let has_provider = has_echo_agent_build_bash || has_echo_agent_build_concise_bash
                || has_opencode_bash || has_task;
            if !has_provider && !notes.is_empty() {
                message.push_str(&format!("; {}", notes.join("; ")));
            }
            RequirementError::new(fq_tool_id, message)
                .with_field_path("tools")
                .with_expected(
                    "include a background-capable bash tool, OpenCode:bash, or EchoAgentBuild:task",
                )
                .with_category("requirements")
        }
        "EchoAgentBuild:search_replace" if !params
            .get("skip_read_before_edit")
            .and_then(|value| value.as_bool())
            .unwrap_or(false) && !has_tool_kind(proposed, ToolKind::Read) => {
            RequirementError::new(
                    fq_tool_id,
                    "skip_read_before_edit=false requires a Read tool in the toolset so files can be read before editing",
                )
                .with_field_path("params.skip_read_before_edit")
                .with_expected(
                    "set skip_read_before_edit=true or include a Read tool such as EchoAgentBuild:read_file",
                )
                .with_bad_value(serde_json::Value::Bool(false))
                .with_category("requirements")
        }
        "EchoAgentBuild:enter_plan_mode" => {
            RequirementError::new(
                    fq_tool_id,
                    "enter_plan_mode requires EchoAgentBuild:exit_plan_mode so plan mode can always be exited",
                )
                .with_field_path("tools")
                .with_expected("include EchoAgentBuild:exit_plan_mode")
                .with_category("requirements")
        }
        "EchoAgentBuild:exit_plan_mode" => {
            RequirementError::new(
                    fq_tool_id,
                    "exit_plan_mode requires EchoAgentBuild:enter_plan_mode so plan mode can be entered before exiting",
                )
                .with_field_path("tools")
                .with_expected("include EchoAgentBuild:enter_plan_mode")
                .with_category("requirements")
        }
        _ => {
            RequirementError::new(fq_tool_id, "unsatisfied requirements")
                .with_category("requirements")
        }
    }
}
fn has_tool(proposed: &[ProposedTool<'_>], namespace: &str, id: &str) -> bool {
    proposed
        .iter()
        .any(|tool| tool.namespace == namespace && tool.id == id)
}
fn has_tool_kind(proposed: &[ProposedTool<'_>], kind: ToolKind) -> bool {
    proposed.iter().any(|tool| tool.kind == kind)
}
fn has_tool_with_bool_param(
    proposed: &[ProposedTool<'_>],
    namespace: &str,
    id: &str,
    param: &str,
    expected: bool,
) -> bool {
    proposed.iter().any(|tool| {
        tool.namespace == namespace
            && tool.id == id
            && tool.params.get(param).and_then(|value| value.as_bool()) == Some(expected)
    })
}
fn compute_effective_params(
    entry: &ToolEntry,
    tool_config: &ToolConfig,
) -> Result<serde_json::Value, ParamValidationError> {
    match &tool_config.params {
        Some(m) => {
            let client_params = serde_json::Value::Object(m.clone());
            (entry.validate_params)(&client_params)?;
            Ok(merge_json(&entry.default_params, &client_params))
        }
        None => Ok(entry.default_params.clone()),
    }
}
fn requirement_error_from_param_error(
    tool_id: &str,
    err: ParamValidationError,
) -> RequirementError {
    let mut out =
        RequirementError::new(tool_id.to_owned(), err.message).with_category(err.category);
    if let Some(path) = err.field_path {
        out = out.with_field_path(format!("params.{path}"));
    }
    if let Some(expected) = err.expected {
        out = out.with_expected(expected);
    }
    if let Some(bad_value) = err.bad_value {
        out = out.with_bad_value(bad_value);
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    /// Build a `SessionContext` for tests using a temp dir and real local
    /// filesystem/terminal backends.
    fn test_session_context(tmp: &TempDir) -> SessionContext {
        SessionContext {
            backend: Arc::new(crate::computer::local::LocalTerminalBackend::new()),
            fs: Arc::new(crate::computer::local::LocalFs),
            cwd: tmp.path().to_path_buf(),
            session_folder: tmp.path().join("session"),
            session_env: Arc::new(HashMap::new()),
            notification_handle: crate::notification::ToolNotificationHandle::noop(),
            owner_session_id: None,
            subagent: None,
            parent_scheduler_handle: None,
            skills: vec![],
            state_path: tmp.path().join("state.json"),
            memory_backend: None,
            web_search_config: crate::implementations::web_search::WebSearchConfig::default(),
            web_fetch_config:
                crate::implementations::echo_agent_build::web_fetch::WebFetchConfig::default(),
            lsp: None,
            image_gen_config:
                crate::implementations::echo_agent_build::image_gen::ImageGenConfig::default(),
            video_gen_config:
                crate::implementations::echo_agent_build::video_gen::VideoGenConfig::default(),
            app_builder_deployer_config:
                crate::implementations::echo_agent_build::app_builder::AppBuilderDeployerConfig::default(),
            api_key_provider: None,
            auth_provider: None,
            attribution_callback: None,
            system_reminder_tag: crate::reminders::DEFAULT_REMINDER_TAG,
        }
    }
    /// Regression test: `kind_params` must merge input params from ALL tools
    /// that share a `ToolKind`, not just the first one.
    ///
    /// Before the fix, the `kind_params` builder used `if map.is_empty()` to
    /// seed identity param-name mappings only from the **first** tool of each
    /// kind. When `codex:apply_patch` (`ToolKind::Edit`, input: `{ patch }`)
    /// appeared before `echo_agent_build:search_replace` (`ToolKind::Edit`, input:
    /// `{ file_path, old_string, new_string, replace_all }`), the renderer's
    /// context had `params.edit = { "patch": "patch" }` — missing
    /// `replace_all`. At runtime, the template `${{ params.edit.replace_all }}`
    /// failed with "undefined value".
    #[tokio::test]
    async fn kind_params_merged_across_multiple_tools_of_same_kind() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "aaa bbb aaa\n").unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "Codex:apply_patch".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:search_replace".to_string(),
                    params: Some(
                        serde_json::json!({ "skip_read_before_edit": true })
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = Arc::new(
            builder
                .finalize(config, ctx)
                .expect("finalize should succeed — InputParam requirements are satisfied"),
        );
        let result = toolset
            .call(
                "search_replace",
                serde_json::json!({
                    "file_path": "test.txt",
                    "old_string": "aaa",
                    "new_string": "ccc",
                    "replace_all": false,
                }),
                "test-call",
                None,
            )
            .await;
        let result = result.expect(
            "search_replace must not fail with template rendering error \
             when codex:apply_patch appears before it in the config",
        );
        assert!(
            result.prompt_text.contains("replace_all"),
            "Should mention replace_all param in error message: {}",
            result.prompt_text
        );
    }
    /// Verify the exact tool output variants for all template-rendered error
    /// paths in `search_replace` when it is the **sole** Edit tool in the config.
    ///
    /// Exercises two code paths that use `TemplateRenderer` at runtime:
    /// 1. `MultipleMatchesFound` — renders `${{ params.edit.replace_all }}`
    /// 2. `NoMatchesFound` — renders `${{ tools.by_kind.read }}`
    ///
    /// Verify that the rendered `search_replace` description exposed to the model
    /// contains the new minimum-anchor guidance and has no unresolved placeholders.
    #[tokio::test]
    async fn search_replace_description_renders_minimum_anchor_guidance() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuild:read_file".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:search_replace".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let tmp = TempDir::new().unwrap();
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("finalize should succeed");
        let defs = toolset.tool_definitions();
        let sr = defs
            .iter()
            .find(|d| d.function.name == "search_replace")
            .expect("search_replace tool definition not found");
        let desc = sr
            .function
            .description
            .as_deref()
            .expect("description must be present");
        assert!(
            !desc.contains("larger string with more surrounding context"),
            "old guidance encouraging longer blocks must be absent"
        );
        assert!(
            !desc.contains("${{"),
            "rendered description must not contain raw template placeholders"
        );
    }
    /// Smoke test: finalize the full EchoAgentBuild toolset and verify every
    /// tool description is fully rendered -- no unresolved MiniJinja vars,
    /// no stale `{max_*}` placeholders, no empty tool-name references from
    /// missing conditional guards.
    #[tokio::test]
    async fn full_toolset_descriptions_render_cleanly() {
        use crate::implementations::echo_agent_build::{
            IMAGE_GEN_TOOL_NAME, IMAGE_TO_VIDEO_TOOL_NAME, REFERENCE_TO_VIDEO_TOOL_NAME,
            SCHEDULER_CREATE_TOOL_NAME, SCHEDULER_DELETE_TOOL_NAME,
        };
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: [
                "read_file",
                "search_replace",
                "run_terminal_cmd",
                "get_task_output",
                "kill_task",
                "grep",
                "list_dir",
                "ask_user_question",
                "enter_plan_mode",
                "exit_plan_mode",
                "todo_write",
                "task",
                "web_search",
                "web_fetch",
                "lsp",
                IMAGE_GEN_TOOL_NAME,
                IMAGE_TO_VIDEO_TOOL_NAME,
                REFERENCE_TO_VIDEO_TOOL_NAME,
                "monitor",
                SCHEDULER_CREATE_TOOL_NAME,
                SCHEDULER_DELETE_TOOL_NAME,
                "scheduler_list",
            ]
            .into_iter()
            .map(|id| ToolConfig::from_id(format!("EchoAgentBuild:{id}")))
            .chain(std::iter::empty::<ToolConfig>())
            .chain(std::iter::empty::<ToolConfig>())
            .collect(),
            behavior_preset: None,
        };
        let tmp = TempDir::new().unwrap();
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("full toolset should finalize");
        fn assert_no_render_whitespace_artifacts(name: &str, text: &str) {
            let mut in_code_block = false;
            for line in text.lines() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("```") {
                    in_code_block = !in_code_block;
                    continue;
                }
                if in_code_block {
                    continue;
                }
                assert!(
                    !trimmed.contains("  "),
                    "{name}: double space (template render artifact?) in line: {line:?}"
                );
                assert!(
                    !trimmed.contains(" , "),
                    "{name}: stranded space before comma in line: {line:?}"
                );
                if let Some((_, roster)) = trimmed.split_once("access to:") {
                    assert!(
                        roster.starts_with(' '),
                        "{name}: roster lost its separators (stripping guard?) in line: {line:?}"
                    );
                }
            }
        }
        fn collect_descriptions(v: &serde_json::Value, out: &mut Vec<String>) {
            match v {
                serde_json::Value::Object(map) => {
                    if let Some(serde_json::Value::String(d)) = map.get("description") {
                        out.push(d.clone());
                    }
                    for val in map.values() {
                        collect_descriptions(val, out);
                    }
                }
                serde_json::Value::Array(arr) => {
                    for val in arr {
                        collect_descriptions(val, out);
                    }
                }
                _ => {}
            }
        }
        let definitions = toolset.tool_definitions();
        for def in definitions {
            let name = &def.function.name;
            let desc = def.function.description.as_deref().unwrap_or_default();
            assert!(
                !desc.contains("${{"),
                "{name}: unresolved ${{{{...}}}} in description"
            );
            assert!(
                !desc.contains("${%"),
                "{name}: unresolved jinja block in description"
            );
            assert!(
                !desc.contains("{max_"),
                "{name}: unresolved {{max_*}} placeholder"
            );
            assert!(
                !desc.contains("the  tool"),
                "{name}: empty tool name (missing conditional guard)"
            );
            assert_no_render_whitespace_artifacts(name, desc);
            let params_str = def.function.parameters.to_string();
            assert!(
                !params_str.contains("${{"),
                "{name}: unresolved ${{{{...}}}} in a field description"
            );
            assert!(
                !params_str.contains("${%"),
                "{name}: unresolved jinja block in a field description"
            );
            let mut field_descs = Vec::new();
            collect_descriptions(&def.function.parameters, &mut field_descs);
            for field_desc in &field_descs {
                assert_no_render_whitespace_artifacts(name, field_desc);
                assert!(
                    !field_desc.contains("{max_"),
                    "{name}: unresolved {{max_*}} placeholder in a field description"
                );
            }
        }
    }
    /// Bash mode resolves the toolset's execute tool by kind, not a hardcoded
    /// name: `run_terminal_cmd` (echoagent).
    #[tokio::test]
    async fn tool_name_for_kind_resolves_execute() {
        use crate::types::tool::ToolKind;
        let tmp = TempDir::new().unwrap();
        let echoagent = ToolRegistryBuilder::new()
            .finalize(
                ToolServerConfig {
                    tools: vec![
                        ToolConfig::from_id("EchoAgentBuild:run_terminal_cmd".to_string()),
                        ToolConfig::from_id("EchoAgentBuild:get_task_output".to_string()),
                        ToolConfig::from_id("EchoAgentBuild:kill_task".to_string()),
                    ],
                    behavior_preset: None,
                },
                test_session_context(&tmp),
            )
            .expect("echoagent toolset should finalize");
        assert_eq!(
            echoagent.tool_name_for_kind(ToolKind::Execute).as_deref(),
            Some("run_terminal_cmd")
        );
    }
    /// `merge_tool_meta` (the harness emission path) must stamp `echo.agent/tool` for a
    /// known tool while preserving existing markers, and leave meta untouched for
    /// an unknown tool.
    #[tokio::test]
    async fn merge_tool_meta_stamps_known_and_preserves_unknown() {
        use crate::normalization::merge_tool_meta;
        use crate::tool_taxonomy::TOOL_META_KEY;
        use crate::types::tool_io::ToolInput;
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig::from_id("EchoAgentBuild:run_terminal_cmd".to_string()),
                ToolConfig::from_id("EchoAgentBuild:get_task_output".to_string()),
                ToolConfig::from_id("EchoAgentBuild:kill_task".to_string()),
            ],
            behavior_preset: None,
        };
        let tmp = TempDir::new().unwrap();
        let toolset = ToolRegistryBuilder::new()
            .finalize(config, test_session_context(&tmp))
            .expect("toolset should finalize");
        let bash = ToolInput::Bash(crate::implementations::BashToolInput {
            command: "ls".into(),
            timeout: None,
            description: "list files".into(),
            is_background: false,
        });
        let merged = merge_tool_meta(
            &toolset,
            Some(serde_json::json!({"bash_mode": true})),
            "run_terminal_cmd",
            Some(&bash),
        )
        .unwrap();
        assert_eq!(merged["bash_mode"], true);
        assert_eq!(merged[TOOL_META_KEY]["kind"], "execute");
        assert_eq!(merged[TOOL_META_KEY]["input"]["command"], "ls");
        let unchanged = merge_tool_meta(
            &toolset,
            Some(serde_json::json!({"backend": true})),
            "not_a_registered_tool",
            None,
        )
        .unwrap();
        assert_eq!(unchanged["backend"], true);
        assert!(unchanged.get(TOOL_META_KEY).is_none());
    }
    /// The wire (`ToolMetadata::is_read_only`) and doom-loop
    /// (`Tool::capabilities().is_read_only`) must agree for every registered
    /// tool. Drift here is a client classifying a tool differently from
    /// in-process loop detection.
    #[test]
    fn capabilities_is_read_only_matches_metadata() {
        let builder = ToolRegistryBuilder::new();
        let mismatches: Vec<String> = builder
            .tools
            .iter()
            .filter_map(|(name, entry)| {
                let lr = echo_agent_computer_hub_sdk::LocalRegistry::new();
                (entry.register_in_local)(&lr);
                let id = echo_agent_tool_protocol::ToolId::new(&entry.id)
                    .unwrap_or_else(|_| panic!("{name}: invalid tool id {:?}", entry.id));
                let caps = lr
                    .find(&id)
                    .unwrap_or_else(|| panic!("{name}: missing from LocalRegistry"))
                    .capabilities()
                    .is_read_only;
                let meta = entry.metadata.is_read_only();
                (meta != caps).then(|| {
                    format!(
                        "{name}: ToolMetadata::is_read_only()={meta} \
                         Tool::capabilities().is_read_only={caps}"
                    )
                })
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "every registered tool must give the same is_read_only from \
             ToolMetadata and Tool::capabilities(); mismatches:\n{}",
            mismatches.join("\n")
        );
    }
    /// `read_only` must come from the per-tool override, not the kind default:
    /// `get_task_output` is `BackgroundTaskAction` (default mutating) but
    /// overrides to read-only.
    #[tokio::test]
    async fn identity_read_only_honors_per_tool_override() {
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig::from_id("EchoAgentBuild:run_terminal_cmd".to_string()),
                ToolConfig::from_id("EchoAgentBuild:get_task_output".to_string()),
                ToolConfig::from_id("EchoAgentBuild:kill_task".to_string()),
            ],
            behavior_preset: None,
        };
        let tmp = TempDir::new().unwrap();
        let toolset = ToolRegistryBuilder::new()
            .finalize(config, test_session_context(&tmp))
            .expect("toolset should finalize");
        let identity = toolset
            .tool_identity("get_task_output")
            .expect("get_task_output resolves");
        assert!(
            identity.read_only,
            "get_task_output overrides is_read_only to true despite its action kind"
        );
    }
    /// `ToolConfig::kind` parsing: known kinds map, unknown strings sink into
    /// `Other` (with a warn, not an error), absent stays `None`.
    #[test]
    fn tool_config_kind_sinks_unknown_to_other() {
        let parse = |v: serde_json::Value| -> ToolConfig {
            serde_json::from_value(v).expect("ToolConfig deserializes")
        };
        let known = parse(serde_json::json!({"id": "EchoAgentBuild:read_file", "kind": "read"}));
        assert_eq!(known.kind, Some(ToolKind::Read));
        let typo = parse(serde_json::json!({"id": "EchoAgentBuild:read_file", "kind": "raed"}));
        assert_eq!(typo.kind, Some(ToolKind::Other));
        let absent = parse(serde_json::json!({"id": "EchoAgentBuild:read_file"}));
        assert_eq!(absent.kind, None);
    }
    /// End-to-end: a `params_name_overrides` rename of `old_string` must flow
    /// into the per-field descriptions that reference it (`new_string`,
    /// `replace_all`) — not just the property keys.
    #[tokio::test]
    async fn search_replace_field_descriptions_track_param_rename() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                // read_file satisfies search_replace's Read requirement.
                ToolConfig {
                    id: "EchoAgentBuild:read_file".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:search_replace".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: Some(std::collections::HashMap::from([(
                        "old_string".to_string(),
                        "find".to_string(),
                    )])),
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let tmp = TempDir::new().unwrap();
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("finalize should succeed");
        let defs = toolset.tool_definitions();
        let sr = defs
            .iter()
            .find(|d| d.function.name == "search_replace")
            .expect("search_replace tool definition not found");
        let props = sr
            .function
            .parameters
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("schema must have properties");
        assert!(
            props.contains_key("find"),
            "old_string should be renamed to find"
        );
        assert!(!props.contains_key("old_string"));
        let new_desc = props["new_string"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(
            new_desc.contains("find") && !new_desc.contains("old_string"),
            "new_string description should reference the renamed param: {new_desc}"
        );
        let replace_all_desc = props["replace_all"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(
            replace_all_desc.contains("find") && !replace_all_desc.contains("old_string"),
            "replace_all description should reference the renamed param: {replace_all_desc}"
        );
    }
    /// Descriptions promising completion notifications ("You are notified on
    /// completion") render conditionally on the client's system-reminders
    /// setting, plumbed via `set_system_reminders_enabled` into the
    /// `TemplateRenderer`. With reminders disabled, the bash description and
    /// the `is_background` field description must not promise notifications;
    /// they point at the get-output tool instead when one is served.
    #[tokio::test]
    async fn bash_descriptions_track_system_reminders_setting() {
        let config_with = |ids: &[&str]| ToolServerConfig {
            tools: ids
                .iter()
                .map(|id| ToolConfig::from_id((*id).to_string()))
                .collect(),
            behavior_preset: None,
        };
        let bash_texts = |toolset: &FinalizedToolset| {
            let defs = toolset.tool_definitions();
            let bash = defs
                .iter()
                .find(|d| d.function.name == "run_terminal_cmd")
                .expect("run_terminal_cmd definition not found")
                .clone();
            let desc = bash.function.description.clone().unwrap_or_default();
            let field_desc = bash.function.parameters["properties"]["is_background"]["description"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            (desc, field_desc)
        };
        let ids = [
            "EchoAgentBuild:run_terminal_cmd",
            "EchoAgentBuild:get_task_output",
            "EchoAgentBuild:kill_task",
        ];
        let tmp = TempDir::new().unwrap();
        let toolset = ToolRegistryBuilder::new()
            .finalize(config_with(&ids), test_session_context(&tmp))
            .expect("finalize");
        let (desc, field_desc) = bash_texts(&toolset);
        assert!(
            desc.contains("You are notified on completion"),
            "reminders on: description should promise notification: {desc}"
        );
        assert!(
            field_desc.contains("you are notified on completion"),
            "reminders on: is_background description should promise notification: {field_desc}"
        );
        let tmp = TempDir::new().unwrap();
        let mut builder = ToolRegistryBuilder::new();
        builder.set_system_reminders_enabled(false);
        let toolset = builder
            .finalize(config_with(&ids), test_session_context(&tmp))
            .expect("finalize");
        let (desc, field_desc) = bash_texts(&toolset);
        assert!(
            !desc.contains("notified on completion"),
            "reminders off: description must not promise notification: {desc}"
        );
        assert!(
            desc.contains("Check on it later with the get_task_output tool"),
            "reminders off: description should point at get_task_output: {desc}"
        );
        assert!(
            !field_desc.contains("notified on completion")
                && field_desc.contains("check on it later with the get_task_output tool"),
            "reminders off: is_background description should point at get_task_output: {field_desc}"
        );
    }
    /// Each assertion pattern-matches on the exact `ToolOutput::SearchReplace`
    /// variant so the test fails if the renderer silently returns empty strings
    /// or the tool returns the wrong variant.
    #[tokio::test]
    async fn search_replace_only_edit_tool_template_rendering() {
        use crate::types::output::{SearchReplaceOutput, ToolOutput};
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("dup.txt"), "aaa bbb aaa\n").unwrap();
        std::fs::write(tmp.path().join("no_match.txt"), "hello world\n").unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuild:read_file".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:search_replace".to_string(),
                    params: None, // default: skip_read_before_edit = false
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = Arc::new(
            builder
                .finalize(config, ctx)
                .expect("finalize should succeed with read_file + search_replace"),
        );
        for fname in &["dup.txt", "no_match.txt"] {
            toolset
                .call(
                    "read_file",
                    serde_json::json!({ "target_file": *fname }),
                    "read-call",
                    None,
                )
                .await
                .expect("read_file should succeed");
        }
        let result = toolset
            .call(
                "search_replace",
                serde_json::json!({
                    "file_path": "dup.txt",
                    "old_string": "aaa",
                    "new_string": "ccc",
                    "replace_all": false,
                }),
                "call-2",
                None,
            )
            .await
            .expect("call should not return ToolError");
        match &result.output {
            ToolOutput::SearchReplace(SearchReplaceOutput::MultipleMatchesFound(msg)) => {
                assert_eq!(
                    msg,
                    "The string to replace was found multiple times in the file. \
                     Use replace_all to replace all occurrences, \
                     or include more context to only edit one occurrence.",
                );
            }
            other => {
                panic!("Expected SearchReplace(MultipleMatchesFound), got: {other:?}")
            }
        }
        let result = toolset
            .call(
                "search_replace",
                serde_json::json!({
                    "file_path": "no_match.txt",
                    "old_string": "nonexistent_string",
                    "new_string": "replacement",
                }),
                "call-3",
                None,
            )
            .await
            .expect("call should not return ToolError");
        match &result.output {
            ToolOutput::SearchReplace(SearchReplaceOutput::NoMatchesFound(e)) => {
                assert_eq!(
                    e.message,
                    "The string to replace was not found in the file, use the read_file tool to see the correct string. \
                     The user may have changed the file since you last read it.",
                );
            }
            other => panic!("Expected SearchReplace(NoMatchesFound), got: {other:?}"),
        }
    }
    /// Verify EchoAgentBuildConcise tools can be finalized and produce concise output.
    #[tokio::test]
    async fn test_concise_namespace_tools() {
        use crate::types::output::{ReadFileOutput, ToolOutput};
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("hello.txt"), "hello\nworld\n").unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuildConcise:read_file".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuildConcise:search_replace".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuildConcise:run_terminal_cmd".to_string(),
                    params: Some(
                        serde_json::json!({ "enabled_background": true })
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig::for_tool::<echo_agent_build::GrepTool>(),
                ToolConfig::for_tool::<echo_agent_build::KillTaskTool>(),
                ToolConfig::for_tool::<echo_agent_build::TaskOutputTool>(),
                ToolConfig {
                    id: "EchoAgentBuild:list_dir".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = Arc::new(
            builder
                .finalize(config, ctx)
                .expect("finalize should succeed for concise config"),
        );
        let result = toolset
            .call(
                "read_file",
                serde_json::json!({ "target_file": "hello.txt" }),
                "call-concise-1",
                None,
            )
            .await
            .expect("read_file concise should succeed");
        match &result.output {
            ToolOutput::ReadFile(ReadFileOutput::FileContent(fc)) => {
                assert_eq!(fc.content, "1→hello\nworld\n");
            }
            other => panic!("Expected ReadFile(FileContent), got: {other:?}"),
        }
        assert!(
            result.prompt_text.starts_with("1→hello"),
            "Prompt text should start with concise format: {}",
            result.prompt_text
        );
    }
    /// Renaming any of these ids strands live tool configs that pin
    /// fully-qualified registry ids (e.g. production toolset manifests).
    #[test]
    fn has_tool_id_knows_pinned_tool_config_ids() {
        let builder = ToolRegistryBuilder::new();
        for id in [
            "EchoAgentBuild:run_terminal_cmd",
            "EchoAgentBuild:read_file",
            "EchoAgentBuild:search_replace",
            "EchoAgentBuild:list_dir",
            "EchoAgentBuild:grep",
            "EchoAgentBuild:get_terminal_command_output",
            "EchoAgentBuild:kill_terminal_command",
        ] {
            assert!(
                builder.has_tool_id(id),
                "registry must know pinned tool-config id `{id}`"
            );
        }
        assert!(
            !builder.has_tool_id("EchoAgentBuild:does_not_exist"),
            "unknown ids must not be reported as known"
        );
        assert!(
            !builder.has_tool_id("run_terminal_cmd"),
            "lookup is by fully-qualified id, not the bare tool id"
        );
    }
    /// Consumers backfill kinds onto kind-less pinned toolsets (hub
    /// `session.bind` wire entries) from this map before capability
    /// filtering; a wrong or missing kind here silently changes which
    /// tools a `capability_mode` keeps.
    #[test]
    fn known_tool_kinds_maps_pinned_tool_config_ids() {
        let kinds = ToolRegistryBuilder::new().known_tool_kinds();
        for (id, expected) in [
            ("EchoAgentBuild:run_terminal_cmd", ToolKind::Execute),
            ("EchoAgentBuild:read_file", ToolKind::Read),
            ("EchoAgentBuild:search_replace", ToolKind::Edit),
            ("EchoAgentBuild:grep", ToolKind::Search),
            ("EchoAgentBuild:list_dir", ToolKind::List),
        ] {
            assert_eq!(
                kinds.get(id),
                Some(&expected),
                "registry must map `{id}` to {expected:?}"
            );
        }
        assert!(
            !kinds.contains_key("EchoAgentBuild:does_not_exist"),
            "unknown ids must be absent"
        );
    }
    /// Regression test: `validate_config` must reject configurations where
    /// two tools resolve to the same `client_name`.
    ///
    /// Without `name_override`, the client_name defaults to `entry.id`
    /// (e.g. `"read_file"`). If both `EchoAgentBuild:read_file` and
    /// `Codex:read_file` are in the config, both would get
    /// `client_name = "read_file"`, making the second unreachable at
    /// dispatch time.
    #[test]
    fn validate_config_rejects_duplicate_client_name() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuild:read_file".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "Codex:read_file".to_string(),
                    params: None,
                    name_override: None, // both resolve to "read_file"
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert!(!errors.is_empty(), "Should reject duplicate client_name");
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("duplicate client_name")),
            "Error should mention duplicate client_name: {errors:?}",
        );
    }
    #[test]
    fn validate_config_reports_param_field_path_and_value() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:run_terminal_cmd".to_string(),
                params: Some(
                    serde_json::from_value(serde_json::json!({
                        "enabled_background": "yes"
                    }))
                    .unwrap(),
                ),
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert_eq!(errors.len(), 1);
        let error = &errors[0];
        assert_eq!(error.tool, "EchoAgentBuild:run_terminal_cmd");
        assert_eq!(
            error.field_path.as_deref(),
            Some("params.enabled_background")
        );
        assert_eq!(error.category.as_deref(), Some("params_type"));
        assert_eq!(error.bad_value, Some(serde_json::json!("yes")));
    }
    #[test]
    fn validate_config_reports_semantic_hashline_param_error() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuildHashline:hashline_read".to_string(),
                params: Some(
                    serde_json::from_value(serde_json::json!({
                        "hash_len": 0
                    }))
                    .unwrap(),
                ),
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert_eq!(errors.len(), 1);
        let error = &errors[0];
        assert_eq!(error.field_path.as_deref(), Some("params.hash_len"));
        assert_eq!(error.category.as_deref(), Some("params_constraint"));
        assert_eq!(error.expected.as_deref(), Some("1..=4"));
    }
    /// Verify that `name_override` can be used to disambiguate tools that
    /// would otherwise share the same `client_name`.
    #[tokio::test]
    async fn validate_config_allows_name_override_to_disambiguate() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuild:read_file".to_string(),
                    params: None,
                    name_override: None, // client_name = "read_file"
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "Codex:read_file".to_string(),
                    params: None,
                    name_override: Some("codex_read_file".to_string()), // disambiguated
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert!(
            errors.is_empty(),
            "Should accept disambiguated names: {errors:?}"
        );
        let ctx = test_session_context(&tmp);
        let _toolset = builder
            .finalize(config, ctx)
            .expect("finalize should succeed with disambiguated names");
    }
    /// Verify that `finalize()` propagates the duplicate `client_name` error
    /// from `validate_config()`.
    #[tokio::test]
    async fn finalize_rejects_duplicate_client_name() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuild:read_file".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "Codex:read_file".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let result = builder.finalize(config, ctx);
        let errors = match result {
            Err(e) => e,
            Ok(_) => panic!("finalize should fail for duplicate client_name"),
        };
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("duplicate client_name")),
            "finalize error should mention duplicate client_name: {errors:?}",
        );
    }
    #[derive(Debug)]
    struct FakeMcpTool {
        description: String,
    }
    impl crate::types::tool_metadata::ToolMetadata for FakeMcpTool {
        fn kind(&self) -> ToolKind {
            ToolKind::Other
        }
        fn tool_namespace(&self) -> crate::types::tool::ToolNamespace {
            crate::types::tool::ToolNamespace::MCP
        }
        fn description_template(&self) -> &str {
            &self.description
        }
    }
    impl echo_agent_tool_runtime::Tool for FakeMcpTool {
        type Args = serde_json::Value;
        type Output = String;
        fn id(&self) -> echo_agent_tool_protocol::ToolId {
            echo_agent_tool_protocol::ToolId::new("fake_mcp").expect("valid")
        }
        fn description(
            &self,
            _ctx: &::echo_agent_tool_runtime::ListToolsContext,
        ) -> echo_agent_tool_types::ToolDescription {
            echo_agent_tool_types::ToolDescription::new("fake_mcp", &self.description)
        }
        async fn run(
            &self,
            _ctx: echo_agent_tool_runtime::ToolCallContext,
            _input: serde_json::Value,
        ) -> Result<String, echo_agent_tool_runtime::ToolError> {
            Ok("ok".into())
        }
    }
    #[tokio::test]
    async fn call_sets_effective_tool_name_for_use_tool_dispatch() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig::for_tool::<
                crate::implementations::use_tool::UseTool,
            >()],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = Arc::new(builder.finalize(config, ctx).unwrap());
        toolset
            .register_tool(
                "linear__save_issue".to_string(),
                FakeMcpTool {
                    description: "Create or update a Linear issue".into(),
                },
                Some(serde_json::json!({
                    "type": "object",
                    "required": ["title"],
                    "properties": {"title": {"type": "string"}},
                })),
            )
            .unwrap();
        let invalid = toolset
            .call(
                "use_tool",
                serde_json::json!({
                    "tool_name": "linear__save_issue",
                    "tool_input": {"title": 42}
                }),
                "call-invalid",
                None,
            )
            .await;
        let Err(error) = invalid else {
            panic!("use_tool accepted a numeric title");
        };
        assert_eq!(
            error.kind,
            echo_agent_tool_runtime::ToolErrorKind::InvalidArguments
        );
        assert!(error.detail.contains("tool_input/title"), "{error}");
        assert!(
            error.detail.contains(r#"a number is not of type "string""#),
            "{error}"
        );
        let result = toolset
            .call(
                "use_tool",
                serde_json::json!({
                    "tool_name": "linear__save_issue",
                    "tool_input": {"title": "hello"}
                }),
                "call-1",
                None,
            )
            .await
            .expect("use_tool should dispatch to the dynamic MCP tool");
        assert_eq!(
            result.effective_tool_name.as_deref(),
            Some("linear__save_issue")
        );
        assert!(matches!(
            result.output,
            crate::types::output::ToolOutput::Text(_)
        ));
    }
    /// A blocking stub tool (only implements `Tool::run`) used to verify the
    /// non-streaming `call` path stays byte-identical after the streaming
    /// refactor.
    #[derive(Debug)]
    struct NonStreamingStub;
    impl crate::types::tool_metadata::ToolMetadata for NonStreamingStub {
        fn kind(&self) -> ToolKind {
            ToolKind::Other
        }
        fn tool_namespace(&self) -> crate::types::tool::ToolNamespace {
            crate::types::tool::ToolNamespace::MCP
        }
        fn description_template(&self) -> &str {
            "non-streaming stub"
        }
    }
    impl echo_agent_tool_runtime::Tool for NonStreamingStub {
        type Args = serde_json::Value;
        type Output = String;
        fn id(&self) -> echo_agent_tool_protocol::ToolId {
            echo_agent_tool_protocol::ToolId::new("non_streaming_stub").expect("valid")
        }
        fn description(
            &self,
            _ctx: &::echo_agent_tool_runtime::ListToolsContext,
        ) -> echo_agent_tool_types::ToolDescription {
            echo_agent_tool_types::ToolDescription::new("non_streaming_stub", "non-streaming stub")
        }
        async fn run(
            &self,
            _ctx: echo_agent_tool_runtime::ToolCallContext,
            _input: serde_json::Value,
        ) -> Result<String, echo_agent_tool_runtime::ToolError> {
            Ok("stub-output".into())
        }
    }
    /// A streaming stub tool that emits one `Progress` item then a `Terminal`,
    /// used to verify `call_streaming` forwards progress and finalizes the
    /// terminal, while `call` silently drops progress.
    #[derive(Debug)]
    struct StreamingStub;
    impl crate::types::tool_metadata::ToolMetadata for StreamingStub {
        fn kind(&self) -> ToolKind {
            ToolKind::Other
        }
        fn tool_namespace(&self) -> crate::types::tool::ToolNamespace {
            crate::types::tool::ToolNamespace::MCP
        }
        fn description_template(&self) -> &str {
            "streaming stub"
        }
    }
    impl echo_agent_tool_runtime::Tool for StreamingStub {
        type Args = serde_json::Value;
        type Output = String;
        fn id(&self) -> echo_agent_tool_protocol::ToolId {
            echo_agent_tool_protocol::ToolId::new("streaming_stub").expect("valid")
        }
        fn description(
            &self,
            _ctx: &::echo_agent_tool_runtime::ListToolsContext,
        ) -> echo_agent_tool_types::ToolDescription {
            echo_agent_tool_types::ToolDescription::new("streaming_stub", "streaming stub")
        }
        async fn run(
            &self,
            _ctx: echo_agent_tool_runtime::ToolCallContext,
            _input: serde_json::Value,
        ) -> Result<String, echo_agent_tool_runtime::ToolError> {
            Ok("terminal-value".into())
        }
        async fn execute(
            &self,
            _ctx: echo_agent_tool_runtime::ToolCallContext,
            _input: serde_json::Value,
        ) -> echo_agent_tool_runtime::ToolStream<String> {
            Box::pin(futures::stream::iter(vec![
                echo_agent_tool_runtime::ToolStreamItem::Progress(
                    echo_agent_tool_runtime::ToolProgress::Text {
                        text: "progress-1".into(),
                    },
                ),
                echo_agent_tool_runtime::ToolStreamItem::Terminal(Ok("terminal-value".to_string())),
            ]))
        }
    }
    /// Back-compat: a non-streaming stub tool driven through `call` yields a
    /// `ToolRunResult` whose `prompt_text` (with reminders applied) is intact.
    #[tokio::test]
    async fn call_non_streaming_stub_back_compat() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig::for_tool::<echo_agent_build::ReadFileTool>()],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = Arc::new(builder.finalize(config, ctx).unwrap());
        toolset
            .register_tool(
                "stub".to_string(),
                NonStreamingStub,
                Some(serde_json::json!({"type": "object", "properties": {}})),
            )
            .unwrap();
        let result = toolset
            .call("stub", serde_json::json!({}), "call-a", None)
            .await
            .expect("call should succeed for the non-streaming stub");
        assert!(
            result.prompt_text.contains("stub-output"),
            "prompt_text should carry the rendered output: {}",
            result.prompt_text
        );
        assert!(result.effective_tool_name.is_none());
    }
    /// `call_streaming` forwards the inner `Progress` item(s) in order and then
    /// yields exactly one `Terminal` whose `ToolRunResult` still carries
    /// `prompt_text`. Driving the same tool through `call` drops the progress
    /// and produces a result of the same shape.
    #[tokio::test]
    async fn call_streaming_forwards_progress_and_finalizes_terminal() {
        use futures::StreamExt;
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig::for_tool::<echo_agent_build::ReadFileTool>()],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = Arc::new(builder.finalize(config, ctx).unwrap());
        toolset
            .register_tool(
                "streamer".to_string(),
                StreamingStub,
                Some(serde_json::json!({"type": "object", "properties": {}})),
            )
            .unwrap();
        let mut stream = toolset.call_streaming("streamer", serde_json::json!({}), "call-b", None);
        let mut progress_count = 0;
        let mut terminal: Option<ToolRunResult> = None;
        while let Some(item) = stream.next().await {
            match item {
                echo_agent_tool_runtime::ToolStreamItem::Progress(p) => {
                    assert!(
                        terminal.is_none(),
                        "progress must arrive before the terminal"
                    );
                    assert!(matches!(
                        p,
                        echo_agent_tool_runtime::ToolProgress::Text { .. }
                    ));
                    progress_count += 1;
                }
                echo_agent_tool_runtime::ToolStreamItem::Terminal(result) => {
                    assert!(terminal.is_none(), "exactly one terminal");
                    terminal = Some(result.expect("terminal should be Ok"));
                }
            }
        }
        assert_eq!(progress_count, 1, "the single progress item is forwarded");
        let streamed = terminal.expect("a terminal must be produced");
        assert!(
            streamed.prompt_text.contains("terminal-value"),
            "terminal prompt_text should carry the rendered output: {}",
            streamed.prompt_text
        );
        let via_call = toolset
            .call("streamer", serde_json::json!({}), "call-b2", None)
            .await
            .expect("call should succeed and drain progress silently");
        assert_eq!(via_call.prompt_text, streamed.prompt_text);
        assert_eq!(via_call.effective_tool_name, streamed.effective_tool_name);
    }
    /// A misbehaving tool whose `execute` returns an *empty* stream — no
    /// `Progress`, no `Terminal`. This breaks the `Tool` streaming contract;
    /// the registry's drain in `call` must surface the violation as a
    /// `stream_no_terminal` error rather than panicking. Defends against a
    /// single buggy tool implementation tearing down the workspace process.
    #[derive(Debug)]
    struct NoTerminalStub;
    impl crate::types::tool_metadata::ToolMetadata for NoTerminalStub {
        fn kind(&self) -> ToolKind {
            ToolKind::Other
        }
        fn tool_namespace(&self) -> crate::types::tool::ToolNamespace {
            crate::types::tool::ToolNamespace::MCP
        }
        fn description_template(&self) -> &str {
            "no-terminal stub"
        }
    }
    impl echo_agent_tool_runtime::Tool for NoTerminalStub {
        type Args = serde_json::Value;
        type Output = String;
        fn id(&self) -> echo_agent_tool_protocol::ToolId {
            echo_agent_tool_protocol::ToolId::new("no_terminal_stub").expect("valid")
        }
        fn description(
            &self,
            _ctx: &::echo_agent_tool_runtime::ListToolsContext,
        ) -> echo_agent_tool_types::ToolDescription {
            echo_agent_tool_types::ToolDescription::new("no_terminal_stub", "no-terminal stub")
        }
        async fn run(
            &self,
            _ctx: echo_agent_tool_runtime::ToolCallContext,
            _input: serde_json::Value,
        ) -> Result<String, echo_agent_tool_runtime::ToolError> {
            Ok("unused".into())
        }
        async fn execute(
            &self,
            _ctx: echo_agent_tool_runtime::ToolCallContext,
            _input: serde_json::Value,
        ) -> echo_agent_tool_runtime::ToolStream<String> {
            Box::pin(futures::stream::empty())
        }
    }
    /// End-to-end "no panic on misbehaving tool" guard: a tool whose `execute`
    /// emits an empty stream (zero `Progress`, zero `Terminal`) must surface
    /// through `call` as `Err(stream_no_terminal)` — never a panic.
    ///
    /// Behaviorally, two layers cooperate to deliver this:
    ///   1. `call_streaming`'s own fallback yields
    ///      `Terminal(Err(stream_no_terminal_error()))` when its inner
    ///      dispatch stream ends without a terminal (the path actually
    ///      exercised by this stub).
    ///   2. `call`'s drain loop now also returns `Err(stream_no_terminal_error())`
    ///      instead of `unreachable!()` if the outer `call_streaming` stream
    ///      itself ever ends without yielding a terminal — defense-in-depth
    ///      that cannot be reached under the `call_streaming` contract but
    ///      whose presence guarantees no `unreachable!()` panic at this
    ///      callsite.
    ///
    /// Both layers raise the same `stream_no_terminal` error kind so consumers
    /// see one consistent shape regardless of which layer caught the
    /// violation.
    #[tokio::test]
    async fn call_returns_error_when_inner_stream_has_no_terminal() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig::for_tool::<echo_agent_build::ReadFileTool>()],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = Arc::new(builder.finalize(config, ctx).unwrap());
        toolset
            .register_tool(
                "no_terminal".to_string(),
                NoTerminalStub,
                Some(serde_json::json!({"type": "object", "properties": {}})),
            )
            .unwrap();
        let err = toolset
            .call("no_terminal", serde_json::json!({}), "call-no-term", None)
            .await
            .expect_err("empty inner stream must produce an Err, not panic");
        let msg = err.to_string();
        assert!(
            msg.contains("stream_no_terminal") || msg.contains("without a terminal"),
            "error must surface the no-terminal violation, got: {msg}"
        );
    }
    #[tokio::test]
    async fn non_pi_finalized_contract_snapshot_is_unchanged() {
        let tmp = TempDir::new().unwrap();
        let toolset = ToolRegistryBuilder::new()
            .finalize(
                ToolServerConfig {
                    tools: vec![
                        ToolConfig::for_tool::<echo_agent_build::TodoWriteTool>(),
                        ToolConfig::for_tool::<opencode::OpenCodeWriteTool>(),
                    ],
                    behavior_preset: None,
                },
                test_session_context(&tmp),
            )
            .unwrap();
        let mut contracts: Vec<serde_json::Value> = toolset
            .tool_definitions()
            .into_iter()
            .map(|definition| {
                serde_json::json!({
                    "name": definition.function.name,
                    "description": definition.function.description,
                    "parameters": definition.function.parameters,
                })
            })
            .collect();
        contracts.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let expected: serde_json::Value = serde_json::from_str(
                r##"
        [
          {
            "name": "todo_write",
            "description": "Create and manage a structured task list. The user sees this list live — it is your primary way to show progress.\n\nUse for any task with 3+ steps. Skip for trivial single-step work.",
            "parameters": {
              "$schema": "http://json-schema.org/draft-07/schema#",
              "required": [
                "todos"
              ],
              "type": "object",
              "properties": {
                "merge": {
                  "description": "Optional. When true (default), merges the provided todos into the existing list by id — send only the items you are changing, and to flip status without changing content send just id + status. When false, the provided todos replace the existing list.",
                  "type": "boolean",
                  "default": true
                },
                "todos": {
                  "description": "Array of todo items to write to the workspace",
                  "type": "array",
                  "items": {
                    "type": "object",
                    "properties": {
                      "id": {
                        "description": "Unique identifier for the todo item",
                        "type": "string"
                      },
                      "content": {
                        "description": "The description/content of the todo item",
                        "type": [
                          "string",
                          "null"
                        ]
                      },
                      "status": {
                        "description": "The status of the todo item: pending, in_progress, completed, or cancelled",
                        "type": [
                          "string",
                          "null"
                        ],
                        "enum": [
                          "pending",
                          "in_progress",
                          "completed",
                          "cancelled",
                          null
                        ]
                      }
                    },
                    "required": [
                      "id"
                    ]
                  }
                }
              }
            }
          },
          {
            "name": "write",
            "description": "Create or overwrite a file.\n\n- Writing to an existing path replaces the file.\n- Parent directories are created for you.",
            "parameters": {
              "$schema": "http://json-schema.org/draft-07/schema#",
              "required": [
                "file_path",
                "content"
              ],
              "properties": {
                "file_path": {
                  "description": "The absolute path to the file to write.",
                  "type": "string"
                },
                "content": {
                  "description": "The full file content to write.",
                  "type": "string"
                }
              },
              "type": "object"
            }
          }
        ]
"##,
            )
            .expect("checked-in snapshot parses");
        assert_eq!(expected, serde_json::Value::Array(contracts));
    }
    #[tokio::test]
    async fn tool_definitions_builtins_only_hides_mcp_tools() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig::for_tool::<echo_agent_build::ReadFileTool>(),
                ToolConfig::for_tool::<echo_agent_build::GrepTool>(),
            ],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = builder.finalize(config, ctx).unwrap();
        toolset
            .register_tool(
                "linear__save_issue".to_string(),
                FakeMcpTool {
                    description: "Create or update a Linear issue".into(),
                },
                Some(serde_json::json!({"type": "object", "properties": {}})),
            )
            .unwrap();
        assert_eq!(toolset.tool_definitions().len(), 3);
        let builtins = toolset.tool_definitions_builtins_only();
        assert_eq!(builtins.len(), 2);
        for def in &builtins {
            assert!(
                !def.function.name.contains("__"),
                "MCP tool {} should be hidden",
                def.function.name
            );
        }
    }
    /// `task` tool must be rejected when neither `get_task_output` nor
    /// `kill_task` are present in the toolset.
    #[test]
    fn task_tool_rejected_without_get_task_output_and_kill_task() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:task".to_string(),
                params: None,
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert!(
            !errors.is_empty(),
            "task tool should be rejected without get_task_output and kill_task"
        );
        assert!(
            errors.iter().any(|e| e.tool == "EchoAgentBuild:task"
                && e.message.contains("EchoAgentBuild:get_task_output")
                && e.message.contains("EchoAgentBuild:kill_task")),
            "error should mention missing background task tools: {errors:?}",
        );
    }
    /// `task` tool must be rejected when only `get_task_output` is present
    /// (missing `kill_task`).
    #[test]
    fn task_tool_rejected_with_only_get_task_output() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuild:task".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:get_task_output".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert!(
            errors.iter().any(|e| e.tool == "EchoAgentBuild:task"
                && e.message.contains("EchoAgentBuild:kill_task")),
            "task tool should be rejected without kill_task: {errors:?}",
        );
    }
    /// `task` tool must be rejected when only `kill_task` is present
    /// (missing `get_task_output`).
    #[test]
    fn task_tool_rejected_with_only_kill_task() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuild:task".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:kill_task".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert!(
            errors.iter().any(|e| e.tool == "EchoAgentBuild:task"
                && e.message.contains("EchoAgentBuild:get_task_output")),
            "task tool should be rejected without get_task_output: {errors:?}",
        );
    }
    /// `task` tool must be accepted when both `get_task_output` and
    /// `kill_task` are present in the toolset.
    #[tokio::test]
    async fn task_tool_accepted_with_both_get_task_output_and_kill_task() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuild:task".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:get_task_output".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:kill_task".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert!(
            errors.is_empty(),
            "task tool should be accepted with get_task_output and kill_task: {errors:?}"
        );
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("finalize should succeed with task + get_task_output + kill_task");
        let defs = toolset.tool_definitions();
        let task_def = defs.iter().find(|d| d.function.name == "task");
        assert!(task_def.is_some(), "task tool should be in definitions");
    }
    /// Verify that the task tool description renders correctly with the default
    /// echo-agent-build agent config (all tools present) and that the new examples
    /// section is included with no unresolved template placeholders.
    #[tokio::test]
    async fn bash_definition_hides_is_background_when_disabled() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:run_terminal_cmd".to_string(),
                params: Some(
                    serde_json::json!({ "enabled_background": false })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let tmp = TempDir::new().unwrap();
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("finalize should succeed with disabled background");
        let defs = toolset.tool_definitions();
        let bash_def = defs
            .iter()
            .find(|d| d.function.name == "run_terminal_cmd")
            .expect("bash tool definition not found");
        let properties = bash_def
            .function
            .parameters
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("bash schema must have properties");
        assert!(
            !properties.contains_key("background"),
            "disabled background should hide is_background from exported schema"
        );
        let desc = bash_def
            .function
            .description
            .as_deref()
            .expect("description must be present");
        assert!(
            !desc.contains("background"),
            "disabled background should remove is_background guidance from default description"
        );
    }
    #[tokio::test]
    async fn bash_definition_preserves_is_background_when_enabled() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuild:run_terminal_cmd".to_string(),
                    params: Some(
                        serde_json::json!({ "enabled_background": true })
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:get_task_output".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:kill_task".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let tmp = TempDir::new().unwrap();
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("finalize should succeed with enabled background");
        let defs = toolset.tool_definitions();
        let bash_def = defs
            .iter()
            .find(|d| d.function.name == "run_terminal_cmd")
            .expect("bash tool definition not found");
        let properties = bash_def
            .function
            .parameters
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("bash schema must have properties");
        assert!(
            properties.contains_key("is_background"),
            "enabled background should preserve is_background in exported schema"
        );
        let desc = bash_def
            .function
            .description
            .as_deref()
            .expect("description must be present");
        assert!(
            desc.contains("background"),
            "enabled background should preserve is_background guidance in default description"
        );
    }
    /// Regression guard: background-param template references must use the real
    /// input-schema property names — `${{ params.execute.is_background }}` and
    /// `${{ params.task.run_in_background }}`. A mistyped key (e.g. the old
    /// `params.execute.background`) has no entry in `kind_params`, so the
    /// renderer silently emits "" — producing prompt text like "set =true" or
    /// "=true commands". This finalizes the full background-capable toolset and
    /// asserts no rendered description leaks a blank param, and that the
    /// cross-tool refs resolve to their canonical keys.
    #[tokio::test]
    async fn background_param_templates_reference_real_schema_keys() {
        let builder = ToolRegistryBuilder::new();
        let tool = |id: &str| ToolConfig {
            id: id.to_string(),
            params: None,
            name_override: None,
            params_name_overrides: None,
            description_override: None,
            behavior_version: None,
            kind: None,
        };
        let config = ToolServerConfig {
            tools: vec![
                tool("EchoAgentBuild:run_terminal_cmd"),
                tool("EchoAgentBuild:task"),
                tool("EchoAgentBuild:get_task_output"),
                tool("EchoAgentBuild:wait_tasks"),
                tool("EchoAgentBuild:kill_task"),
            ],
            behavior_preset: None,
        };
        let tmp = TempDir::new().unwrap();
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("finalize should succeed");
        let defs = toolset.tool_definitions();
        for d in &defs {
            let desc = d.function.description.as_deref().unwrap_or("");
            assert!(
                !desc.contains(" =true"),
                "tool `{}` renders a blank background param (mistyped `params.*` key?):\n{desc}",
                d.function.name
            );
            assert!(
                !desc.contains("${{ params"),
                "tool `{}` has an unrendered param template:\n{desc}",
                d.function.name
            );
        }
        let desc_of = |name: &str| -> String {
            defs.iter()
                .find(|d| d.function.name == name)
                .and_then(|d| d.function.description.clone())
                .unwrap_or_default()
        };
        assert!(
            desc_of("run_terminal_cmd").contains("is_background"),
            "bash description must resolve params.execute.is_background"
        );
        for name in ["get_task_output", "wait_tasks"] {
            let desc = desc_of(name);
            assert!(
                desc.contains("is_background"),
                "`{name}` description must resolve params.execute.is_background"
            );
            assert!(
                desc.contains("run_in_background"),
                "`{name}` description must resolve params.task.run_in_background"
            );
            let timeout = defs
                .iter()
                .find(|d| d.function.name == name)
                .map(|d| &d.function.parameters["properties"]["timeout_ms"])
                .unwrap_or_else(|| panic!("`{name}` should expose timeout_ms"));
            assert!(
                timeout.get("maximum").is_some(),
                "`{name}`.timeout_ms must carry the resolved wait ceiling: {timeout}"
            );
            assert!(
                !timeout["description"]
                    .as_str()
                    .unwrap_or("")
                    .contains("{max_"),
                "`{name}`.timeout_ms has an unresolved placeholder: {timeout}"
            );
        }
    }
    #[test]
    fn validate_config_rejects_auto_bg_without_background_enabled() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:run_terminal_cmd".to_string(),
                params: Some(
                    serde_json::json!({ "enabled_background": false, "auto_background_on_timeout": true })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert_eq!(errors.len(), 1, "expected exactly one validation error");
        let error = &errors[0];
        assert_eq!(
            error.field_path.as_deref(),
            Some("params.auto_background_on_timeout")
        );
        assert_eq!(error.category.as_deref(), Some("params_constraint"));
        assert!(
            error
                .message
                .contains("auto_background_on_timeout requires enabled_background"),
            "error message should explain the invariant, got: {}",
            error.message
        );
    }
    #[tokio::test]
    async fn bash_definition_preserves_override_and_hides_is_background_when_disabled() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:run_terminal_cmd".to_string(),
                params: Some(
                    serde_json::json!({ "enabled_background": false, "auto_background_on_timeout": false })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                name_override: None,
                params_name_overrides: None,
                description_override: Some("custom bash description".to_string()),
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let tmp = TempDir::new().unwrap();
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("finalize should succeed with description override");
        let defs = toolset.tool_definitions();
        let bash_def = defs
            .iter()
            .find(|d| d.function.name == "run_terminal_cmd")
            .expect("bash tool definition not found");
        let properties = bash_def
            .function
            .parameters
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("bash schema must have properties");
        assert!(
            !properties.contains_key("background"),
            "disabled background should hide is_background from exported schema even with override"
        );
        assert_eq!(
            bash_def.function.description.as_deref(),
            Some("custom bash description")
        );
    }
    #[test]
    fn bash_with_disabled_background_does_not_require_task_tools() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:run_terminal_cmd".to_string(),
                params: Some(
                    serde_json::json!({ "enabled_background": false, "auto_background_on_timeout": false })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert!(
            errors.is_empty(),
            "bash with disabled background should not require get_task_output/kill_task: {errors:?}"
        );
    }
    #[test]
    fn bash_with_enabled_background_reports_missing_task_tools() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:run_terminal_cmd".to_string(),
                params: None,
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert_eq!(
            errors.len(),
            1,
            "expected one bash requirement error: {errors:?}"
        );
        let error = &errors[0];
        assert_eq!(error.tool, "EchoAgentBuild:run_terminal_cmd");
        assert_eq!(error.category.as_deref(), Some("requirements"));
        assert_eq!(
            error.field_path.as_deref(),
            Some("params.enabled_background")
        );
        assert_eq!(error.bad_value, Some(serde_json::json!(true)));
        assert!(error.message.contains("EchoAgentBuild:get_task_output"));
        assert!(error.message.contains("EchoAgentBuild:kill_task"));
        assert!(
            error
                .expected
                .as_deref()
                .unwrap_or_default()
                .contains("enabled_background=false")
        );
    }
    #[test]
    fn task_requirement_error_lists_missing_background_tools() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:task".to_string(),
                params: None,
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert_eq!(
            errors.len(),
            1,
            "expected one task requirement error: {errors:?}"
        );
        let error = &errors[0];
        assert_eq!(error.tool, "EchoAgentBuild:task");
        assert!(error.message.contains("EchoAgentBuild:get_task_output"));
        assert!(error.message.contains("EchoAgentBuild:kill_task"));
        assert_eq!(error.field_path.as_deref(), Some("tools"));
    }
    #[test]
    fn get_task_output_requirement_error_mentions_supported_providers() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:get_task_output".to_string(),
                params: None,
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert_eq!(
            errors.len(),
            1,
            "expected one get_task_output requirement error: {errors:?}"
        );
        let error = &errors[0];
        assert_eq!(error.tool, "EchoAgentBuild:get_task_output");
        assert!(error.message.contains("background-capable bash tool"));
        assert!(error.message.contains("OpenCode:bash"));
        assert!(error.message.contains("EchoAgentBuild:task"));
    }
    #[test]
    fn search_replace_requirement_error_mentions_read_tool() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:search_replace".to_string(),
                params: None,
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert!(
            !errors.is_empty(),
            "expected search_replace requirement error"
        );
        let error = errors
            .iter()
            .find(|error| error.tool == "EchoAgentBuild:search_replace")
            .expect("search_replace error should be present");
        assert!(error.message.contains("Read tool"));
        assert_eq!(
            error.field_path.as_deref(),
            Some("params.skip_read_before_edit")
        );
    }
    /// AskUserQuestion validates without the plan-mode tools, matching
    /// the reference agent (its plan-mode prompt note is `${% if %}`-guarded).
    #[test]
    fn ask_user_question_validates_standalone() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:ask_user_question".to_string(),
                params: None,
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert!(
            errors.is_empty(),
            "ask_user_question should validate without plan-mode tools: {errors:?}"
        );
    }
    #[tokio::test]
    async fn task_description_renders_with_default_agent_config() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuild:run_terminal_cmd".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:read_file".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:search_replace".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:list_dir".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:grep".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:web_search".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:task".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:get_task_output".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig {
                    id: "EchoAgentBuild:kill_task".to_string(),
                    params: None,
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
            ],
            behavior_preset: None,
        };
        let tmp = TempDir::new().unwrap();
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("finalize should succeed with default echo-agent-build tools");
        let defs = toolset.tool_definitions();
        let task_def = defs
            .iter()
            .find(|d| d.function.name == "task")
            .expect("task tool definition not found");
        let desc = task_def
            .function
            .description
            .as_deref()
            .expect("description must be present");
        assert!(
            !desc.contains("${{"),
            "rendered description must not contain raw template placeholders, got:\n{desc}"
        );
        assert!(!desc.is_empty(), "task tool description must be non-empty");
    }
    fn hashline_tool_config(id: &str) -> ToolConfig {
        ToolConfig {
            id: id.to_string(),
            params: None,
            name_override: None,
            params_name_overrides: None,
            description_override: None,
            behavior_version: None,
            kind: None,
        }
    }
    #[test]
    fn hashline_tools_registered_in_builder() {
        let builder = ToolRegistryBuilder::new();
        assert!(
            builder
                .tools
                .contains_key("EchoAgentBuildHashline:hashline_read"),
            "hashline_read should be registered"
        );
        assert!(
            builder
                .tools
                .contains_key("EchoAgentBuildHashline:hashline_edit"),
            "hashline_edit should be registered"
        );
        assert!(
            builder
                .tools
                .contains_key("EchoAgentBuildHashline:hashline_grep"),
            "hashline_grep should be registered"
        );
    }
    #[tokio::test]
    async fn hashline_config_finalizes_successfully() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                hashline_tool_config("EchoAgentBuildHashline:hashline_read"),
                hashline_tool_config("EchoAgentBuildHashline:hashline_edit"),
                hashline_tool_config("EchoAgentBuildHashline:hashline_grep"),
            ],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("all-hashline config should finalize");
        let defs = toolset.tool_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains(&"hashline_read"), "defs: {names:?}");
        assert!(names.contains(&"hashline_edit"), "defs: {names:?}");
        assert!(names.contains(&"hashline_grep"), "defs: {names:?}");
    }
    /// Mutual exclusion between standard and hashline toolsets is enforced
    /// by the client config layer (which sends a coherent toolset config).
    /// The registry accepts both toolsets independently.
    #[tokio::test]
    async fn standard_and_hashline_each_finalize_independently() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let standard_config = ToolServerConfig {
            tools: vec![
                hashline_tool_config("EchoAgentBuild:read_file"),
                hashline_tool_config("EchoAgentBuild:search_replace"),
                hashline_tool_config("EchoAgentBuild:grep"),
            ],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        builder
            .finalize(standard_config, ctx)
            .expect("standard config should finalize");
        let builder2 = ToolRegistryBuilder::new();
        let hashline_config = ToolServerConfig {
            tools: vec![
                hashline_tool_config("EchoAgentBuildHashline:hashline_read"),
                hashline_tool_config("EchoAgentBuildHashline:hashline_edit"),
                hashline_tool_config("EchoAgentBuildHashline:hashline_grep"),
            ],
            behavior_preset: None,
        };
        let ctx2 = test_session_context(&tmp);
        builder2
            .finalize(hashline_config, ctx2)
            .expect("hashline config should finalize");
    }
    #[tokio::test]
    async fn hashline_tool_kinds_are_correct() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                hashline_tool_config("EchoAgentBuildHashline:hashline_read"),
                hashline_tool_config("EchoAgentBuildHashline:hashline_edit"),
                hashline_tool_config("EchoAgentBuildHashline:hashline_grep"),
            ],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = builder.finalize(config, ctx).unwrap();
        let defs = toolset.tool_definitions();
        assert_eq!(defs.len(), 3);
        let mut names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["hashline_edit", "hashline_grep", "hashline_read"]
        );
    }
    #[test]
    fn mixed_standard_and_hashline_rejected() {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                hashline_tool_config("EchoAgentBuild:read_file"),
                hashline_tool_config("EchoAgentBuildHashline:hashline_edit"),
                hashline_tool_config("EchoAgentBuild:grep"),
            ],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert!(
            errors.iter().any(|e| e.message.contains("mixed")),
            "mixed file-toolset should be rejected: {errors:?}"
        );
    }
    /// Empty-struct tool inputs produce schemas with explicit `properties: {}` and `required: []`
    #[tokio::test]
    async fn empty_struct_schema_has_properties_and_required() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig::for_tool::<echo_agent_build::EnterPlanModeTool>(),
                ToolConfig::for_tool::<echo_agent_build::ExitPlanModeTool>(),
            ],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("finalize should succeed with plan mode tools");
        let defs = toolset.tool_definitions();
        for tool_name in &["enter_plan_mode", "exit_plan_mode"] {
            let def = defs
                .iter()
                .find(|d| d.function.name == *tool_name)
                .unwrap_or_else(|| panic!("{tool_name} definition not found"));
            let params = &def.function.parameters;
            assert_eq!(
                params.get("properties"),
                Some(&serde_json::json!({})),
                "{tool_name}: schema must have `properties: {{}}`",
            );
            assert_eq!(
                params.get("required"),
                Some(&serde_json::json!([])),
                "{tool_name}: schema must have `required: []`",
            );
        }
    }
    /// End-to-end: construct a hashline ToolServerConfig with custom
    /// params + shared utilities, finalize, and verify the resulting
    /// toolset contains hashline trio + utilities, not standard file tools.
    #[tokio::test]
    async fn hashline_toolset_e2e_with_utilities() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig {
                    id: "EchoAgentBuildHashline:hashline_read".to_owned(),
                    params: Some(
                        serde_json::json!({"scheme": "chunk", "hash_len": 2, "chunk_size": 16})
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig::for_tool::<echo_agent_build_hashline::HashlineEditTool>(),
                ToolConfig::for_tool::<echo_agent_build_hashline::HashlineGrepTool>(),
                ToolConfig::for_tool::<echo_agent_build::ListDirTool>(),
            ],
            behavior_preset: None,
        };
        let errors = builder.validate_config(&config);
        assert!(
            errors.is_empty(),
            "hashline toolset should validate: {errors:?}"
        );
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize(config, ctx)
            .expect("hashline toolset should finalize");
        let defs = toolset.tool_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains(&"hashline_read"), "defs: {names:?}");
        assert!(names.contains(&"hashline_edit"), "defs: {names:?}");
        assert!(names.contains(&"hashline_grep"), "defs: {names:?}");
        assert!(!names.contains(&"read_file"), "defs: {names:?}");
        assert!(!names.contains(&"search_replace"), "defs: {names:?}");
        assert!(names.contains(&"list_dir"), "defs: {names:?}");
    }
    fn bash_config_with_background() -> ToolConfig {
        ToolConfig {
            id: "EchoAgentBuild:run_terminal_cmd".to_owned(),
            params: Some(
                serde_json::json!({ "enabled_background": true })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            name_override: None,
            params_name_overrides: None,
            description_override: None,
            behavior_version: None,
            kind: None,
        }
    }
    async fn echo_agent_build_bridge(tmp: &TempDir) -> crate::bridge::ToolBridge {
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig::for_tool::<echo_agent_build::ListDirTool>(),
                ToolConfig::for_tool::<echo_agent_build::ReadFileTool>(),
                ToolConfig::for_tool::<echo_agent_build::SearchReplaceTool>(),
                bash_config_with_background(),
                ToolConfig::for_tool::<echo_agent_build::TaskOutputTool>(),
                ToolConfig::for_tool::<echo_agent_build::KillTaskTool>(),
                ToolConfig::for_tool::<echo_agent_build::GrepTool>(),
                ToolConfig::for_tool::<echo_agent_build::TodoWriteTool>(),
            ],
            behavior_preset: None,
        };
        crate::bridge::ToolBridge::finalize_builder(builder, config, test_session_context(tmp))
            .await
            .expect("finalize")
    }
    /// list_dir through the hub dispatch path returns valid output.
    #[tokio::test]
    async fn hub_dispatch_list_dir() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("hello.txt"), "world").unwrap();
        let bridge = echo_agent_build_bridge(&tmp).await;
        let result = bridge
            .call(
                "list_dir",
                serde_json::json!({ "target_directory": tmp.path().to_str().unwrap() }),
                "test-call-id",
            )
            .await
            .expect("call should succeed");
        assert!(
            result.prompt_text.contains("hello.txt"),
            "output should contain test file, got: {}",
            result.prompt_text
        );
    }
    /// Parity: list_dir through hub dispatch produces the same prompt_text
    /// as the legacy path (FinalizedToolset::call).
    #[tokio::test]
    async fn hub_dispatch_parity_list_dir() {
        let tmp = TempDir::new().unwrap();
        let test_dir = tmp.path().join("testdir");
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(test_dir.join("parity.txt"), "test").unwrap();
        let args = serde_json::json!({ "target_directory": test_dir.to_str().unwrap() });
        let hub_bridge = echo_agent_build_bridge(&tmp).await;
        let hub_result = hub_bridge
            .call("list_dir", args.clone(), "hub-call")
            .await
            .expect("hub call");
        let legacy_tmp = TempDir::new().unwrap();
        let legacy_test_dir = legacy_tmp.path().join("testdir");
        std::fs::create_dir_all(&legacy_test_dir).unwrap();
        std::fs::write(legacy_test_dir.join("parity.txt"), "test").unwrap();
        let legacy_args =
            serde_json::json!({ "target_directory": legacy_test_dir.to_str().unwrap() });
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig::for_tool::<echo_agent_build::ListDirTool>()],
            behavior_preset: None,
        };
        let legacy_toolset = Arc::new(
            builder
                .finalize(config, test_session_context(&legacy_tmp))
                .expect("finalize"),
        );
        let legacy_result = legacy_toolset
            .call("list_dir", legacy_args, "legacy-call", None)
            .await
            .expect("legacy call");
        assert!(
            hub_result.prompt_text.contains("parity.txt"),
            "hub output should contain parity.txt"
        );
        assert!(
            legacy_result.prompt_text.contains("parity.txt"),
            "legacy output should contain parity.txt"
        );
    }
    /// search_replace (a write tool) works through hub dispatch.
    #[tokio::test]
    async fn hub_dispatch_search_replace() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("editable.txt");
        std::fs::write(&file, "hello world").unwrap();
        let bridge = echo_agent_build_bridge(&tmp).await;
        bridge
            .call(
                "read_file",
                serde_json::json!({ "target_file": file.to_str().unwrap() }),
                "read-call",
            )
            .await
            .expect("read_file");
        let result = bridge
            .call(
                "search_replace",
                serde_json::json!({
                    "file_path": file.to_str().unwrap(),
                    "old_string": "hello",
                    "new_string": "goodbye"
                }),
                "edit-call",
            )
            .await
            .expect("search_replace should succeed");
        assert!(
            !result.prompt_text.contains("error"),
            "search_replace should not error, got: {}",
            result.prompt_text
        );
        let contents = std::fs::read_to_string(&file).unwrap();
        assert_eq!(contents, "goodbye world");
    }
    /// bash (run_terminal_cmd) works through hub dispatch.
    #[tokio::test]
    async fn hub_dispatch_bash() {
        let tmp = TempDir::new().unwrap();
        let bridge = echo_agent_build_bridge(&tmp).await;
        let result = bridge
            .call(
                "run_terminal_cmd",
                serde_json::json!({
                    "command": "echo hub_dispatch_test_sentinel",
                    "description": "test"
                }),
                "bash-call",
            )
            .await
            .expect("bash should succeed");
        assert!(
            result.prompt_text.contains("hub_dispatch_test_sentinel"),
            "bash output should contain sentinel, got: {}",
            result.prompt_text
        );
    }
    /// Invalid arguments through hub dispatch produce an error (not a panic).
    #[tokio::test]
    async fn hub_dispatch_invalid_args() {
        let tmp = TempDir::new().unwrap();
        let bridge = echo_agent_build_bridge(&tmp).await;
        let result = bridge.call("grep", serde_json::json!({}), "bad-call").await;
        assert!(
            result.is_err(),
            "call with missing required args should return an error"
        );
    }
    /// `finalize()` must seed a `SkillManager` so the inline flush in
    /// `call()` and `SkillDiscoveryReminder` work for hosts that don't
    /// use `ToolBridge`.
    #[tokio::test]
    async fn test_finalize_seeds_skill_manager() {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig::for_tool::<echo_agent_build::ListDirTool>()],
            behavior_preset: None,
        };
        let toolset = builder
            .finalize(config, test_session_context(&tmp))
            .expect("finalize");
        let res = toolset.resources.lock().await;
        let mgr = res
            .get::<crate::types::skill_discovery_tracker::SkillManager>()
            .expect("finalize should seed SkillManager");
        assert!(
            mgr.cwd.is_some(),
            "SkillManager.cwd must be set for discovery to work"
        );
    }
    /// Startup skills passed via `SessionContext.skills` must survive a
    /// dynamic discovery. Before the fix, `SkillManager` was seeded with
    /// `startup_skills: vec![]`, so `take_pending()` would compute
    /// `dedup_by_canonical_path(discovered, [])` and overwrite
    /// `AvailableSkills` with only the new discoveries, dropping boot
    /// skills.
    #[tokio::test]
    async fn test_startup_skills_survive_dynamic_discovery() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("probe.txt"), "hello\n").unwrap();
        let boot_skill = crate::implementations::skills::types::SkillInfo {
            name: "boot-skill".to_string(),
            description: "Discovered at finalize time".to_string(),
            path: "/boot/SKILL.md".to_string(),
            ..Default::default()
        };
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![
                ToolConfig::for_tool::<echo_agent_build::ListDirTool>(),
                ToolConfig::for_tool::<echo_agent_build::ReadFileTool>(),
            ],
            behavior_preset: None,
        };
        let mut ctx = test_session_context(&tmp);
        ctx.skills = vec![boot_skill.clone()];
        let toolset = builder.finalize(config, ctx).expect("finalize");
        {
            let res = toolset.resources.lock().await;
            let skills = res
                .get::<crate::types::resources::AvailableSkills>()
                .unwrap();
            assert_eq!(skills.0.len(), 1);
            assert_eq!(skills.0[0].name, "boot-skill");
        }
        {
            let mut res = toolset.resources.lock().await;
            let mgr = res
                .get_mut::<crate::types::skill_discovery_tracker::SkillManager>()
                .expect("finalize should have seeded SkillManager");
            mgr.add_discovered(vec![crate::implementations::skills::types::SkillInfo {
                name: "dynamic-skill".to_string(),
                description: "Found mid-session".to_string(),
                path: "/dynamic/SKILL.md".to_string(),
                ..Default::default()
            }]);
        }
        {
            let mut res = toolset.resources.lock().await;
            let mgr = res
                .get_mut::<crate::types::skill_discovery_tracker::SkillManager>()
                .unwrap();
            if let Some((runtime_skills, _effects)) = mgr.take_pending() {
                res.insert(crate::types::resources::AvailableSkills(runtime_skills));
            }
        }
        {
            let res = toolset.resources.lock().await;
            let skills = res
                .get::<crate::types::resources::AvailableSkills>()
                .expect("AvailableSkills should exist after flush");
            let names: Vec<&str> = skills.0.iter().map(|s| s.name.as_str()).collect();
            assert!(
                names.contains(&"boot-skill"),
                "boot skill must survive dynamic discovery, got: {:?}",
                names
            );
            assert!(
                names.contains(&"dynamic-skill"),
                "dynamic skill must be present after flush, got: {:?}",
                names
            );
            assert_eq!(skills.0.len(), 2, "should have exactly 2 skills");
        }
    }
    /// generate_schema strips the boilerplate root `title` (struct name) and
    /// root `description` (struct doc "Input for the <canonical> tool") so the
    /// canonical name can't leak via parameters.description after randomization
    /// renames the tool. $schema and per-property descriptions are retained.
    #[test]
    fn generate_schema_strips_root_title_and_description() {
        let schema =
            generate_schema::<crate::implementations::echo_agent_build::bash::BashToolInput>();
        assert!(
            schema.get("title").is_none(),
            "root title (struct name) must be stripped: {schema}"
        );
        assert!(
            schema.get("description").is_none(),
            "root description (leaks canonical tool name) must be stripped: {schema}"
        );
        assert!(schema.get("$schema").is_some(), "$schema must be retained");
        assert!(
            schema["properties"]
                .as_object()
                .is_some_and(|p| !p.is_empty()),
            "per-property schema must be retained: {schema}"
        );
    }
    #[test]
    fn generate_schema_memoizes_per_type() {
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct SchemaMemoProbe {
            field: String,
        }
        let key = std::any::TypeId::of::<SchemaMemoProbe>();
        let first = generate_schema_cached::<SchemaMemoProbe>();
        let second = generate_schema_cached::<SchemaMemoProbe>();
        assert_eq!(first, second);
        assert_eq!(
            schema_uncached_calls(key),
            1,
            "a type's schema must be generated at most once per process"
        );
    }
    fn toolset_with_viewer_ctx(
        viewer_ctx: Option<echo_agent_tool_runtime::WorkspaceViewerContext>,
    ) -> (Arc<FinalizedToolset>, TempDir) {
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:read_file".to_string(),
                params: None,
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = builder
            .finalize_with_trunc_config(
                config,
                ctx,
                crate::types::context::TruncationConfig::default(),
                viewer_ctx,
            )
            .expect("finalize succeeds");
        (Arc::new(toolset), tmp)
    }
    #[tokio::test]
    async fn prepare_dispatch_stamps_workspace_viewer_ctx_when_present() {
        let (toolset, _tmp) =
            toolset_with_viewer_ctx(Some(echo_agent_tool_runtime::WorkspaceViewerContext {
                stream_tool_progress: true,
            }));
        let parts = toolset
            .prepare_dispatch(
                "read_file",
                serde_json::json!({"target_file": "noop"}),
                "test-call",
                None,
                None,
            )
            .expect("prepare_dispatch succeeds");
        let wvc = parts
            .ctx
            .extensions
            .get::<echo_agent_tool_runtime::WorkspaceViewerContext>()
            .expect("WorkspaceViewerContext must be stamped on the ctx");
        assert!(wvc.stream_tool_progress);
    }
    #[tokio::test]
    async fn prepare_dispatch_omits_workspace_viewer_ctx_when_none() {
        let (toolset, _tmp) = toolset_with_viewer_ctx(None);
        let parts = toolset
            .prepare_dispatch(
                "read_file",
                serde_json::json!({"target_file": "noop"}),
                "test-call",
                None,
                None,
            )
            .expect("prepare_dispatch succeeds");
        assert!(
            parts
                .ctx
                .extensions
                .get::<echo_agent_tool_runtime::WorkspaceViewerContext>()
                .is_none(),
            "no extension must be stamped when workspace_viewer_ctx is None",
        );
    }
    /// End-to-end: `FinalizedToolset` with `stream_tool_progress: true`
    /// produces `bash_output_chunk` Progress frames via `call_streaming`.
    #[tokio::test]
    async fn bash_streaming_progress_emitted_via_finalized_toolset_when_gate_on() {
        use futures::StreamExt;
        let tmp = TempDir::new().unwrap();
        let builder = ToolRegistryBuilder::new();
        let config = ToolServerConfig {
            tools: vec![ToolConfig {
                id: "EchoAgentBuild:run_terminal_cmd".to_string(),
                params: Some(
                    serde_json::json!({"enabled_background": false})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            }],
            behavior_preset: None,
        };
        let ctx = test_session_context(&tmp);
        let toolset = Arc::new(
            builder
                .finalize_with_trunc_config(
                    config,
                    ctx,
                    crate::types::context::TruncationConfig::default(),
                    Some(echo_agent_tool_runtime::WorkspaceViewerContext {
                        stream_tool_progress: true,
                    }),
                )
                .expect("finalize"),
        );
        let mut stream = toolset.call_streaming(
            "run_terminal_cmd",
            serde_json::json!({
                "command": "for i in 1 2 3; do echo $i; sleep 0.1; done",
                "description": "stream progress test"
            }),
            "test-call",
            None,
        );
        let mut progress_count = 0usize;
        let mut got_terminal = false;
        while let Some(item) = stream.next().await {
            match item {
                echo_agent_tool_runtime::ToolStreamItem::Progress(_) => progress_count += 1,
                echo_agent_tool_runtime::ToolStreamItem::Terminal(r) => {
                    r.expect("terminal must succeed");
                    got_terminal = true;
                }
            }
        }
        assert!(
            progress_count >= 1,
            "gate ON must produce ≥ 1 Progress frame via workspace dispatch, got {progress_count}",
        );
        assert!(got_terminal, "must observe terminal");
    }
}
